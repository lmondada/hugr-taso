use hugr::hugr::circuit_hugr::circuit_hash;
use hugr::hugr::{CircuitHugr, HugrView};
use hugr::ops::OpType;
use portmatching::matcher::many_patterns::PatternMatch;
use portmatching::{Matcher, Pattern};
use priority_queue::PriorityQueue;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Instant;
use std::{fs, iter, thread};

use serde::{Deserialize, Serialize};

use crate::compile::{load_matcher_or_compile, CompiledTrie};

mod qtz_circuit;

#[derive(Clone, Serialize, Deserialize)]
pub struct RepCircSet {
    pub(crate) rep_circ: CircuitHugr,
    pub(crate) others: Vec<CircuitHugr>,
    uuid: String,
}

impl RepCircSet {
    pub fn new(rep_circ: CircuitHugr, others: Vec<CircuitHugr>) -> Self {
        let uuid = uuid::Uuid::new_v4().to_string();

        // let path = format!("debug/{uuid}");
        // fs::create_dir_all(&path).unwrap();
        // fs::write(format!("{path}/rep.gv"), rep_circ.hugr().dot_string()).unwrap();
        // for (i, other) in others.iter().enumerate() {
        //     fs::write(format!("{path}/other_{i}.gv"), other.hugr().dot_string()).unwrap();
        // }

        Self {
            rep_circ,
            others,
            uuid,
        }
    }
    pub fn len(&self) -> usize {
        self.others.len() + 1
    }
}

// TODO refactor so both implementations share more code

/// Load a set of ECCs from a file
pub fn load_eccs(ecc_name: &str) -> Vec<RepCircSet> {
    let path = format!("data/{ecc_name}.json");
    let all_circs = qtz_circuit::load_ecc_set(&path);

    all_circs
        .into_values()
        .map(|all| {
            // TODO is the rep circ always the first??
            let mut all = all.into_iter().map(CircuitHugr::new).collect::<Vec<_>>();
            let rep_circ = all.remove(0);

            RepCircSet::new(rep_circ, all)
        })
        .collect()
}

impl RepCircSet {
    /// Which patterns can be transformed into which
    pub fn rewrite_rules(&self) -> HashMap<usize, Vec<usize>> {
        // Rules all -> rep
        let mut rules: HashMap<_, _> = (1..=self.others.len()).zip(iter::repeat(vec![0])).collect();
        // Rules rep -> all
        rules.insert(0, (1..=self.others.len()).collect());
        rules
    }

    pub fn circuits(&self) -> Vec<&CircuitHugr> {
        let mut circuits = Vec::with_capacity(self.others.len() + 1);
        circuits.push(&self.rep_circ);
        circuits.extend(self.others.iter());
        circuits
    }

    fn circuits_mut(&mut self) -> Vec<&mut CircuitHugr> {
        let mut patterns = Vec::with_capacity(self.others.len() + 1);
        patterns.push(&mut self.rep_circ);
        patterns.extend(self.others.iter_mut());
        patterns
    }

    pub(crate) fn remove_blanks(&mut self) {
        let mut blanks: HashSet<_> = (0..self.rep_circ.input_ports().count()).collect();
        for circ in self.circuits() {
            let new_blanks = circ.blank_wires();
            blanks.retain(|b| new_blanks.contains(b));
        }
        for circ in self.circuits_mut().into_iter() {
            circ.remove_wires(&blanks);
        }
    }
}

pub fn taso_mpsc<C>(
    circ: CircuitHugr,
    ecc_sets: &str,
    gamma: f64,
    cost: C,
    timeout: Option<u64>,
    n_threads: usize,
) -> CircuitHugr
where
    C: Fn(&CircuitHugr) -> usize + Send + Sync,
{
    let mut matcher = load_matcher_or_compile(ecc_sets).unwrap();
    // matcher.filter_rewrites(2);

    let start_time = Instant::now();

    println!("Spinning up {n_threads} threads");

    // channel for sending circuits from threads back to main
    let (t_main, r_main) = mpsc::channel();

    let _rev_cost = |x: &CircuitHugr| usize::MAX - cost(x);

    let mut pq = PriorityQueue::new();
    let mut cbest = circ.clone();
    let cin_cost = _rev_cost(&circ);
    let mut cbest_cost = cin_cost;
    let chash = circuit_hash(&circ);
    println!("new best of size {}", cost(&cbest));

    // map of seen circuits, if the circuit has been popped from the queue,
    // holds None
    let mut dseen: HashMap<usize, Option<CircuitHugr>> = HashMap::from_iter([(chash, Some(circ))]);
    pq.push(chash, cin_cost);

    // each thread scans for rewrites using all the patterns and
    // sends rewritten circuits back to main
    let (joins, threads_tx): (Vec<_>, Vec<_>) = (0..n_threads)
        .map(|thread_id| spawn_pattern_matching_thread(thread_id, t_main.clone(), matcher.clone()))
        .unzip();

    let mut cycle_inds = (0..n_threads).cycle();

    let mut thread_status = vec![ChannelStatus::Empty; n_threads];
    let mut circ_cnt = 0;
    loop {
        while let Some((hc, priority)) = pq.pop() {
            let seen_circ = dseen
                .insert(hc, None)
                .flatten()
                .expect("seen circ missing.");

            if priority > cbest_cost {
                cbest = seen_circ.clone();
                cbest_cost = priority;
                println!("new best of size {}", cost(&cbest));
            }
            // send the popped circuit to one thread
            let next_ind = cycle_inds.next().expect("cycle never ends");
            let tx = &threads_tx[next_ind];
            tx.send(ChannelMsg::Item(seen_circ)).unwrap();
            thread_status[next_ind] = ChannelStatus::NonEmpty;
        }

        while let Ok(received) = r_main.try_recv() {
            let newc = match received {
                ChannelMsg::Item(newc) => newc,
                ChannelMsg::Empty(thread_id) => {
                    thread_status[thread_id] = ChannelStatus::Empty;
                    continue;
                }
                ChannelMsg::Panic => panic!("A thread panicked"),
            };
            // println!("Main got one");
            circ_cnt += 1;
            let newchash = circuit_hash(&newc);
            if dseen.contains_key(&newchash) {
                continue;
            }
            let newcost = _rev_cost(&newc);
            if gamma * (newcost as f64) > (cbest_cost as f64) {
                pq.push(newchash, newcost);
                dseen.insert(newchash, Some(newc));
            }
        }

        if pq.is_empty() && thread_status.iter().all(|&s| s == ChannelStatus::Empty) {
            break;
        }
        if let Some(timeout) = timeout {
            if start_time.elapsed().as_secs() > timeout {
                println!("Timeout");
                break;
            }
        }
    }

    println!("Joining");
    for (join, tx) in joins.into_iter().zip(threads_tx.into_iter()) {
        // tell all the threads we're done and join the threads
        tx.send(ChannelMsg::Empty(0)).unwrap();
        join.join().unwrap();
    }
    println!("Tried {circ_cnt} circuits");
    println!("END RESULT: {}", cost(&cbest));
    cbest
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChannelStatus {
    NonEmpty,
    Empty,
}

enum ChannelMsg<M> {
    Item(M),
    Empty(usize),
    Panic,
}

fn spawn_pattern_matching_thread(
    thread_id: usize,
    tx_main: Sender<ChannelMsg<CircuitHugr>>,
    matcher: CompiledTrie,
) -> (JoinHandle<()>, Sender<ChannelMsg<CircuitHugr>>) {
    let CompiledTrie {
        matcher,
        rewrite_rules,
        all_circs,
        pattern2circ,
    } = matcher;
    // channel for sending circuits to each thread
    let (tx_thread, rx) = mpsc::channel();

    let jn = thread::spawn(move || {
        let mut await_main = false;
        let recv = |wait| {
            if wait {
                rx.recv().ok()
            } else {
                rx.try_recv().ok()
            }
        };
        loop {
            if let Some(received) = recv(await_main) {
                let sent_hugr: CircuitHugr = match received {
                    ChannelMsg::Item(c) => c,
                    // We've been terminated
                    ChannelMsg::Empty(_) => break,
                    ChannelMsg::Panic => panic!("Was told to do so"),
                };
                let (g, w) = sent_hugr.hugr().as_weighted_graph();
                let mut convex_check = sent_hugr.convex_checker(sent_hugr.input_node());
                for &PatternMatch { id, root } in &matcher.find_weighted_matches(g, &w) {
                    let pattern = &all_circs[pattern2circ[&id]];
                    let pattern_root = matcher.get_pattern(id).root();
                    let pattern_root_weight = pattern.hugr().get_optype(pattern_root.into());
                    // hack: root weight not checked atm, so check here
                    if let OpType::LeafOp(pattern_root_weight) = pattern_root_weight {
                        if Some(pattern_root_weight) != w[root].as_ref() {
                            continue;
                        }
                    }
                    for &new_id in rewrite_rules[&id].iter() {
                        let Some(replacement) = pattern
                                .simple_replacement(
                                    all_circs[new_id].clone(),
                                    sent_hugr.hugr(),
                                    pattern_root.into(),
                                    root.into(),
                                ) else { continue };
                        if !replacement.is_convex(&mut convex_check) {
                            continue;
                        }
                        let mut newc = sent_hugr.clone();
                        newc.hugr_mut()
                            .apply_simple_replacement(replacement)
                            .expect("rewrite failure");
                        if newc.hugr().validate().is_err() {
                            let dir = format!("transforms/");
                            fs::create_dir_all(&dir).unwrap();
                            fs::write(format!("{dir}/bef.gv"), sent_hugr.hugr().dot_string())
                                .unwrap();
                            fs::write(format!("{dir}/aft.gv"), newc.hugr().dot_string()).unwrap();
                            fs::write(
                                format!("{dir}/pattern.gv"),
                                all_circs[pattern2circ[&id]].hugr().dot_string(),
                            )
                            .unwrap();
                            fs::write(
                                format!("{dir}/repl.gv"),
                                all_circs[new_id].hugr().dot_string(),
                            )
                            .unwrap();
                            tx_main.send(ChannelMsg::Panic).unwrap();
                            panic!("invalid replacement");
                        }
                        tx_main.send(ChannelMsg::Item(newc)).unwrap();
                    }
                }
            } else {
                if await_main {
                    // `recv` failed, an error must have occured
                    break;
                }
                // We are out of work, let main know
                tx_main.send(ChannelMsg::Empty(thread_id)).unwrap();
                await_main = true;
            }
        }
    });

    (jn, tx_thread)
}

// pub fn taso<C>(
//     circ: Circuit,
//     repset: Vec<RepCircSet>,
//     gamma: f64,
//     cost: C,
//     _timeout: i64,
// ) -> Circuit
// where
//     C: Fn(&Circuit) -> usize + Send + Sync,
// {
//     // TODO timeout

//     let _rev_cost = |x: &Circuit| usize::MAX - cost(x);

//     let mut pq = PriorityQueue::new();
//     let mut cbest = circ.clone();
//     let cin_cost = _rev_cost(&circ);
//     let mut cbest_cost = cin_cost;
//     let chash = circuit_hash(&circ);
//     // map of seen circuits, if the circuit has been popped from the queue,
//     // holds None
//     let mut dseen: HashMap<usize, Option<Circuit>> = HashMap::from_iter([(chash, Some(circ))]);
//     pq.push(chash, cin_cost);

//     while let Some((hc, priority)) = pq.pop() {
//         // remove circuit from map and replace with None
//         let seen_circ = dseen
//             .insert(hc, None)
//             .flatten()
//             .expect("seen circ missing.");
//         if priority > cbest_cost {
//             cbest = seen_circ.clone();
//             cbest_cost = priority;
//         }
//         // par_iter implementation

//         // let pq = Mutex::new(&mut pq);
//         // tra_patterns.par_iter().for_each(|(pattern, c2)| {
//         //     pattern_rewriter(pattern.clone(), &hc.0, |_| (c2.clone(), 0.0)).for_each(|rewrite| {
//         //         let mut newc = hc.0.clone();
//         //         newc.apply_rewrite(rewrite).expect("rewrite failure");
//         //         let newchash = circuit_hash(&newc);
//         //         let mut dseen = dseen.lock().unwrap();
//         //         if dseen.contains(&newchash) {
//         //             return;
//         //         }
//         //         let newhc = HashCirc(newc);
//         //         let newcost = _rev_cost(&newhc);
//         //         if gamma * (newcost as f64) > (cbest_cost as f64) {
//         //             let mut pq = pq.lock().unwrap();
//         //             pq.push(newhc, newcost);
//         //             dseen.insert(newchash);
//         //         }
//         //     });
//         // })

//         // non-parallel implementation:

//         for rp in repset.iter() {
//             for rewrite in rp.to_rewrites(&seen_circ) {
//                 // TODO here is where a optimal data-sharing copy would be handy
//                 let mut newc = seen_circ.clone();
//                 newc.apply_rewrite(rewrite).expect("rewrite failure");
//                 let newchash = circuit_hash(&newc);
//                 if dseen.contains_key(&newchash) {
//                     continue;
//                 }
//                 let newcost = _rev_cost(&newc);
//                 if gamma * (newcost as f64) > (cbest_cost as f64) {
//                     pq.push(newchash, newcost);
//                     dseen.insert(newchash, Some(newc));
//                 }
//             }
//         }
//     }
//     cbest
// }

#[cfg(test)]
mod tests;

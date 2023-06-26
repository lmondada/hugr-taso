use hugr::hugr::circuit_hugr::circuit_hash;
use hugr::hugr::{CircuitHugr, HugrView};
use hugr::ops::OpType;
use portmatching::matcher::many_patterns::PatternMatch;
use portmatching::{Matcher, Pattern};
use priority_queue::PriorityQueue;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::mem::swap;
use std::sync::mpsc::{self, SyncSender};
use std::thread::JoinHandle;
use std::time::Instant;
use std::{io, iter, thread};

use serde::{Deserialize, Serialize};

use crate::compile::{load_matcher_or_compile, CompiledTrie};

mod qtz_circuit;

#[derive(Clone, Serialize, Deserialize)]
pub struct RepCircSet {
    // First is representative circuit always
    all_circs: Vec<CircuitHugr>,
}

impl RepCircSet {
    pub fn new(rep_circ: CircuitHugr, others: Vec<CircuitHugr>) -> Self {
        let all_circs = [rep_circ].into_iter().chain(others).collect();
        Self { all_circs }
    }

    pub fn from_circs(mut all_circs: Vec<CircuitHugr>) -> Self {
        assert!(!all_circs.is_empty());
        // The smallest is the rep_circ. Make sure it is first
        let min_ind = (0..all_circs.len())
            .min_by_key(|&i| all_circs[i].node_count())
            .unwrap();
        if min_ind != 0 {
            let (first, min) = {
                let (a, b) = all_circs.split_at_mut(min_ind);
                (&mut a[0], &mut b[0])
            };
            swap(first, min);
        }
        Self { all_circs }
    }

    pub fn len(&self) -> usize {
        self.all_circs.len()
    }

    pub fn rep_circ(&self) -> &CircuitHugr {
        &self.all_circs[0]
    }

    pub fn others(&self) -> &[CircuitHugr] {
        &self.all_circs[1..]
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
            let all = all.into_iter().map(CircuitHugr::new).collect::<Vec<_>>();
            RepCircSet::from_circs(all)
        })
        .collect()
}

impl RepCircSet {
    /// Which patterns can be transformed into which
    pub fn rewrite_rules(&self) -> HashMap<usize, Vec<usize>> {
        // Rules all -> rep
        let mut rules: HashMap<_, _> = (1..self.len()).zip(iter::repeat(vec![0])).collect();
        // Rules rep -> all
        rules.insert(0, (1..self.len()).collect());
        rules
    }

    pub fn circuits(&self) -> &[CircuitHugr] {
        self.all_circs.as_slice()
    }

    /// All the RepCircSets within `this` where blank wires are removed
    pub(crate) fn no_blank_eccs(&self) -> Vec<RepCircSet> {
        // The "blank signature" of each circuit
        let mut blanks2circs = HashMap::new();
        for circ in self.circuits() {
            blanks2circs
                .entry(circ.blank_wires())
                .or_insert_with(Vec::new)
                .push(circ);
        }

        let mut sets = Vec::new();
        for blanks in blanks2circs.keys() {
            let mut circs = Vec::new();
            for (other_blanks, other_circs) in blanks2circs.iter() {
                if other_blanks.clone() & blanks.clone() == *blanks {
                    circs.extend(other_circs.iter().map(|&circ| {
                        let mut circ = circ.clone();
                        circ.remove_wires(&blanks);
                        circ
                    }));
                }
            }
            sets.push(RepCircSet::from_circs(circs));
        }
        sets
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
    matcher.filter_rewrites(2);

    let start_time = Instant::now();

    let file = File::create("best_circs.csv").unwrap();
    let mut log_cbest = csv::Writer::from_writer(file);

    println!("Spinning up {n_threads} threads");

    // channel for sending circuits from threads back to main
    let (t_main, r_main) = mpsc::sync_channel(n_threads * 100);

    let _rev_cost = |x: &CircuitHugr| usize::MAX - cost(x);

    let mut pq = PriorityQueue::new();
    let mut cbest = circ.clone();
    let cin_cost = _rev_cost(&circ);
    let mut cbest_cost = cin_cost;
    let chash = circuit_hash(&circ);
    log_best(cost(&cbest), &mut log_cbest).unwrap();

    // Hash of seen circuits. Dot not store circuits as this map gets huge
    let mut dseen: HashSet<usize> = HashSet::from_iter([(chash)]);
    // The circuits being currently processed (this should not get big)
    let mut circs_in_pq = HashMap::new();

    pq.push(chash, cin_cost);
    circs_in_pq.insert(chash, circ);

    // each thread scans for rewrites using all the patterns and
    // sends rewritten circuits back to main
    let (joins, threads_tx): (Vec<_>, Vec<_>) = (0..n_threads)
        .map(|thread_id| spawn_pattern_matching_thread(thread_id, t_main.clone(), matcher.clone()))
        .unzip();

    let mut cycle_inds = (0..n_threads).cycle();

    let mut thread_status = vec![ChannelStatus::Empty; n_threads];
    let mut circ_cnt = 0;
    let mut circ_q = Vec::new();
    loop {
        while let Ok(received) = r_main.try_recv() {
            match received {
                ChannelMsg::Item(newc) => circ_q.push(newc),
                ChannelMsg::Empty(thread_id) => thread_status[thread_id] = ChannelStatus::Empty,
                ChannelMsg::Panic => panic!("A thread panicked"),
            };
        }

        while let Some((hc, priority)) = pq.pop() {
            let seen_circ = circs_in_pq.remove(&hc).unwrap();

            if priority > cbest_cost {
                cbest = seen_circ.clone();
                cbest_cost = priority;
                log_best(cost(&cbest), &mut log_cbest).unwrap();
            }
            // send the popped circuit to first available thread, or block
            let mut send = |circ: CircuitHugr| {
                let mut i = 0;
                loop {
                    let next_ind = cycle_inds.next().expect("cycle never ends");
                    let tx = &threads_tx[next_ind];
                    if tx.try_send(ChannelMsg::Item(circ.clone())).is_ok() {
                        return next_ind;
                    } else if i + 1 == n_threads {
                        tx.send(ChannelMsg::Item(circ)).unwrap();
                        return next_ind;
                    }
                    i += 1;
                }
            };
            let next_ind = send(seen_circ);
            thread_status[next_ind] = ChannelStatus::NonEmpty;
        }

        let queue_size = circ_q.len();
        // We compute the hashes in the threads because it's expensive
        for (newchash, newc) in circ_q.drain(..) {
            circ_cnt += 1;
            if circ_cnt % 1000 == 0 {
                println!("{circ_cnt} circuits...");
                println!("from thread: {queue_size} new circuits");
                println!("Total queue size: {} circuits", pq.len());
                println!("dseen size: {} circuits", dseen.len());
            }
            if dseen.contains(&newchash) {
                continue;
            }
            let newcost = _rev_cost(&newc);
            if gamma * (newcost as f64) > (cbest_cost as f64) {
                pq.push(newchash, newcost);
                dseen.insert(newchash);
                circs_in_pq.insert(newchash, newc);
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

    println!("Tried {circ_cnt} circuits");
    println!("Joining");
    for (join, tx) in joins.into_iter().zip(threads_tx.into_iter()) {
        // tell all the threads we're done and join the threads
        tx.send(ChannelMsg::Empty(0)).unwrap();
        join.join().unwrap();
    }
    println!("END RESULT: {}", cost(&cbest));
    fs::write("final_best_circ.gv", cbest.hugr().dot_string()).unwrap();
    fs::write(
        "final_best_circ.json",
        serde_json::to_vec(cbest.hugr()).unwrap(),
    )
    .unwrap();
    cbest
}

#[derive(serde::Serialize, Debug)]
struct BestCircSer {
    circ_len: usize,
    time: String,
}

impl BestCircSer {
    fn new(circ_len: usize) -> Self {
        let time = chrono::Local::now().to_rfc3339();
        Self { circ_len, time }
    }
}

fn log_best(cbest: usize, wtr: &mut csv::Writer<File>) -> io::Result<()> {
    println!("new best of size {}", cbest);
    wtr.serialize(BestCircSer::new(cbest)).unwrap();
    wtr.flush()
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
    tx_main: SyncSender<ChannelMsg<(usize, CircuitHugr)>>,
    matcher: CompiledTrie,
) -> (JoinHandle<()>, SyncSender<ChannelMsg<CircuitHugr>>) {
    let CompiledTrie {
        matcher,
        rewrite_rules,
        all_circs,
        pattern2circ,
    } = matcher;
    // channel for sending circuits to each thread
    let (tx_thread, rx) = mpsc::sync_channel(1000);

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
                await_main = false;
                let sent_hugr: CircuitHugr = match received {
                    ChannelMsg::Item(c) => c,
                    // We've been terminated
                    ChannelMsg::Empty(_) => break,
                    ChannelMsg::Panic => panic!("Was told to do so"),
                };
                let (g, w) = sent_hugr.hugr().as_weighted_graph();
                let no_pred_nodes = sent_hugr
                    .hugr()
                    .children(sent_hugr.hugr().root())
                    .filter(|&n| sent_hugr.hugr().num_inputs(n) == 0);
                let mut convex_check = sent_hugr.convex_checker(no_pred_nodes);
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
                            tx_main.send(ChannelMsg::Panic).unwrap();
                            panic!("invalid replacement");
                        }
                        let newchash = circuit_hash(&newc);
                        tx_main.send(ChannelMsg::Item((newchash, newc))).unwrap();
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

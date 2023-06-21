use hugr::hugr::circuit_hugr::circuit_hash;
use hugr::hugr::CircuitHugr;
use hugr::pattern::HugrPattern;
use portmatching::matcher::many_patterns::PatternMatch;
use portmatching::{ManyPatternMatcher, Matcher, Pattern, TrieMatcher};
use priority_queue::PriorityQueue;
use std::collections::HashSet;
use std::sync::mpsc;
use std::{collections::HashMap, sync::Arc};
use std::{iter, thread};

use serde::{Deserialize, Serialize};

mod qtz_circuit;

#[derive(Clone, Serialize, Deserialize)]
pub struct RepCircSet {
    pub rep_circ: CircuitHugr,
    pub others: Vec<CircuitHugr>,
}

impl RepCircSet {
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

            RepCircSet {
                rep_circ,
                others: all,
            }
        })
        .collect()
}

impl RepCircSet {

    /// Which patterns can be transformed into which
    fn rewrite_rules(&self) -> HashMap<usize, Vec<usize>> {
        // Rules all -> rep
        let mut rules: HashMap<_, _> = (1..=self.others.len()).zip(iter::repeat(vec![0])).collect();
        // Rules rep -> all
        rules.insert(0, (1..=self.others.len()).collect());
        rules
    }

    fn circuits(&self) -> Vec<&CircuitHugr> {
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

    fn remove_blanks(&mut self) {
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
    mut repset: Vec<RepCircSet>,
    gamma: f64,
    cost: C,
    _timeout: i64,
    max_threads: usize,
) -> CircuitHugr
where
    C: Fn(&CircuitHugr) -> usize + Send + Sync,
{
    // TODO timeout

    println!("Building matcher trie...");
    let mut matcher = TrieMatcher::new(portmatching::TrieConstruction::Balanced);
    let mut rewrite_rules = HashMap::new();
    let mut all_circs = Vec::new();
    let mut pattern2circ = HashMap::new();
    for set in repset.iter_mut() {
        set.remove_blanks();
        let mut pattern_ids = Vec::with_capacity(set.len());
        let mut circ_ids = Vec::with_capacity(set.len());
        for circ in set.circuits() {
            circ_ids.push(all_circs.len());
            all_circs.push(circ.clone());
            if let Ok(p) = HugrPattern::from_circuit(circ.clone()) {
                pattern_ids.push(Some(matcher.add_pattern(p.clone())));
            } else {
                pattern_ids.push(None);
            }
        }
        rewrite_rules.extend(
            set.rewrite_rules()
                .into_iter()
                .filter_map(|(from, all_tos)| {
                    let all_tos = all_tos
                        .into_iter()
                        .map(|to| circ_ids[to])
                        .collect::<Vec<_>>();
                    pattern_ids[from].map(|from| (from, all_tos))
                }),
        );
        pattern2circ.extend(
            pattern_ids
                .into_iter()
                .zip(circ_ids)
                .filter_map(|(p, c)| p.map(|p| (p, c))),
        );
    }
    // matcher.optimise(10, 10);
    println!("Done");

    let n_threads = std::cmp::min(max_threads, repset.len());

    println!("Spinning up {n_threads} threads");

    let (t_main, r_main) = mpsc::channel();

    let _rev_cost = |x: &CircuitHugr| usize::MAX - cost(x);

    let mut pq = PriorityQueue::new();
    let mut cbest = circ.clone();
    let cin_cost = _rev_cost(&circ);
    let mut cbest_cost = cin_cost;
    let chash = circuit_hash(&circ);
    // map of seen circuits, if the circuit has been popped from the queue,
    // holds None
    let mut dseen: HashMap<usize, Option<CircuitHugr>> = HashMap::from_iter([(chash, Some(circ))]);
    pq.push(chash, cin_cost);

    // each thread scans for rewrites using all the patterns and
    // sends rewritten circuits back to main
    let (joins, thread_ts): (Vec<_>, Vec<_>) = (0..n_threads)
        .map(|i| {
            // channel for sending circuits to each thread
            let (t_this, r_this) = mpsc::channel();
            let tn = t_main.clone();
            let matcher = matcher.clone();
            let rewrite_rules = rewrite_rules.clone();
            let all_circs = all_circs.clone();
            let pattern2circ = pattern2circ.clone();
            let jn = thread::spawn(move || {
                for received in r_this {
                    let sent_hugr: Arc<CircuitHugr> = if let Some(hc) = received {
                        hc
                    } else {
                        // main has signalled no more circuits will be sent
                        return;
                    };
                    let (g, w) = sent_hugr.hugr().as_weighted_graph();
                    println!("thread {i} got one");
                    for &PatternMatch { id, root } in &matcher.find_weighted_matches(g, &w) {
                        let pattern_root = matcher.get_pattern(id).root();
                        for &new_id in rewrite_rules[&id].iter() {
                            let Some(replacement) = all_circs[pattern2circ[&id]]
                                .simple_replacement(
                                    all_circs[new_id].clone(),
                                    sent_hugr.hugr(),
                                    pattern_root.into(),
                                    root.into(),
                                ) else { continue };
                            let mut newc = (*sent_hugr).clone();
                            // fs::write("pre.gv", newc.hugr().dot_string()).unwrap();
                            // fs::write("pre_pattern.gv", all_circs[pattern2circ[&id]].hugr().dot_string()).unwrap();
                            newc.hugr_mut()
                                .apply_simple_replacement(replacement)
                                .expect("rewrite failure");
                            // fs::write("post.gv", newc.hugr().dot_string()).unwrap();
                            // fs::write("post_pattern.gv", all_circs[new_id].hugr().dot_string()).unwrap();
                            tn.send(Some(newc)).unwrap();
                            println!("thread {i} sent one back");
                        }
                    }
                    // no more circuits will be generated, tell main this thread is
                    // done with this circuit
                    tn.send(None).unwrap();
                }
            });

            (jn, t_this)
        })
        .unzip();

    let mut thread_ts_cycle = thread_ts.iter().cycle();

    while let Some((hc, priority)) = pq.pop() {
        let seen_circ = dseen
            .insert(hc, None)
            .flatten()
            .expect("seen circ missing.");
        println!("\npopped one of size {}", &seen_circ.node_count());

        if priority > cbest_cost {
            cbest = seen_circ.clone();
            cbest_cost = priority;
            println!("new best of size {}", cost(&cbest));
        }
        let seen_circ = Arc::new(seen_circ);
        // send the popped circuit to one thread
        let thread_t = thread_ts_cycle.next().expect("cycle never ends");
        thread_t.send(Some(seen_circ.clone())).unwrap();

        let mut done_tracker = 0;
        for received in &r_main {
            let newc = if let Some(newc) = received {
                newc
            } else {
                done_tracker += 1;
                if done_tracker == n_threads {
                    // all threads have said they are done with this circuit
                    break;
                } else {
                    continue;
                }
            };
            println!("Main got one");
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
    }
    println!("Joining");
    for (join, tx) in joins.into_iter().zip(thread_ts.into_iter()) {
        // tell all the threads we're done and join the threads
        tx.send(None).unwrap();
        join.join().unwrap();
    }
    cbest
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

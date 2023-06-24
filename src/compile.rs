use std::{collections::HashMap, fs, io, mem, path::Path, time::Instant};

use hugr::{
    hugr::CircuitHugr,
    pattern::{HugrMatcher, HugrPattern},
};

use portmatching::{ManyPatternMatcher, PatternID};

use crate::{ensure_exists, load_eccs, taso::RepCircSet};

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct CompiledTrie {
    pub(crate) matcher: HugrMatcher,
    pub(crate) rewrite_rules: HashMap<PatternID, Vec<usize>>,
    pub(crate) all_circs: Vec<CircuitHugr>,
    pub(crate) pattern2circ: HashMap<PatternID, usize>,
}

impl CompiledTrie {
    /// Only use a subset of rewrite rules.
    ///
    /// Discard rules that we know will not apply.
    ///
    /// The `temp` parameter is used as a cutoff of how much larger
    /// the new circuit compared to the old one.
    /// This value is absolute: for a fixed gamma,
    /// a larger circuit will tolerate a larger temp (but if you're going
    /// to fix gamma = 1, then there is no point considering temp > 0).
    pub(crate) fn filter_rewrites(&mut self, temp: usize) {
        for (rep_id, others) in self.rewrite_rules.iter_mut() {
            let rep_id = self.pattern2circ[rep_id];
            let size_rep = self.all_circs[rep_id].node_count();
            *others = mem::take(others)
                .into_iter()
                .filter(|&i| {
                    let size_other = self.all_circs[i].node_count();
                    size_other <= size_rep + temp
                })
                .collect();
        }
    }
}

const DATA_DIR: &str = "data/compiled_tries";

pub fn compile_eccs(eccs: &[RepCircSet], name: &str) {
    println!("Building matcher trie...");
    let start_time = Instant::now();
    let mut matcher = HugrMatcher::new(portmatching::TrieConstruction::Balanced);
    let mut rewrite_rules = HashMap::new();
    let mut all_circs = Vec::new();
    let mut all_pattern_ids = Vec::new();
    let mut pattern2circ = HashMap::new();
    for set in eccs.into_iter().flat_map(|set| set.no_blank_eccs()) {
        let mut pattern_ids = Vec::with_capacity(set.len());
        let mut circ_ids = Vec::with_capacity(set.len());
        for circ in set.circuits() {
            let circ_ind = (0..all_circs.len())
                .find(|&i| &all_circs[i] == circ)
                .unwrap_or_else(|| {
                    // Add circuit to matcher
                    let mut circ = circ.clone();
                    let hugr = circ.hugr_mut();

                    // Hack: serialize and deserialize to stabilise Node indices
                    *hugr = serde_json::from_slice(&serde_json::to_vec(hugr).unwrap()).unwrap();

                    let ind = all_circs.len();
                    all_circs.push(circ.clone());
                    if let Ok(p) = HugrPattern::from_circuit(circ) {
                        all_pattern_ids.push(Some(matcher.add_pattern(p.clone())));
                    } else {
                        all_pattern_ids.push(None);
                    }
                    ind
                });
            circ_ids.push(circ_ind);
            pattern_ids.push(all_pattern_ids[circ_ind]);
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
    println!("Done in {}s", start_time.elapsed().as_secs());

    println!("Skipping optimising");
    // let start_time = Instant::now();
    // println!("Optimising...");
    // matcher.optimise(10, 3);
    // println!("Done in {}s", start_time.elapsed().as_secs());

    let to_serialize = CompiledTrie {
        matcher,
        rewrite_rules,
        all_circs,
        pattern2circ,
    };
    let path = path(name);
    fs::create_dir_all(DATA_DIR).unwrap();
    fs::write(&path, serde_json::to_vec(&to_serialize).unwrap())
        .expect(&format!("could not write to {path}"));
    println!("Saved to {path}\n");
}

pub(crate) fn load_matcher(name: &str) -> io::Result<CompiledTrie> {
    let path = path(name);
    Ok(serde_json::from_reader(fs::File::open(&path)?).unwrap())
}

pub(crate) fn load_matcher_or_compile(name: &str) -> io::Result<CompiledTrie> {
    if !Path::new(&path(name)).exists() {
        ensure_exists(name).unwrap();
        let eccs = load_eccs(name);
        compile_eccs(&eccs, name);
    }
    load_matcher(name)
}

fn path(name: &str) -> String {
    format!("{DATA_DIR}/{name}.json")
}

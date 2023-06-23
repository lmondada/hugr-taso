use std::{collections::HashMap, fs, io, path::Path, time::Instant};

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

const DATA_DIR: &str = "data/compiled_tries";

pub fn compile_eccs(mut eccs: Vec<RepCircSet>, name: &str) {
    println!("Building matcher trie...");
    let start_time = Instant::now();
    let mut matcher = HugrMatcher::new(portmatching::TrieConstruction::Balanced);
    let mut rewrite_rules = HashMap::new();
    let mut all_circs = Vec::new();
    let mut pattern2circ = HashMap::new();
    for set in eccs.iter_mut() {
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
        compile_eccs(eccs, name);
    }
    load_matcher(name)
}

fn path(name: &str) -> String {
    format!("{DATA_DIR}/{name}.json")
}

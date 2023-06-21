mod taso;
mod get_files;

use hugr::hugr::HugrView;

use taso::{load_eccs, taso_mpsc};
use get_files::ensure_exists;

fn main() {
    let n_qubits = 2;
    let max_len = 2;
    let ecc_name = format!("Nam_{max_len}_{n_qubits}_complete_ECC_set");

    ensure_exists(&ecc_name).unwrap();
    let eccs = load_eccs(&ecc_name);

    println!("Loaded {} ECCs", eccs.len());
    println!(
        "Total {} circuits",
        eccs.iter().map(|x| x.len()).sum::<usize>()
    );

    // TODO use more useful circuit
    let circ = eccs
        .iter()
        .flat_map(|e| e.others.iter())
        .next()
        .unwrap()
        .clone();

    taso_mpsc(circ, eccs, 1.001, |c| c.hugr().node_count(), 0, num_cpus::get() - 1);
}

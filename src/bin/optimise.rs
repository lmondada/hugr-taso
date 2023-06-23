use hugr::hugr::HugrView;

use hugr_taso::{ensure_exists, load_eccs, taso_mpsc};

fn main() {
    let n_qubits = 3;
    let max_len = 5;
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
        .flat_map(|e| e.circuits())
        .max_by_key(|c| c.hugr().node_count())
        .unwrap()
        .clone();

    taso_mpsc(
        circ,
        &ecc_name,
        1.001,
        |c| c.hugr().node_count(),
        None,
        num_cpus::get() - 1,
    );
}

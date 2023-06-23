use hugr_taso::{compile_eccs, ensure_exists, load_eccs};

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

    compile_eccs(eccs, &ecc_name);
}

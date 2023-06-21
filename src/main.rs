mod taso;

use hugr::hugr::HugrView;
use taso::{taso_mpsc, rep_sets_from_path};

fn main() {
    let eccs = rep_sets_from_path("test_files/h_rz_cxcomplete_ECC_set.json");
    println!("Loaded {} ECCs", eccs.len());
    println!(
        "Total {} circuits",
        eccs.iter().map(|x| x.len()).sum::<usize>()
    );

    // TODO use more useful circuit
    let circ = eccs[0].rep_circ.clone();
    taso_mpsc(circ, eccs, 1.001, |c| c.hugr().node_count(), 0, 1);
}

use std::collections::HashMap;

use hugr::builder::{AppendWire, DFGBuilder, Dataflow, DataflowHugr};
use hugr::types::{ClassicType, LinearType, SimpleType};
use serde::{Deserialize, Serialize};

// use crate::circuit::{
//     circuit::Circuit,
//     dag::Edge,
//     operation::{Op, WireType},
// };

use hugr::ops::{LeafOp, OpType as Op};
use hugr::Hugr as Circuit;

#[derive(Debug, Serialize, Deserialize)]
struct RepCircOp {
    opstr: String,
    outputs: Vec<String>,
    inputs: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RepCirc(Vec<RepCircOp>);

#[derive(Debug, Serialize, Deserialize)]
struct MetaData {
    n_qb: usize,
    n_input_param: usize,
    n_total_param: usize,
    num_gates: u64,
    id: Vec<String>,
    fingerprint: Vec<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RepCircData {
    meta: MetaData,
    circ: RepCirc,
}

fn map_op(opstr: &str) -> Op {
    // TODO, more
    match opstr {
        "h" => LeafOp::H,
        "rz" => LeafOp::RzF64,
        "rx" => LeafOp::RxF64,
        "cx" => LeafOp::CX,
        "t" => LeafOp::T,
        "tdg" => LeafOp::Tadj,
        "x" => LeafOp::X,
        "add" => LeafOp::AddF64,
        x => panic!("unknown op {x}"),
    }
    .into()
}

fn map_wt(wirestr: &str) -> (SimpleType, usize) {
    let wt = if wirestr.starts_with('Q') {
        LinearType::Qubit.into()
    } else if wirestr.starts_with('P') {
        ClassicType::F64.into()
    } else {
        panic!("unknown op {wirestr}");
    };

    (wt, wirestr[1..].parse().unwrap())
}
// TODO change to TryFrom
impl From<RepCircData> for Circuit {
    fn from(RepCircData { circ: rc, meta }: RepCircData) -> Self {
        let qb_types: Vec<SimpleType> = vec![LinearType::Qubit.into(); meta.n_qb];
        let param_types: Vec<SimpleType> = vec![ClassicType::F64.into(); meta.n_input_param];

        let mut dfg_builder =
            DFGBuilder::new([qb_types.clone(), param_types].concat(), qb_types).unwrap();
        let mut param_wires: Vec<_> = dfg_builder.input_wires().skip(meta.n_qb).map(Some).collect();
        let mut circ = dfg_builder.as_circuit(dfg_builder.input_wires().take(meta.n_qb).collect());

        for RepCircOp {
            opstr,
            inputs,
            outputs,
        } in rc.0
        {
            let op = map_op(&opstr);

            let hugr_inputs = inputs.into_iter().map(|is| {
                let (wt, idx) = map_wt(&is);
                match wt {
                    SimpleType::Linear(LinearType::Qubit) => AppendWire::I(idx),
                    SimpleType::Classic(ClassicType::F64) => AppendWire::W(param_wires[idx].unwrap()),
                    _ => panic!("unexpected wire type."),
                }
            });
            let param_outputs = outputs.iter().map(|s| map_wt(s)).filter_map(|(wt, idx)| {
                matches!(wt, SimpleType::Classic(ClassicType::F64)).then_some(idx)
            });
            let hugr_outputs = circ.append_with_outputs(op, hugr_inputs).unwrap();
            for (idx, wire) in param_outputs.zip(hugr_outputs) {
                if param_wires.len() <= idx {
                    param_wires.resize(idx + 1, None);
                }
                param_wires[idx] = Some(wire);
            }
        }
        let outputs = circ.finish();
        dfg_builder.finish_hugr_with_outputs(outputs).unwrap()
    }
}

pub(super) fn load_ecc_set(path: &str) -> HashMap<String, Vec<Circuit>> {
    let jsons = std::fs::read_to_string(path).unwrap();
    let (_, ecc_map): (Vec<()>, HashMap<String, Vec<RepCircData>>) =
        serde_json::from_str(&jsons).unwrap();

    ecc_map
        .into_values()
        .map(|datmap| {
            let id = datmap[0].meta.id[0].clone();
            let circs = datmap.into_iter().map(|rcd| rcd.into()).collect();
            (id, circs)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    // use crate::validate::check_soundness;

    use super::*;
    fn load_representative_set(path: &str) -> HashMap<String, Circuit> {
        let jsons = std::fs::read_to_string(path).unwrap();
        // read_rep_json(&jsons).unwrap();
        let st: Vec<RepCircData> = serde_json::from_str(&jsons).unwrap();
        st.into_iter()
            .map(|mut rcd| (rcd.meta.id.remove(0), rcd.into()))
            .collect()
    }

    #[test]
    fn test_read_rep() {
        let rep_map: HashMap<String, Circuit> =
            load_representative_set("test_files/h_rz_cxrepresentative_set.json");

        for c in rep_map.values().take(1) {
            println!("{}", c.dot_string());
        }
    }

    // #[test]
    // fn test_read_complete() {
    //     let ecc: HashMap<String, Vec<Circuit>> =
    //         load_ecc_set("test_files/h_rz_cxcomplete_ECC_set.json");

    //     // ecc.values()
    //     //     .flatten()
    //     //     .for_each(|c| check_soundness(c).unwrap());
    // }
}

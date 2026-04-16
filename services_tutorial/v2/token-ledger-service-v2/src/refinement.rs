//! refinement

use alloc::vec::Vec;
use codec::{Decode, Encode};
use jam_pvm_common::{ApiError, error, info};
use token_ledger_state_v2::Hash;
use token_ledger_state_v2::Mode;

#[derive(Encode, Decode)]
pub struct Payload {
    pub version: Mode,
    pub operations: token_ledger_state_v2::Operations,
    pub witness: token_ledger_state_v2::merkle::Witness,
}

pub fn refine_payload(payload: &[u8]) -> (Vec<u8>, usize) {
    let mut to_solicit = Vec::new();
    let mut exported_segments = Vec::new();
    let mut processed_segments = Vec::new();
    info!(
        "[refinement] Refining transition for payload of length: {}",
        payload.len()
    );
    let (previous_root, new_root, operations_len, version, _) = refine_transition(
        payload,
        &mut to_solicit,
        &mut exported_segments,
        &mut processed_segments,
        true,
        true,
    );
    info!(
        "[refinement] Refinement done. Root {:?} -> {:?}",
        hex::encode(previous_root),
        hex::encode(new_root)
    );
    (
        crate::accumulation::Operation {
            version,
            previous_root,
            new_root,
            to_solicit,
            exported_segments,
            processed_segments,
        }
        .encode(),
        operations_len,
    )
}

fn refine_transition(
    mut payload: &[u8],
    to_solicit: &mut Vec<token_ledger_state_v2::Solicit>,
    exported_segments: &mut Vec<Hash>,
    processed_segments: &mut Vec<Hash>,
    allow_preimage: bool,
    handle_segments: bool,
) -> (Hash, Hash, usize, Mode, usize) {
    let original_payload = payload;
    let Payload {
        version,
        operations,
        witness,
    } = match Payload::decode(&mut payload) {
        Ok(ops) => ops,
        Err(e) => {
            error!("Failed to parse signed operations: {}", e);
            // TODO noops but should not forward this to accumulate
            return (Default::default(), Default::default(), 0, Mode::Direct, 0);
        }
    };

    let read_size = original_payload.len() - payload.len();

    let mut operations_len = operations.len();
    info!(
        "[refinement] Payload: {} operations and version {:?}",
        operations_len, version
    );

    let Some(mut partial_state) = token_ledger_state_v2::merkle::State::from_witness(witness)
    else {
        error!("error loading state");
        unimplemented!("TODO error report in work output ?");
    };

    info!("loaded state from witness: {}", partial_state);
    let previous_root = partial_state.get_root();

    if version == Mode::Segment {
        if handle_segments {
            // put in segment
            // here we should large payload put in multiple segments, but for tutorial we only use one and panic when payload too big.
            match jam_pvm_common::refine::export_slice(original_payload) {
                Ok(exported) => {
                    let exported_hash = token_ledger_state_v2::hash_multiple(&[original_payload]);
                    info!(
                        "Inserted segment with hash {}, at index {}",
                        hex::encode(&exported_hash),
                        exported
                    );
                    exported_segments.push(exported_hash);
                    // TODO a real noops would be better
                    return (
                        previous_root,
                        previous_root,
                        operations_len,
                        version,
                        read_size,
                    );
                }
                Err(ApiError::StorageFull) => {
                    error!("cannot add segment, storage full, ignoring");
                }
                Err(e) => {
                    error!("fail pushing segment {:?}", e);
                    panic!("fail pushing segment {:?}", e);
                }
            }

            return (previous_root, previous_root, 1, version, read_size);
        }
        // payload loaded from process segment will process next
    }
    if version == Mode::ProcessSegments {
        let mut new_root = previous_root;
        for ix in 0.. {
            match jam_pvm_common::refine::import(ix) {
                Some(segment) => {
                    // note segment is padded, this is not an issue with payload decoding
                    info!("Loading transition from segment, root {:?}.", previous_root);
                    let (proot, nroot, ops, _, size) = refine_transition(
                        segment.as_slice(),
                        to_solicit,
                        exported_segments,
                        processed_segments,
                        false,
                        false,
                    );

                    // Note this force to run segment in sequence at this point
                    if proot != new_root {
                        error!("processing segment witness fail due to updated root");
                    }
                    new_root = nroot;
                    info!("Transition from segment new root: {:?}.", new_root);
                    operations_len += ops;
                    let segment_hash =
                        token_ledger_state_v2::hash_multiple(&[&segment.as_slice()[0..size]]);
                    processed_segments.push(segment_hash);
                }
                None => break,
            }
        }
        return (previous_root, new_root, operations_len, version, read_size);
    }

    let transition_result =
        token_ledger_state_v2::state_transition(&mut partial_state, &operations, false);

    let mut new_root = partial_state.get_root();

    for solicit in transition_result.to_solicit {
        if solicit.on_root != previous_root {
            error!(
                "Skip a solicit preimage on non current root: {:?}, {:?}",
                solicit.hash, solicit.on_root
            );
            continue;
        }
        info!("looking up preimage");
        if let Some(preimage) = jam_pvm_common::refine::lookup(&solicit.hash) {
            info!("got  preimage");
            if !allow_preimage {
                continue;
            }
            info!("loading transition from preimage");
            let (proot, nroot, ops, _, _) = refine_transition(
                &preimage,
                to_solicit,
                exported_segments,
                processed_segments,
                false,
                false,
            );

            // Note this force to run preimage in sequence at this point
            if proot != new_root {
                error!("processing preimage witness fail due to updated root");
            }
            new_root = nroot;
            operations_len += ops;
        } else {
            // solicit
            info!("solliciting  preimage");
            to_solicit.push(solicit);
        }
    }

    info!("[refinement] refine transition done");

    return (previous_root, new_root, operations_len, version, read_size);
}

#[test]
fn encode_process_payload() {
    let process_payload = Payload {
        version: Mode::ProcessSegments,
        operations: Default::default(),
        witness: Default::default(),
    };
    let encoded = process_payload.encode();
    let hex_encoded = hex::encode(&encoded);

    assert_eq!(hex_encoded, "0300000000");

    hex::decode(&hex_encoded).unwrap();
    Payload::decode(&mut encoded.as_slice()).unwrap();
}

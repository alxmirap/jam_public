use super::*;
use codec::{Decode, Encode};
use jam_pvm_common::accumulate::{checkpoint, get_storage, set_storage};
use jam_pvm_common::{info, warn};
use jam_types::{TransferRecord, WorkItemRecord};

pub fn on_transfer(item: TransferRecord) {
    info!(
        "Received service balance transfer: {} from {:x}h",
        item.amount, item.source
    );
}

pub fn on_work_item(record: WorkItemRecord) {
    let output = match record.result {
        Ok(output) => output,
        Err(e) => {
            warn!("Work item failed in refine: {:?}", e);
            return;
        }
    };

    let refinement_output = match RefinementOutput::decode(&mut &output[..]) {
        Ok(o) => o,
        Err(e) => {
            warn!("Failed to decode refinement output: {:?}", e);
            return;
        }
    };

    match refinement_output {
        RefinementOutput::TransferBatch { old_root, new_root } => {
            info!(
                "TokenLedgerV2 Accumulate: processing transfer batch with root {}",
                hex::encode(old_root)
            );

            let current_root: StateRoot = get_storage(STATE_ROOT_KEY)
                .and_then(|b| StateRoot::decode(&mut &b[..]).ok())
                .unwrap_or([0u8; 32]);

            if current_root != old_root {
                warn!(
                    "TokenLedgerV2 Accumulate: Root mismatch: chain has {}, batch expects {}",
                    hex::encode(current_root),
                    hex::encode(old_root)
                );
                return;
            }

            let _ = set_storage(STATE_ROOT_KEY, &new_root.encode());
            info!(
                "TokenLedgerV2 Accumulate: State root updated: {} -> {}",
                hex::encode(old_root),
                hex::encode(new_root),
            );
            checkpoint();
        }

        RefinementOutput::Reset { new_root } => {
            info!(
                "TokenLedgerV2 Accumulate: processing reset to new root {}",
                hex::encode(new_root)
            );
            match set_storage(STATE_ROOT_KEY, &new_root.encode()) {
                Ok(_) => {
                    info!(
                        "TokenLedgerV2 Accumulate: state root reset to {}",
                        hex::encode(new_root)
                    );
                }
                Err(e) => {
                    warn!(
                        "TokenLedgerV2 Accumulate: Failed to update state root during reset: {:?}",
                        e
                    );
                    return;
                }
            }
            checkpoint();
        }
    }
}

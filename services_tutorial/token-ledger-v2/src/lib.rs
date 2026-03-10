//! Token Ledger V2: L2-style service demonstrating Builder pattern with D3L
//!
//! This service stores only a state commitment (root) on-chain. The actual balances
//! are maintained off-chain by the Builder. Transactions are submitted via extrinsics
//! and exported to D3L for history/verification.
//!
//! ## Operations
//! - **TransferBatch**: Batch of signed transfers, verified in refine, exported to D3L
//! - **Reset**: Admin operation to set a new state root (for initialization or recovery)
//!
//! ## Authorization
//! - Transfers: Each sender signs their own transfer
//! - Reset: Requires admin signature (hard-coded public key)

#![cfg_attr(any(target_arch = "riscv32", target_arch = "riscv64"), no_std)]
extern crate alloc;

use alloc::vec::Vec;
use codec::{Decode, Encode};
use ed25519_consensus::VerificationKeyBytes;
use jam_pvm_common::{Service, accumulate, declare_service, error, info, warn};
use jam_types::{
    AccumulateItem, CoreIndex, Hash, ServiceId, Slot, WorkOutput, WorkPackageHash, WorkPayload,
};

mod accumulation;
mod json;
mod refinement;
pub use json::decode_fixed_hex;
pub use refinement::{reset_signing_message, transfer_signing_message};

/// 32-byte account identifier (Ed25519 public key)
pub type AccountId = [u8; 32];

/// 32-byte state commitment
pub type StateRoot = [u8; 32];

/// Storage key for the current state root
pub const STATE_ROOT_KEY: &[u8] = b"state_root";

/// Hard-coded admin public key for reset operations.
/// This is of course not to be used in production,
/// but serves as a simple example of an authorized operation.
///
/// This key is derived from a deterministic seed ("admin" + padding) to match
/// the builder's admin_keypair() function.
pub fn admin() -> VerificationKeyBytes {
    use ed25519_consensus::SigningKey;

    // Same seed as builder's admin_keypair()
    let mut seed = [0u8; 32];
    seed[0..5].copy_from_slice(b"admin");

    let signing_key = SigningKey::from(seed);
    signing_key.verification_key().to_bytes().into()
}

/// A signed transfer request
#[derive(Clone, Debug, Encode, Decode)]
pub struct Transfer {
    pub from: AccountId,
    pub to: AccountId,
    pub amount: u64,
    pub nonce: u64,
    pub signature: [u8; 64],
}

/// Discriminator for payload types
#[derive(Clone, Debug, Encode, Decode)]
pub enum Payload {
    TransferBatch {
        old_root: StateRoot,
        new_root: StateRoot,
    },
    Reset {
        new_root: StateRoot,
        signature: [u8; 64],
    },
}

/// Output from refine to accumulate
#[derive(Clone, Debug, Encode, Decode)]
pub enum RefinementOutput {
    TransferBatch {
        old_root: StateRoot,
        new_root: StateRoot,
    },
    Reset {
        new_root: StateRoot,
    },
}

/// The Token Ledger V2 Service
pub struct TokenLedgerV2;
declare_service!(TokenLedgerV2);

impl Service for TokenLedgerV2 {
    fn refine(
        _core_index: CoreIndex,
        _item_index: usize,
        service_id: ServiceId,
        payload: WorkPayload,
        package_hash: WorkPackageHash,
    ) -> WorkOutput {
        use jam_pvm_common::refine;

        info!("TokenLedgerV2 refine on service {service_id:x}h, package {package_hash}");

        // Decode payload
        let payload = match json::parse_payload(&payload) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to decode payload: {}", e);
                return Vec::new().into();
            }
        };

        match payload {
            Payload::TransferBatch { old_root, new_root } => {
                info!(
                    "Processing TransferBatch from {} to {}",
                    hex::encode(old_root),
                    hex::encode(new_root)
                );
                let extrinsic = refine::extrinsic(0);
                if let Some(extrinsic_data) = extrinsic.as_ref() {
                    info!("Found extrinsic data with {} bytes", extrinsic_data.len());

                    let transfers: Vec<Transfer> = match Decode::decode(&mut &extrinsic_data[..]) {
                        Ok(t) => t,
                        Err(e) => {
                            error!("Failed to decode transfers from extrinsic: {:?}", e);
                            return Vec::new().into();
                        }
                    };

                    if refinement::verify_transfers(&transfers) {
                        info!("All transfer signatures verified successfully");
                    } else {
                        error!("One or more transfer signatures failed verification");
                        return Vec::new().into();
                    }

                    if extrinsic_data.len() <= jam_types::SEGMENT_LEN {
                        match refine::export_slice(&extrinsic_data) {
                            Ok(index) => info!(
                                "Exported transfers to D3L segment: {} {}",
                                hex::encode(&package_hash.0),
                                index
                            ),
                            Err(e) => warn!("Failed to export to D3L: {:?}", e),
                        }
                    } else {
                        warn!(
                            "Transfer batch too large for single segment: {} bytes",
                            extrinsic_data.len()
                        );
                    }

                    refinement::on_transfer_batch(old_root, new_root, &transfers)
                } else {
                    info!("no extrinsic at index 0. Nothing to do.");
                    return Vec::new().into();
                }
            }
            Payload::Reset {
                new_root,
                signature,
            } => refinement::on_reset(new_root, signature),
        }
    }

    fn accumulate(slot: Slot, service_id: ServiceId, item_count: usize) -> Option<Hash> {
        info!(
            "TokenLedgerV2 accumulate on service {service_id:x}h @{slot} with {item_count} items"
        );

        for item in accumulate::accumulate_items() {
            match item {
                AccumulateItem::WorkItem(record) => accumulation::on_work_item(record),
                AccumulateItem::Transfer(t) => accumulation::on_transfer(t),
            }
        }

        None
    }
}

pub const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

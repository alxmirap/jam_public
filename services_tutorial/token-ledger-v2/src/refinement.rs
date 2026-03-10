use super::*;
use alloc::vec::Vec;
use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};
use codec::Encode;
use ed25519_consensus::{Signature, VerificationKey};
use jam_pvm_common::{error, info, warn};
use jam_types::WorkOutput;

type Blake2b256 = Blake2b<U32>;

pub fn on_transfer_batch(
    old_root: StateRoot,
    new_root: StateRoot,
    transfers: &[Transfer],
) -> WorkOutput {
    let computed_root = compute_batch_root(&old_root, &transfers);

    if computed_root != new_root {
        error!(
            "Root mismatch: computed={}, claimed={}",
            hex::encode(computed_root),
            hex::encode(new_root)
        );
        return Vec::new().into();
    }

    let output = RefinementOutput::TransferBatch { old_root, new_root };

    output.encode().into()
}

pub fn on_reset(new_root: StateRoot, signature: [u8; 64]) -> WorkOutput {
    info!("Processing reset to root={}", hex::encode(new_root));

    if !verify_reset_signature(new_root, signature) {
        warn!("Invalid admin signature for reset");
        return Vec::new().into();
    }

    let output = RefinementOutput::Reset { new_root };

    info!("Reset verified successfully");
    output.encode().into()
}

fn compute_batch_root(old_root: &StateRoot, transfers: &[Transfer]) -> StateRoot {
    let mut hasher = Blake2b256::new();
    hasher.update(old_root);
    hasher.update(&transfers.encode());
    let result = hasher.finalize();

    let mut output = [0u8; 32];
    output.copy_from_slice(&result);
    output
}

pub fn verify_transfers(transfers: &[Transfer]) -> bool {
    for (i, transfer) in transfers.iter().enumerate() {
        if transfer.amount == 0 {
            warn!("Transfer {} rejected: zero amount", i);
            return false;
        }
        if transfer.from == transfer.to {
            warn!("Transfer {} rejected: self-transfer", i);
            return false;
        }
        if !verify_transfer_signature(transfer) {
            warn!("Transfer {} rejected: invalid signature", i);
            return false;
        }
    }
    true
}

fn verify_transfer_signature(transfer: &Transfer) -> bool {
    let signing_message = transfer_signing_message(
        &transfer.from,
        &transfer.to,
        transfer.nonce,
        transfer.amount,
    );

    let key = match VerificationKey::try_from(transfer.from) {
        Ok(key) => key,
        Err(_) => return false,
    };

    let signature = Signature::from(transfer.signature);
    key.verify(&signature, &signing_message).is_ok()
}

fn verify_reset_signature(new_root: StateRoot, signature: [u8; 64]) -> bool {
    let signing_message = reset_signing_message(new_root);
    let admin_key: [u8; 32] = admin().into();

    let key = match VerificationKey::try_from(admin_key) {
        Ok(key) => key,
        Err(_) => return false,
    };

    let signature = Signature::from(signature);
    key.verify(&signature, &signing_message).is_ok()
}

pub fn transfer_signing_message(
    from: &AccountId,
    to: &AccountId,
    nonce: u64,
    amount: u64,
) -> [u8; 32] {
    let mut raw = [0u8; 80];
    raw[0..32].copy_from_slice(from);
    raw[32..64].copy_from_slice(to);
    raw[64..72].copy_from_slice(&nonce.to_le_bytes());
    raw[72..80].copy_from_slice(&amount.to_le_bytes());

    let mut hasher = Blake2b256::new();
    hasher.update(&raw);
    let digest = hasher.finalize();

    let mut out = [0u8; 32];
    out.copy_from_slice(&digest[..]);
    out
}

pub fn reset_signing_message(new_root: StateRoot) -> [u8; 32] {
    let mut raw = [0u8; 32];
    raw[0..32].copy_from_slice(&new_root);

    let mut hasher = Blake2b256::new();
    hasher.update(&raw);
    let digest = hasher.finalize();

    let mut out = [0u8; 32];
    out.copy_from_slice(&digest[..]);
    out
}

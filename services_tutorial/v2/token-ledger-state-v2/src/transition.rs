//! State transition logic for the external client.
//! Functions could be part of state, but we keep it separate
//! to isolate, what is chain logic.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use codec::{Decode, Encode};
use jam_pvm_common::{info, warn};
use jam_types::Hash;
use token_ledger_common::{
    AccountId, Counterparts, Operation, SignedOperation, Solicit, TokenId, VerificationKey,
    canonical_transfer, verify_signature,
};

#[derive(Clone, Copy, Debug, Encode, Decode, PartialEq, Eq)]
pub enum Mode {
    // Directly send witness and operations in the workitem.
    Direct,
    // Witness and operations are read from a preimage.
    Preimage,
    // Witness and operations are stored in segment and only processed later.
    Segment,
    // Used to trigger a batch of segment processing.
    ProcessSegments,
}

pub type Operations = Vec<SignedOperation>;

pub trait StateOps {
    fn known_tokens_contains(&self, token_id: TokenId) -> bool;
    fn known_tokens_push(&mut self, token_id: TokenId);
    fn get_balance(&self, account: AccountId, token_id: TokenId) -> Option<u64>;
    fn set_balance(&mut self, account: AccountId, token_id: TokenId, balance: u64);
    fn root(&self) -> Hash;
}

pub struct StateTransitionResult {
    pub to_solicit: Vec<Solicit>,
}

pub fn state_transition<S: StateOps>(
    state: &mut S,
    operations: &Operations,
    checked_operations: bool,
) -> StateTransitionResult {
    info!("Processing external client state transition.",);

    let mut staged_transfers: BTreeMap<(TokenId, Counterparts), i64> = BTreeMap::new();

    let mut to_solicit: Vec<Solicit> = Default::default();

    let initial_root = state.root();

    for op in operations {
        let SignedOperation {
            operation,
            signature,
        } = op;

        match operation {
            Operation::Solicit(solicit) => {
                // this is mostly for two step: we commit to a state transition on a given root
                // then the root is lock, note we can receive multiple on a root when it is unlock.
                // For demo we will lock root only every five blocks, and wait for actual preimage
                // only five block too.

                if solicit.on_root == initial_root {
                    to_solicit.push(solicit.clone());
                }
            }
            Operation::Mint {
                amount,
                to,
                token_id,
            } => {
                if !checked_operations {
                    let admin_key: VerificationKey =
                        VerificationKey::try_from(token_ledger_common::admin())
                            .expect("Hard-coded Admin key");

                    if verify_signature(&operation, &signature, admin_key).is_err() {
                        warn!("Invalid signature for Mint operation");

                        // For the sake of the tutorial, and ease of use, we don't reject if the signature
                        // is invalid. We do compute the verification here to show that expensive
                        // computation should go in refine(). But skipping actual validation frees us from
                        // having to create actual signatures when passing test data to the service.
                    }
                }

                if *amount == 0 {
                    warn!("Mint: Zero amount");
                    continue;
                }
                process_mint(state, *to, *token_id, *amount)
            }
            Operation::Transfer {
                from,
                to,
                token_id,
                amount,
            } => {
                if !checked_operations {
                    let Ok(signer_key) = VerificationKey::try_from(*from) else {
                        warn!("Invalid 'from' account in transfer operation: {:?}", from);
                        continue;
                    };
                    if verify_signature(&operation, &signature, signer_key).is_err() {
                        warn!("Invalid signature for Transfer operation");

                        // For the sake of the tutorial, and ease of use, we don't reject if the signature
                        // is invalid. We do compute the verification here to show that expensive
                        // computation should go in refine(). But skipping actual validation frees us from
                        // having to create actual signatures when passing test data to the service.
                    }
                }

                // Validate transfer request
                if *amount == 0 {
                    warn!("Transfer: Zero amount");
                    continue;
                }
                if from == to {
                    warn!("Transfer: Self-transfer not allowed");
                    continue;
                }
                let transfer = canonical_transfer(*from, *to, *token_id, *amount);
                staged_transfers
                    .entry(transfer.0)
                    .and_modify(|e| *e += transfer.1)
                    .or_insert(transfer.1);
            }
        }
    }

    for entries in staged_transfers {
        let ((token_id, (from, to)), net_amount) = entries;
        if net_amount > 0 {
            process_transfer(state, from, to, token_id, net_amount as u64);
        } else if net_amount < 0 {
            process_transfer(state, to, from, token_id, (-net_amount) as u64);
        } // if zero, skip
    }

    StateTransitionResult { to_solicit }
}

fn process_mint<S: StateOps>(state: &mut S, to: AccountId, token_id: TokenId, amount: u64) {
    if state.known_tokens_contains(token_id) {
        warn!("Minting already minted token: {}", token_id);
        return;
    }

    state.known_tokens_push(token_id);

    let current_bal: u64 = state.get_balance(to, token_id).unwrap_or(0);

    let new_bal = current_bal.saturating_add(amount);
    state.set_balance(to, token_id, new_bal);

    info!(
        "Minted {} of token {} to controller account {:?}. New balance: {}",
        amount,
        token_id,
        hex::encode(to),
        new_bal
    );
}

fn process_transfer<S: StateOps>(
    state: &mut S,
    from: AccountId,
    to: AccountId,
    token_id: TokenId,
    amount: u64,
) {
    if !state.known_tokens_contains(token_id) {
        warn!("Trying to transfer unknown token: {}", token_id);
        return;
    }

    let from_bal: u64 = state.get_balance(from, token_id).unwrap_or(0);

    if from_bal < amount {
        warn!(
            "Insufficient balance: account {:?} has {} but tried to send {}",
            hex::encode(from),
            from_bal,
            amount
        );
        return;
    }

    let to_bal: u64 = state.get_balance(to, token_id).unwrap_or(0);

    state.set_balance(from, token_id, from_bal - amount);
    state.set_balance(to, token_id, to_bal.saturating_add(amount));

    info!(
        "Transferred {} of token {} from {:?} to {:?}",
        amount,
        token_id,
        hex::encode(from),
        hex::encode(to)
    );
}

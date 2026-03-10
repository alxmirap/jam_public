// This module groups together all the functionality for account and key management.

use super::*;

/// A keypair for signing
#[derive(Clone, Debug)]
pub struct Keypair {
    pub signing_key: SigningKey,
    pub public_key: AccountId,
}

impl Keypair {
    /// Create a keypair from a 32-byte seed
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from(*seed);
        let verification_key = signing_key.verification_key();
        let public_key: AccountId = verification_key.to_bytes();
        Self {
            signing_key,
            public_key,
        }
    }

    /// Sign a message
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }
}

/// Generate a deterministic keypair for testing (index-based)
pub fn generate_keypair(index: usize) -> Keypair {
    let mut seed = [0u8; 32];
    seed[0..8].copy_from_slice(&(index as u64).to_le_bytes());
    Keypair::from_seed(&seed)
}

/// Admin keypair (deterministic for reproducibility)
pub fn admin_keypair() -> Keypair {
    let mut seed = [0u8; 32];
    seed[0..5].copy_from_slice(b"admin");
    Keypair::from_seed(&seed)
}

#[derive(Clone, Debug)]
pub struct AccountState {
    pub keypair: Keypair,
    pub balance: u64,
    pub nonce: u64,
}

impl BuilderState {
    /// Initialize with a small set of demo accounts
    pub fn init_accounts(&mut self, count: usize, balance: u64) {
        self.accounts.clear();
        self.pending_net_transfers.clear();

        for i in 0..count {
            self.register_sender_account(i, balance);
        }

        self.current_root = self.compute_state_root();

        println!(
            "Initialized state with {} accounts, root: {}",
            self.accounts.len(),
            hex::encode(self.current_root)
        );

        for (index, account_state) in &self.accounts {
            println!(
                "  Account {}: {}",
                index,
                &hex::encode(account_state.keypair.public_key)[..16]
            );
        }
    }

    pub fn register_sender_account(&mut self, index: usize, balance: u64) {
        if let std::collections::btree_map::Entry::Vacant(e) = self.accounts.entry(index) {
            let keypair = generate_keypair(index);
            let account = keypair.public_key;
            e.insert(AccountState {
                keypair,
                balance,
                nonce: 0,
            });
            println!(
                "Registered new sender account with index {index}: {}",
                hex::encode(account)
            );
        }
    }
}

pub(crate) fn canonical_counterpart_delta<T>(
    from: usize,
    to: usize,
    amount: T,
) -> (usize, usize, i128)
where
    T: Into<i128>,
{
    if from < to {
        (from, to, amount.into())
    } else {
        (to, from, -amount.into())
    }
}

//! Merkle state, build over a simple fix size binary tree and linked data.
//! For the sake of keeping this example concise, client implementation only manage
//! two use cases (in realy world for efficiency there is many processing that can be
//! skip for others use cases):
//! - client (with unsafe persistence (breaks on any crash), and always registering witnesses).
//! - pvm, no std, riscv targetting and running on partial state build from witness.
//!
//! Client implementation is obtain by including "std" feature, while pvm is obtain by
//! building no std.

use super::{
    EMPTY_HASH, Hash, MerkleValue, TREE_DEPTH, TreeIndex, hash_multiple, hash_pair, hash_sequence,
    tree_index_from_key,
};

use alloc::collections::BTreeMap;
use alloc::fmt;
use alloc::vec::Vec;
use codec::{Decode, Encode};
use token_ledger_common::{AccountId, TokenId};

// very small state size, expect hash collisions (jut fail on hash collision: we store key so we
// can see if hash collision)_
pub const TREE_SIZE: usize = 1 << TREE_DEPTH;
pub const TREE_HASHES: usize = (TREE_SIZE * 2) - 1;

// We use some small bounded simple binary tree (2^15 itemrs) for the sake of having the simpliest implementation.
// We also avoid optimization to have same kind of footprint between empty state and full state,
// and a cost model relatively easy to reason with.
// Merkle hash 0 is used for empty state so we can use hash default implementation for it.
// A key access involve a witness of 15 hash and the actual key and value.
// Missing value in witness will false positive return an empty hash leading to root mismatch, not
// the best for debugging purpose.
#[derive(Default)]
pub struct MerkleTree {
    // TODO non pub
    pub hashes: BTreeMap<TreeIndex, Hash>,
}

impl MerkleTree {
    // Note this must always be call to access any `hashes field content`,
    // so we register witness properly.
    pub fn get_hash(&self, ix: TreeIndex) -> &Hash {
        if let Some(hash) = self.hashes.get(&ix) {
            return hash;
        } else {
            return &EMPTY_HASH;
        }
    }

    pub fn root(&self) -> &Hash {
        return self.get_hash((TREE_HASHES - 1) as TreeIndex);
    }

    pub fn insert(&mut self, ix: TreeIndex, value_hash: Hash) {
        let mut hash = value_hash;
        let mut at = ix;
        let mut offset: TreeIndex = 0;
        for depth in 0..TREE_DEPTH {
            self.hashes.insert(offset + at, hash);
            if at % 2 == 0 {
                hash = hash_pair(&hash, self.get_hash(offset + at + 1));
            } else {
                hash = hash_pair(self.get_hash(offset + at - 1), &hash);
            }
            offset += 1 << (TREE_DEPTH - depth);
            at = at / 2;
        }

        self.hashes.insert(offset + at, hash);
    }
}

#[derive(Default)]
pub struct State {
    balances: StateTree<Balance>,
    known_tokens: KnownTokens,
}

#[derive(Default, Encode, Decode, Debug)]
pub struct Witness {
    // root is part of the hashes
    pub hashes: Vec<(TreeIndex, Hash)>,
    pub key_value_balances: Vec<(Vec<u8>, Balance)>,
    // Currently no operation make sense without accessing it so always store.
    pub token_ids: Vec<TokenId>,
}

impl State {
    pub fn from_witness(witness: Witness) -> Option<Self> {
        let mut result = Self::default();
        if let Some(balances) =
            StateTree::init_from_witness(&witness.hashes, witness.key_value_balances)
        {
            result.balances = balances;
        } else {
            return None;
        }

        result.known_tokens.token_ids = witness.token_ids;

        return Some(result);
    }

    pub fn get_root(&self) -> Hash {
        return hash_pair(self.balances.root(), &self.known_tokens.merkle_value());
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "State {{ root: {}, known accounts: {:?}, known_tokens: {:?} }}",
            hex::encode(self.get_root()),
            self.balances.key_values.len(),
            self.known_tokens.token_ids
        )
    }
}

impl crate::transition::StateOps for State {
    fn root(&self) -> Hash {
        self.get_root()
    }

    fn get_balance(&self, account: AccountId, token_id: TokenId) -> Option<u64> {
        let to_key = token_ledger_common::balance_key(token_id, &account);
        self.balances.get(to_key.as_slice()).cloned()
    }

    fn set_balance(&mut self, account: AccountId, token_id: TokenId, balance: u64) {
        let to_key = token_ledger_common::balance_key(token_id, &account);
        if !self.balances.set(to_key.to_vec(), balance) {
            unimplemented!("error on key collision");
        }
    }

    fn known_tokens_contains(&self, token_id: TokenId) -> bool {
        return self.known_tokens.token_contains(token_id);
    }

    fn known_tokens_push(&mut self, token_id: TokenId) {
        self.known_tokens.push_token(token_id);
    }
}

pub trait ValueTraits: Clone + Decode + Encode + MerkleValue {}

impl<V: Clone + Decode + Encode + MerkleValue> ValueTraits for V {}

pub struct StateTree<V: ValueTraits> {
    /// Key and values for given tree indexe (u15).
    /// Allows access to state key and to value by tree index.
    pub key_values: BTreeMap<TreeIndex, KeyValue<V>>,
    /// Merkle tree, contains all merkle hashes of leafs.
    pub tree: MerkleTree,
}

impl<V: ValueTraits> Default for StateTree<V> {
    fn default() -> Self {
        Self {
            key_values: Default::default(),
            tree: Default::default(),
        }
    }
}

impl<V: ValueTraits> StateTree<V> {
    fn get_value(&self, k: &[u8]) -> Option<&KeyValue<V>> {
        let ix = tree_index_from_key(&k);
        if let Some(v) = self.key_values.get(&ix) {
            if v.key.as_slice() != k {
                return None;
            }
            return Some(v);
        }
        None
    }

    pub fn get(&self, k: &[u8]) -> Option<&V> {
        let v = self.get_value(k)?;
        if v.key.as_slice() != k {
            return None;
        };
        Some(&v.value)
    }

    // fail on key collision by returning false
    pub fn set(&mut self, k: Vec<u8>, v: V) -> bool {
        let ix = tree_index_from_key(&k);
        if let Some(existing) = self.key_values.get_mut(&ix) {
            if existing.key.as_slice() == k {
                existing.value = v;
                self.tree.insert(ix, existing.merkle_value());
            } else {
                return false;
            }
        } else {
            let value = KeyValue { key: k, value: v };
            self.tree.insert(ix, value.merkle_value());
            self.key_values.insert(ix, value);
        }
        true
    }

    pub fn root(&self) -> &Hash {
        self.tree.root()
    }

    pub fn init_from_witness(
        witness_hashes: &[(TreeIndex, Hash)],
        witness_key_values: Vec<(Vec<u8>, V)>,
    ) -> Option<Self> {
        let mut result = Self::default();
        // insert all witness hashes
        for (index, hash) in witness_hashes.iter() {
            result.tree.hashes.insert(*index, *hash);
        }
        let witness_root = *result.root();
        for (key, value) in witness_key_values.into_iter() {
            result.set(key, value);
            // set should not change root injected from hashes.
            if result.root() != &witness_root {
                return None;
            }
        }

        Some(result)
    }
}

#[derive(Default)]
pub struct KnownTokens {
    pub token_ids: Vec<TokenId>,
}

impl MerkleValue for KnownTokens {
    fn merkle_value(&self) -> Hash {
        if self.token_ids.len() > 0 {
            hash_sequence(self.token_ids.as_slice())
        } else {
            EMPTY_HASH
        }
    }
}

impl KnownTokens {
    pub fn push_token(&mut self, token_id: TokenId) {
        if !self.token_ids.iter().any(|t| t == &token_id) {
            self.token_ids.push(token_id);
        }
    }

    pub fn token_contains(&self, token_id: TokenId) -> bool {
        self.token_ids.contains(&token_id)
    }
}

pub type Balance = u64;

impl MerkleValue for Balance {
    fn merkle_value(&self) -> Hash {
        let mut result = [0; 32];
        result[0..8].copy_from_slice(u64::to_le_bytes(*self).as_slice());
        result
    }
}

impl MerkleValue for TokenId {
    fn merkle_value(&self) -> Hash {
        let mut result = [0; 32];
        // could actually put 8 tokenid per hash_value, no need here
        result[0..4].copy_from_slice(u32::to_le_bytes(*self).as_slice());
        result
    }
}

#[derive(Clone, Encode, Decode)]
pub struct KeyValue<V> {
    pub value: V,
    pub key: Vec<u8>,
}

impl<V: MerkleValue> MerkleValue for KeyValue<V> {
    fn merkle_value(&self) -> Hash {
        hash_multiple(&[self.value.merkle_value().as_slice(), self.key.as_slice()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state() {
        let empty: State = Default::default();

        assert_eq!([0; 32], empty.get_root());
    }
    #[test]
    fn check_constants() {
        assert_eq!(TREE_DEPTH + 1, core::mem::size_of::<TreeIndex>() * 8);
    }
}

//! client state is simply using the shared state code, and
//! adding to it a simple persistence system  (unsafe, breaks on any crash), and always register witnesses.

use std::cell::RefCell;
use std::collections::BTreeMap;

use codec::{Decode, Encode};
use std::io::{Read, Seek, Write};
use token_ledger_common::{AccountId, TokenId};
use token_ledger_state_v2::merkle::{
    Balance, KeyValue, MerkleTree, TREE_HASHES, ValueTraits, Witness,
};
use token_ledger_state_v2::{
    EMPTY_HASH, Hash, MerkleValue, StateOps, TREE_DEPTH, TreeIndex, hash_pair, tree_index_from_key,
};

#[derive(Default)]
pub struct State {
    balances: StateTree<Balance>,
    known_tokens: KnownTokens,
    persist: Option<std::path::PathBuf>,
}

impl State {
    fn read_head(mut db_location: std::path::PathBuf) -> Hash {
        db_location.push("HEAD");

        if let Ok(mut db_file) = std::fs::File::open(db_location) {
            let mut hash_str = String::new();
            db_file.read_to_string(&mut hash_str).unwrap();
            let hash_vec = hex::decode(hash_str).unwrap();
            let mut hash = EMPTY_HASH;
            hash.copy_from_slice(&hash_vec);
            hash
        } else {
            EMPTY_HASH
        }
    }

    pub fn from_db_path(db_location: std::path::PathBuf, head: Option<Hash>) -> Self {
        println!("[-----------------------------------]");
        println!(
            "Restoring state from db path: [{}]. Overload head: {:?}\n",
            db_location.display(),
            head
        );

        let head_hash = if let Some(h) = head {
            h
        } else {
            Self::read_head(db_location.clone())
        };
        println!("Determined Head hash: {}\n", hex::encode(head_hash));

        let state = if head_hash != EMPTY_HASH {
            let mut state_path = db_location.clone();
            state_path.push(hex::encode(head_hash));
            println!(
                "Head not at genesis. Reading state from db location: {}",
                state_path.display()
            );
            let mut db_file = std::fs::File::open(state_path).unwrap();
            let mut reader = codec::IoReader(&mut db_file);
            State {
                balances: StateTree::<Balance>::from_stream(&mut reader),
                known_tokens: KnownTokens::from_stream(&mut reader),
                persist: Some(db_location),
            }
        } else {
            println!("Head at genesis, starting with empty state");
            let mut state = State::default();
            state.set_new_persist_file(db_location);
            state
        };
        println!("\nRestored state: {}", state);
        println!("[-----------------------------------]");
        state
    }

    pub fn set_new_persist_file(&mut self, db_location: std::path::PathBuf) {
        self.persist = Some(db_location);
    }

    pub fn take_witness(&mut self) -> Witness {
        let (hashes, values) = self.balances.take_witness();
        Witness {
            hashes: hashes.into_iter().collect(),
            key_value_balances: values.into_iter().collect(),
            token_ids: self.known_tokens.take_witness(),
        }
    }

    pub fn from_witness(witness: Witness) -> Option<Self> {
        let mut result = Self::default();
        if let Some(balances) =
            StateTree::init_from_witness(&witness.hashes, witness.key_value_balances)
        {
            result.balances = balances;
        } else {
            return None;
        }

        result.known_tokens.merkle.token_ids = witness.token_ids;

        // init balances witness tokens id initial hashes
        let _ = result.take_witness();

        Some(result)
    }

    pub fn get_root(&self) -> Hash {
        hash_pair(
            self.balances.root(),
            &self.known_tokens.merkle.merkle_value(),
        )
    }

    pub fn serialize(&mut self) {
        let Some(db_location) = self.persist.clone() else {
            return;
        };
        let hash = self.get_root();
        let mut state_path = db_location.clone();
        state_path.push(hex::encode(hash));
        if std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&state_path)
            .is_ok()
        {
            // ignore already a state with this content
        } else {
            let mut file = std::fs::File::create_new(&state_path).unwrap();
            file.seek(std::io::SeekFrom::Start(0)).unwrap();
            self.balances.serialize(&mut file);
            self.known_tokens.serialize(&mut file);
            file.flush().unwrap();
        }
        // update head
        let mut head_path = db_location.clone();
        head_path.push("HEAD");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&head_path)
        {
            file.write_all(hex::encode(hash).as_bytes()).unwrap();
            file.flush().unwrap();
        } else {
            let mut file = std::fs::File::create_new(&head_path).unwrap();
            file.write_all(hex::encode(hash).as_bytes()).unwrap();
            file.flush().unwrap();
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "State {{ root: {}, known accounts: {:?}, known_tokens: {:?} }}",
            hex::encode(self.get_root()),
            self.balances.state.key_values.len(),
            self.known_tokens.merkle.token_ids
        )
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.serialize();
    }
}

impl StateOps for State {
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
        // not registering witness: always pass all tokens to witness
        self.known_tokens.merkle.token_contains(token_id)
    }

    fn known_tokens_push(&mut self, token_id: TokenId) {
        self.known_tokens.merkle.push_token(token_id)
    }
}

#[derive(Default)]
pub struct TreeWitness {
    initial_hashes: BTreeMap<TreeIndex, Hash>,
    witness: RefCell<BTreeMap<TreeIndex, Hash>>,
}

impl TreeWitness {
    fn record_witness_hash<'a>(&self, merkle: &'a MerkleTree, ix: TreeIndex) -> &'a Hash {
        let hash = merkle.get_hash(ix);
        if hash == &EMPTY_HASH {
            return hash;
        }

        let witness_hash = self.initial_hashes.get(&ix).unwrap_or(&EMPTY_HASH);
        if witness_hash != &EMPTY_HASH {
            self.witness.borrow_mut().insert(ix, *witness_hash);
        }

        hash
    }

    fn root<'a>(&self, merkle: &'a MerkleTree) -> &'a Hash {
        self.record_witness_hash(merkle, (TREE_HASHES - 1) as TreeIndex)
    }

    /// Get merkle hash siblings when doing this tree access, it will be included
    /// in the witness.
    fn record_witness_access(&self, merkle: &MerkleTree, ix: TreeIndex) {
        let mut at = ix;
        let mut offset: TreeIndex = 0;
        for depth in 0..TREE_DEPTH {
            if at.is_multiple_of(2) {
                self.record_witness_hash(merkle, offset + at + 1);
            } else {
                self.record_witness_hash(merkle, offset + at - 1);
            }
            offset += 1 << (TREE_DEPTH - depth);
            at /= 2;
        }
    }
}

struct StateTree<V: ValueTraits> {
    state: token_ledger_state_v2::merkle::StateTree<V>,
    tree_witness: TreeWitness,
    /// This records all value accesses to be included in the Witness.
    /// (Witness is both values and merkle tree hashes siblings).
    key_values_witness: RefCell<BTreeMap<Vec<u8>, V>>,
}

impl<V: ValueTraits> Default for StateTree<V> {
    fn default() -> Self {
        Self {
            state: Default::default(),
            tree_witness: Default::default(),
            key_values_witness: Default::default(),
        }
    }
}

impl<V: ValueTraits> StateTree<V> {
    fn from_stream<R: codec::Input>(buf_reader: &mut R) -> Self {
        let mut result = Self::default();
        let nb_item = u64::decode(buf_reader).unwrap();
        dbg!("loading {} items", nb_item);

        for _ in 0..nb_item {
            let v = KeyValue::<V>::decode(buf_reader).unwrap();
            result.set(v.key, v.value);
        }
        result.tree_witness.initial_hashes = result.state.tree.hashes.clone();
        result
    }

    fn serialize<W: Write>(&mut self, w: &mut W) {
        dbg!("serializing {} items", self.state.key_values.len());
        (self.state.key_values.len() as u64).encode_to(w);
        for (_, v) in self.state.key_values.iter() {
            v.encode_to(w);
        }
    }

    fn get_value(&self, k: &[u8]) -> Option<&KeyValue<V>> {
        let ix = tree_index_from_key(k);
        self.tree_witness
            .record_witness_access(&self.state.tree, ix);
        // code is a bit redundant with state but simple enough to not be factored
        let v = self.state.key_values.get(&ix);
        if let Some(value_v) = v.as_ref() {
            // record value in witness
            if !self.key_values_witness.borrow().contains_key(&value_v.key) {
                self.key_values_witness
                    .borrow_mut()
                    .insert(value_v.key.clone(), value_v.value.clone());
            }
            // resolve possible key collision
            if value_v.key.as_slice() != k {
                return None;
            }
        }
        v
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
        self.tree_witness
            .record_witness_access(&self.state.tree, ix);
        self.state.set(k, v)
    }

    pub fn root(&self) -> &Hash {
        self.tree_witness.root(&self.state.tree)
    }

    fn init_from_witness(
        witness_hashes: &[(TreeIndex, Hash)],
        witness_key_values: Vec<(Vec<u8>, V)>,
    ) -> Option<Self> {
        let token_ledger_state_v2::merkle::StateTree { tree, key_values } =
            token_ledger_state_v2::merkle::StateTree::init_from_witness(
                witness_hashes,
                witness_key_values,
            )?;

        Some(Self {
            tree_witness: TreeWitness {
                initial_hashes: tree.hashes.clone(),
                witness: Default::default(),
            },
            key_values_witness: Default::default(),
            state: token_ledger_state_v2::merkle::StateTree::<V> { key_values, tree },
        })
    }

    fn take_witness(&mut self) -> (BTreeMap<TreeIndex, Hash>, BTreeMap<Vec<u8>, V>) {
        let hashes = std::mem::take(self.tree_witness.witness.get_mut());
        // update for next run
        self.tree_witness.initial_hashes = self.state.tree.hashes.clone();

        let values = std::mem::take(self.key_values_witness.get_mut());

        (hashes, values)
    }
}

#[derive(Default)]
pub struct KnownTokens {
    merkle: token_ledger_state_v2::merkle::KnownTokens,
    witness: Vec<TokenId>,
}

impl KnownTokens {
    fn from_stream<R: codec::Input>(buf_reader: &mut R) -> Self {
        let mut result = Self::default();
        result.merkle.token_ids = Decode::decode(buf_reader).unwrap();
        result.witness = result.merkle.token_ids.clone();
        result
    }

    fn serialize<W: Write>(&mut self, w: &mut W) {
        let encoded = self.merkle.token_ids.encode();
        dbg!("serializing {} known token bytes", encoded.len());
        w.write_all(encoded.as_slice()).unwrap();
    }

    fn take_witness(&mut self) -> Vec<TokenId> {
        std::mem::replace(&mut self.witness, self.merkle.token_ids.clone())
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
        assert_eq!(TREE_DEPTH + 1, std::mem::size_of::<TreeIndex>() * 8);
    }
    #[test]
    fn create_token_and_distribute() {
        let mut state: State = Default::default();
        assert_eq!([0; 32], state.get_root());

        state.set_balance([1; 32], 1, 10);

        assert_ne!([0; 32], state.get_root());

        let witness_1 = state.take_witness();
        let mut state2 = State::from_witness(witness_1).unwrap();
        state2.set_balance([1; 32], 1, 10);
        assert_eq!(state.get_root(), state2.get_root());

        assert_eq!(Some(10), state.get_balance([1; 32], 1));
        // get balance witness
        let witness_2 = state.take_witness();
        let state3 = State::from_witness(witness_2).unwrap();
        assert_eq!(state.get_root(), state3.get_root());
        assert_eq!(Some(10), state3.get_balance([1; 32], 1));

        // insert token and insert another value
        state.set_balance([8; 32], 2, 32);
        state.known_tokens_push(1);
        state.known_tokens_push(2);
        let witness_3 = state.take_witness();

        let mut state4 = State::from_witness(witness_3).unwrap();
        assert_eq!(state3.get_root(), state4.get_root());
        state4.set_balance([8; 32], 2, 32);
        state4.known_tokens_push(1);
        state4.known_tokens_push(2);
        assert_eq!(state.get_root(), state4.get_root());

        // test serialize
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.keep();
        state.set_new_persist_file(dir_path.clone());
        let root_bef_ser = state.get_root();
        core::mem::drop(state);

        let state_from_ser = State::from_db_path(dir_path, None);

        assert_eq!(root_bef_ser, state_from_ser.get_root());
    }
}

#![no_std]

extern crate alloc;

// An auxiliary module for handling JSON-encoded data.
pub mod json;

// using consensus to avoid importing jam-std-common (quite heay import TODO feature gate it a
// bit?).
use blake2b_simd::Params;
pub use ed25519_consensus::{SigningKey, VerificationKey, VerificationKeyBytes};
use jam_types::Hash;

use codec::{Decode, Encode};

/// Operations that can be submitted to the token ledger
#[derive(Clone, Debug, Encode, Decode)]
pub enum Operation {
    Mint {
        to: AccountId,
        token_id: TokenId,
        amount: u64,
    },
    Transfer {
        from: AccountId,
        to: AccountId,
        token_id: TokenId,
        amount: u64,
    },
    Solicit(Solicit),
}

/// Solicit a preimage for the service.
/// Is expected to be applied on a given state root.
#[derive(Clone, Debug, Encode, Decode)]
pub struct Solicit {
    pub on_root: Hash,
    pub hash: Hash,
    pub len: u64,
}

/// A Refinement Operation with its authorization signature
/// For this tutorial, we will define this authorization as a signature by the hard-coded admin
/// account, covering the encoded operation.
/// In real-world cases, developers might specify a specific format to the signature message.
#[derive(Clone, Debug, Encode, Decode)]
pub struct SignedOperation {
    pub operation: Operation,
    pub signature: Signature,
}

pub fn verify_signature(
    op: &Operation,
    signature: &Signature,
    key: VerificationKey,
) -> Result<(), &'static str> {
    let message = op.signing_message().encode();
    key.verify(&signature.0, &message).map_err(|_| {
        jam_pvm_common::info!(
            "Signature verification failed for message: {:?} and key {:?}",
            hex::encode(&message),
            hex::encode(key.as_bytes())
        );
        "Signature verification failed for message"
    })
}

impl Operation {
    /// Encode the operation as bytes for signing
    pub fn signing_message(&self) -> Hash {
        match self {
            Operation::Mint {
                to,
                token_id,
                amount,
            } => {
                let mut raw = [0u8; 72];
                raw[0..32].copy_from_slice(to);
                raw[32..36].copy_from_slice(&token_id.to_le_bytes());
                raw[36..44].copy_from_slice(&amount.to_le_bytes());

                let mut hasher = Params::new().hash_length(32).to_state();
                hasher.update(&raw);
                let mut out = [0u8; 32];
                out.copy_from_slice(&hasher.finalize().as_bytes()[0..32]);
                out
            }
            Operation::Transfer {
                from,
                to,
                token_id,
                amount,
            } => {
                let mut raw = [0u8; 80];
                raw[0..32].copy_from_slice(from);
                raw[32..64].copy_from_slice(to);
                raw[64..68].copy_from_slice(&token_id.to_le_bytes());
                raw[68..76].copy_from_slice(&amount.to_le_bytes());

                let mut hasher = Params::new().hash_length(32).to_state();
                hasher.update(&raw);
                let mut out = [0u8; 32];
                out.copy_from_slice(&hasher.finalize().as_bytes()[0..32]);
                out
            }
            Operation::Solicit(solicit) => {
                let mut raw = [0u8; 72];
                raw[0..32].copy_from_slice(&solicit.on_root);
                raw[32..64].copy_from_slice(&solicit.hash);
                raw[64..72].copy_from_slice(&solicit.len.to_le_bytes());

                let mut hasher = Params::new().hash_length(32).to_state();
                hasher.update(&raw);
                let mut out = [0u8; 32];
                out.copy_from_slice(&hasher.finalize().as_bytes()[0..32]);
                out
            }
        }
    }
}

// Orders a transaction between two parties, so that for both possible directions of transfer,
// we always have the same party first, independently from being the sender or the recipient.
// This allows to keep track of the net balance between two parties over several transfers
// without having to keep track of the direction of each individual transfer.
pub fn canonical_transfer(
    from: AccountId,
    to: AccountId,
    token_id: TokenId,
    amount: u64,
) -> ((TokenId, Counterparts), i64) {
    let a = &from;
    let b = &to;
    let (counterparts, amount) = if a < b {
        ((*a, *b), amount as i64)
    } else {
        ((*b, *a), -(amount as i64))
    };
    ((token_id, (counterparts.0, counterparts.1)), amount)
}

/// A keypair for signing
#[derive(Clone, Debug)]
pub struct Keypair {
    pub signing_key: SigningKey,
    pub public_key: VerificationKey,
}

impl Keypair {
    /// Create a keypair from a 32-byte seed
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from(*seed);
        let verification_key = signing_key.verification_key();
        Self {
            signing_key,
            public_key: verification_key,
        }
    }

    /// Sign a message
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }
}

/// Generate a deterministic keypair for testing (index-based)
pub fn generate_keypair(seed: u64) -> Keypair {
    let mut seed_bytes = [0u8; 32];
    seed_bytes[0..8].copy_from_slice(&seed.to_le_bytes());
    Keypair::from_seed(&seed_bytes)
}

/// For demonstration only: a hard-coded admin account that must authorise every operation submitted
/// to the service. In a real-world scenario, developers must decide on a proper authorization
/// layer, deciding who can authorize operations and how the service would manage their identities
/// and keys.
pub fn admin() -> VerificationKeyBytes {
    admin_keypair().public_key.to_bytes().into()
}

pub fn admin_keypair() -> Keypair {
    let mut seed_bytes = [0u8; 32];
    seed_bytes[0..5].copy_from_slice(b"admin");
    Keypair::from_seed(&seed_bytes)
}

/// A unique identifier for a token type
pub type TokenId = u32;

/// An account identifier (32-byte public key)
pub type AccountId = [u8; 32];

/// A set of two counterparts of a single transfer.
/// There is no implication of who the sender is, because we
/// want to accumulate several operations, in either direction,
/// between the same accounts.
/// The final net balance will determine the resultant transfer direction.
pub type Counterparts = (AccountId, AccountId);

// code from jam-std TODO feature gate std to only import these low overhead compile
// things.
pub const SIGNATURE_LEN: usize = 64;
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Signature(pub ed25519_consensus::Signature);

impl codec::Encode for Signature {
    fn encode_to<T: codec::Output + ?Sized>(&self, dest: &mut T) {
        self.0.to_bytes().encode_to(dest)
    }
    fn size_hint(&self) -> usize {
        self.0.to_bytes().size_hint()
    }
}
impl codec::MaxEncodedLen for Signature {
    fn max_encoded_len() -> usize {
        SIGNATURE_LEN
    }
}
impl codec::ConstEncodedLen for Signature {}
impl codec::Decode for Signature {
    fn decode<I: codec::Input>(input: &mut I) -> Result<Self, codec::Error> {
        Ok(Self(<[u8; SIGNATURE_LEN]>::decode(input)?.into()))
    }
}
impl core::fmt::Debug for Signature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Signature")
    }
}

/// Storage key for an account's token balance
/// Format: "bal:" || token_id (4 bytes LE) || account_id (32 bytes)
pub const BALANCE_KEY_SIZE: usize = 4 + 4 + 32;
pub fn balance_key(token_id: TokenId, account: &AccountId) -> [u8; BALANCE_KEY_SIZE] {
    const PREFIX_BALANCE: &[u8] = b"bal:";
    const PREFIX_LENGTH: usize = PREFIX_BALANCE.len();
    const TOKEN_LENGTH: usize = core::mem::size_of::<TokenId>();
    const ACCOUNT_LENGTH: usize = core::mem::size_of::<AccountId>();
    let mut key = [0u8; PREFIX_BALANCE.len() + TOKEN_LENGTH + ACCOUNT_LENGTH];

    key[..PREFIX_LENGTH].copy_from_slice(PREFIX_BALANCE);
    key[PREFIX_LENGTH..PREFIX_LENGTH + TOKEN_LENGTH].copy_from_slice(&token_id.to_le_bytes());
    key[PREFIX_LENGTH + TOKEN_LENGTH..].copy_from_slice(account);
    key
}

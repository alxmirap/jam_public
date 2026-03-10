use super::*;

use codec::Decode;
use ed25519_consensus::VerificationKey;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
enum PayloadJson {
    TransferBatch { old_root: String, new_root: String },
    Reset { new_root: String, signature: String },
}

#[test]
fn test_batch_workflow() {
    let mut state = BuilderState::default();
    state.init_accounts(3, 1000);

    let pk0 = state.accounts[&0].keypair.public_key;
    let pk1 = state.accounts[&1].keypair.public_key;
    let pk2 = state.accounts[&2].keypair.public_key;
    let init_root = state.current_root;

    state.queue_transfer(0, 1, 100).unwrap();
    state.queue_transfer(1, 2, 50).unwrap();
    state.queue_transfer(2, 0, 25).unwrap();

    let (payload_json, extrinsic) = state.prepare_transfers().unwrap();
    let transfers = Vec::<Transfer>::decode(&mut &extrinsic[..]).unwrap();
    assert_eq!(transfers.len(), 3);
    for transfer in &transfers {
        assert!(verify_transfer_signature(transfer));
    }

    let parsed = serde_json::from_slice::<PayloadJson>(&payload_json).unwrap();
    if let PayloadJson::TransferBatch { old_root, new_root } = parsed {
        assert_eq!(old_root, hex::encode(init_root));
        assert_ne!(new_root, hex::encode(init_root));
    } else {
        panic!("Expected transfer batch JSON payload");
    }

    assert_eq!(state.accounts[&0].balance, 925);
    assert_eq!(state.accounts[&1].balance, 1050);
    assert_eq!(state.accounts[&2].balance, 1025);
    assert_eq!(state.accounts[&0].nonce, 1);
    assert_eq!(state.accounts[&1].nonce, 1);
    assert_eq!(state.accounts[&2].nonce, 1);
    assert_eq!(state.accounts[&0].keypair.public_key, pk0);
    assert_eq!(state.accounts[&1].keypair.public_key, pk1);
    assert_eq!(state.accounts[&2].keypair.public_key, pk2);
}

#[test]
fn test_transfer_signatures() {
    let mut state = BuilderState::default();
    state.init_accounts(3, 1000);

    let pk0 = state.accounts[&0].keypair.public_key;
    let pk1 = state.accounts[&1].keypair.public_key;
    let pk2 = state.accounts[&2].keypair.public_key;

    state.queue_transfer(0, 1, 100).unwrap();
    state.queue_transfer(1, 2, 50).unwrap();

    let (_payload_json, extrinsic) = state.prepare_transfers().unwrap();
    let transfers = Vec::<Transfer>::decode(&mut &extrinsic[..]).unwrap();

    for transfer in transfers.iter() {
        assert!(verify_transfer_signature(transfer));
    }

    assert_eq!(state.accounts[&0].nonce, 1);
    assert_eq!(state.accounts[&1].nonce, 1);
    assert_eq!(state.accounts[&2].nonce, 0);
    assert_eq!(state.accounts[&0].keypair.public_key, pk0);
    assert_eq!(state.accounts[&1].keypair.public_key, pk1);
    assert_eq!(state.accounts[&2].keypair.public_key, pk2);
}

#[test]
fn test_reset_signature() {
    let mut state = BuilderState::default();
    state.init_accounts(2, 1000);

    let payload_json = state.prepare_reset().unwrap();
    let parsed = serde_json::from_slice::<PayloadJson>(&payload_json).unwrap();

    if let PayloadJson::Reset {
        new_root,
        signature,
    } = parsed
    {
        let admin = admin_keypair();
        let key = VerificationKey::try_from(admin.public_key).unwrap();
        let message = reset_signing_message(decode_fixed_hex::<32>(&new_root).unwrap());

        let sig_arr = decode_fixed_hex::<64>(&signature).unwrap();
        let sig = ed25519_consensus::Signature::from(sig_arr);
        key.verify(&sig, &message)
            .expect("admin signature should be valid");
    } else {
        panic!("Expected reset JSON payload");
    }
}

#[test]
fn test_unknown_recipient_is_registered() {
    let mut state = BuilderState::default();
    state.init_accounts(2, 1000);

    state.queue_transfer(0, 7, 25).unwrap();

    assert!(state.accounts.contains_key(&7));
    assert_eq!(state.accounts.len(), 3);
}

#[test]
fn test_counterpart_aggregation_nets_bidirectional_transfers() {
    let mut state = BuilderState::default();
    state.init_accounts(5, 1000);

    let pk0 = state.accounts[&0].keypair.public_key;
    let pk1 = state.accounts[&1].keypair.public_key;
    let pk2 = state.accounts[&2].keypair.public_key;
    let pk3 = state.accounts[&3].keypair.public_key;

    // First case: we can net multiple accounts between the same pair of counterparts
    state.queue_transfer(0, 1, 100).unwrap();
    state.queue_transfer(1, 0, 30).unwrap();
    state.queue_transfer(1, 0, 20).unwrap();
    state.queue_transfer(0, 1, 80).unwrap();

    let (_payload_json, extrinsic) = state.prepare_transfers().unwrap();
    let transfers = Vec::<Transfer>::decode(&mut &extrinsic[..]).unwrap();

    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0].amount, 130);
    assert_eq!(transfers[0].from, state.accounts[&0].keypair.public_key);
    assert_eq!(transfers[0].to, state.accounts[&1].keypair.public_key);
    assert_eq!(state.accounts[&0].nonce, 1);
    assert_eq!(state.accounts[&1].nonce, 0);

    // We correctly detect the flow of the net balance.
    // First: A sends...
    state.queue_transfer(2, 3, 100).unwrap();
    state.queue_transfer(2, 3, 100).unwrap();

    let (_payload_json, extrinsic) = state.prepare_transfers().unwrap();
    let transfers = Vec::<Transfer>::decode(&mut &extrinsic[..]).unwrap();

    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0].amount, 200);
    assert_eq!(transfers[0].from, state.accounts[&2].keypair.public_key);
    assert_eq!(transfers[0].to, state.accounts[&3].keypair.public_key);
    assert_eq!(state.accounts[&2].nonce, 1);
    assert_eq!(state.accounts[&3].nonce, 0);

    // Now: B sends...
    state.queue_transfer(3, 2, 100).unwrap();
    state.queue_transfer(3, 2, 100).unwrap();

    let (_payload_json, extrinsic) = state.prepare_transfers().unwrap();
    let transfers = Vec::<Transfer>::decode(&mut &extrinsic[..]).unwrap();

    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0].amount, 200);
    assert_eq!(transfers[0].from, state.accounts[&3].keypair.public_key);
    assert_eq!(transfers[0].to, state.accounts[&2].keypair.public_key);
    assert_eq!(state.accounts[&2].nonce, 1);
    assert_eq!(state.accounts[&3].nonce, 1);
    assert_eq!(state.accounts[&0].keypair.public_key, pk0);
    assert_eq!(state.accounts[&1].keypair.public_key, pk1);
    assert_eq!(state.accounts[&2].keypair.public_key, pk2);
    assert_eq!(state.accounts[&3].keypair.public_key, pk3);
}

#[test]
fn test_failed_transfer_insufficient_balance_nonce_unchanged() {
    let mut state = BuilderState::default();
    state.init_accounts(2, 1000);

    let nonce0_before = state.accounts[&0].nonce;
    let nonce1_before = state.accounts[&1].nonce;

    state.queue_transfer(0, 1, 2_000).unwrap();

    let err = state.prepare_transfers().unwrap_err();
    assert!(err.to_string().contains("Insufficient balance"));

    assert_eq!(state.accounts[&0].nonce, nonce0_before);
    assert_eq!(state.accounts[&1].nonce, nonce1_before);
    assert_eq!(state.accounts[&0].balance, 1000);
    assert_eq!(state.accounts[&1].balance, 1000);
}

#[test]
fn test_remove_pair_clears_pending_for_counterparts() {
    let mut state = BuilderState::default();
    state.init_accounts(3, 1000);

    state.queue_transfer(0, 1, 40).unwrap();
    state.queue_transfer(1, 2, 10).unwrap();

    state.remove_pending_pair(1, 0).unwrap();

    assert!(!state.pending_net_transfers.contains_key(&(0, 1)));
    assert!(state.pending_net_transfers.contains_key(&(1, 2)));
}

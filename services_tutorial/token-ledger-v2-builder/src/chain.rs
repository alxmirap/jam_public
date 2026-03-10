use super::*;

impl BuilderState {
    /// Apply pending net transfers, compute new root, and send-ready data (payload_json, extrinsic_scale)
    pub fn prepare_transfers(&mut self) -> Result<(Vec<u8>, Vec<u8>)> {
        if self.pending_net_transfers.is_empty() {
            return Err(anyhow!("No pending transfers"));
        }

        let pending_net = std::mem::take(&mut self.pending_net_transfers);
        let old_root = self.current_root;
        let mut transfers: Vec<Transfer> = Vec::new();

        for ((a_idx, b_idx), net) in pending_net {
            if net == 0 {
                continue;
            }

            let (from_idx, to_idx, amount) = if net > 0 {
                (a_idx, b_idx, net as u64)
            } else {
                (b_idx, a_idx, (-net) as u64)
            };

            let (from_keypair, from_pub, nonce) = {
                let from_state = self
                    .accounts
                    .get_mut(&from_idx)
                    .ok_or_else(|| anyhow!("Sender account index not found: {}", from_idx))?;

                let nonce = from_state.nonce;
                let from_balance_before = from_state.balance;
                from_state.balance = from_state.balance.checked_sub(amount).ok_or_else(|| {
                    let sender_hex = hex::encode(from_state.keypair.public_key);
                    anyhow!(
                        "Insufficient balance: sender [{}] {} has {} but needs {}",
                        from_idx,
                        &sender_hex[..16],
                        from_balance_before,
                        amount
                    )
                })?;
                from_state.nonce += 1;

                (
                    from_state.keypair.clone(),
                    from_state.keypair.public_key,
                    nonce,
                )
            };

            let to_pub = {
                let to_state = self
                    .accounts
                    .get_mut(&to_idx)
                    .ok_or_else(|| anyhow!("Recipient account index not found: {}", to_idx))?;
                to_state.balance = to_state.balance.saturating_add(amount);
                to_state.keypair.public_key
            };

            let signature = sign_transfer_message(&from_keypair, &from_pub, &to_pub, nonce, amount);
            transfers.push(Transfer {
                from: from_pub,
                to: to_pub,
                amount,
                nonce,
                signature,
            });
        }

        if transfers.is_empty() {
            return Err(anyhow!("No net transfers after aggregation"));
        }

        let new_root = self.compute_batch_root(&transfers);
        self.current_root = new_root;

        let extrinsic_data = transfers.encode();
        println!(
            "Encoding {} transfers for submission. Total length: {}",
            transfers.len(),
            extrinsic_data.len()
        );
        let payload = Payload::TransferBatch { old_root, new_root };
        let payload_json = json::encode_payload_json(&payload)?;

        println!(
            "Prepared batch: {} transfers, root {} -> {}",
            transfers.len(),
            &hex::encode(old_root)[..8],
            &hex::encode(new_root)[..8]
        );

        // Ok((payload_json, extrinsic_data))
        Ok((payload_json, extrinsic_data))
    }

    /// Create a reset payload (signed by admin)
    pub fn prepare_reset(&mut self) -> Result<Vec<u8>> {
        let admin = admin_keypair();
        let message = reset_signing_message(self.current_root);
        let signature = admin.sign(&message);

        println!(
            "Reset signed by admin: {}",
            &hex::encode(admin.public_key)[..16]
        );

        let payload = Payload::Reset {
            new_root: self.current_root,
            signature,
        };

        json::encode_payload_json(&payload)
    }
}

pub(crate) fn sign_transfer_message(
    keypair: &Keypair,
    from: &AccountId,
    to: &AccountId,
    nonce: u64,
    amount: u64,
) -> [u8; 64] {
    let message = transfer_signing_message(from, to, nonce, amount);
    let signature = keypair.sign(&message);

    let key = VerificationKey::try_from(*from).expect("should be a valid public key");
    let signature_obj = Signature::from(signature);
    let verify_ok = key.verify(&signature_obj, &message).is_ok();
    assert!(
        verify_ok,
        "builder self-check: transfer signature did not verify"
    );

    signature
}

/// Submit a work package to the chain. A package is submitted to cores in order,
/// if the submission fails, until one succeeds or it returns an error.
pub(crate) async fn submit_to_chain(
    state: &mut BuilderState,
    payload_json: &[u8],
    extrinsic_data: &[u8],
    export_count: u16,
) -> Result<String> {
    let node = state
        .rpc_client
        .as_ref()
        .ok_or_else(|| anyhow!("RPC client not configured"))?;

    let params = node.parameters().await?;
    let core_count = match params {
        VersionedParameters::V1(p) => p.core_count,
    };

    if core_count == 0 {
        return Err(anyhow!("Chain reports core_count=0"));
    }

    let mut errors: Vec<String> = Vec::new();
    for core in 0..core_count {
        match submit_to_chain_on_core(state, payload_json, extrinsic_data, core, export_count).await
        {
            Ok((msg, package_hash)) => {
                state.insert_package(package_hash);
                return Ok(format!(
                    "Selected core {} and updated default core.\n{}",
                    core, msg
                ));
            }
            Err(error) => errors.push(format!("core {}: {}", core, error)),
        }
    }

    Err(anyhow!(
        "Submission failed on all {} cores.\n{}",
        core_count,
        errors.join("\n")
    ))
}

pub(crate) async fn submit_to_chain_on_core(
    state: &BuilderState,
    payload_json: &[u8],
    extrinsic_data: &[u8],
    core_idx: CoreIndex,
    export_count: u16,
) -> Result<(String, WorkPackageHash)> {
    if let Some(service_id) = state.service_id {
        let node = state
            .rpc_client
            .as_ref()
            .ok_or_else(|| anyhow!("RPC client not configured"))?;

        let best = node.best_block().await?;
        let state_root = node.state_root(best.header_hash).await?;
        let beefy_root = node.beefy_root(best.header_hash).await?;
        let finalized = node.finalized_block().await?;

        let service = node
            .service_data(best.header_hash, service_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "Service {service_id} not found at anchor {:?}",
                    best.header_hash
                )
            })?;

        let service_code_preimage_available = node
            .service_preimage(best.header_hash, service_id, service.code_hash.0)
            .await?
            .is_some();

        let null_authorizer_hash: jam_types::CodeHash =
            hash_raw(jam_null_authorizer_bin::BLOB).into();
        let auth_code_preimage_available = node
            .service_preimage(
                best.header_hash,
                BOOTSTRAP_SERVICE_ID,
                null_authorizer_hash.0,
            )
            .await?
            .is_some();

        if !service_code_preimage_available || !auth_code_preimage_available {
            return Err(anyhow!(
                "Preflight failed before submit: code preimage missing. service_preimage_available={}, authorizer_preimage_available={}\nservice={:08x}, service_code_hash={}, auth_code_host={:08x}, null_authorizer_hash={}, anchor={:?}\nHint: this commonly happens when targeting externally deployed services whose code preimage is not available to this node.",
                service_code_preimage_available,
                auth_code_preimage_available,
                service_id,
                hex::encode(service.code_hash.0),
                BOOTSTRAP_SERVICE_ID,
                hex::encode(null_authorizer_hash.0),
                best.header_hash
            ));
        }

        let extrinsic_hash = hash_raw(extrinsic_data).into();
        let extrinsic_specs = vec![ExtrinsicSpec {
            hash: extrinsic_hash,
            len: extrinsic_data.len() as u32,
        }]
        .try_into()
        .map_err(|_| anyhow!("Too many extrinsics in work item"))?;
        let extrinsics = vec![Bytes::copy_from_slice(extrinsic_data)];

        let expected_export_count: u16 = if extrinsic_data.len() <= SEGMENT_LEN {
            export_count
        } else {
            0
        };

        let item = WorkItem {
            service: service_id,
            code_hash: service.code_hash,
            payload: payload_json.to_vec().into(),
            refine_gas_limit: max_refine_gas(),
            accumulate_gas_limit: max_accumulate_gas(),
            import_segments: Default::default(),
            extrinsics: extrinsic_specs,
            export_count: expected_export_count,
        };

        let package = WorkPackage {
            authorization: Authorization::new(),
            auth_code_host: BOOTSTRAP_SERVICE_ID,
            authorizer: Authorizer {
                code_hash: null_authorizer_hash,
                config: AuthConfig::new(),
            },
            context: RefineContext {
                anchor: best.header_hash,
                state_root,
                beefy_root,
                lookup_anchor: finalized.header_hash,
                lookup_anchor_slot: finalized.slot,
                prerequisites: Default::default(),
            },
            items: vec![item]
                .try_into()
                .map_err(|_| anyhow!("Too many work items in package"))?,
        };

        let package_hash: WorkPackageHash = hash_raw(&package.encode()).into();

        if let Err(error) = node
            .submit_work_package(core_idx, &package, &extrinsics)
            .await
        {
            return Err(anyhow!(
                "submit_work_package failed: {}\nHint: this often means no reachable guarantor for the selected core/anchor, authorizer mismatch, or package validation rejection.",
                error,
            ));
        }

        let mut status_msg = String::from("submitted (no status update yet)");

        if let Ok(mut sub) = node
            .subscribe_work_package_status(package_hash, best.header_hash, false)
            .await
        {
            for _ in 0..20 {
                match sub.next().await {
                    Some(Ok(update)) => {
                        status_msg = format!("slot {}: {:?}", update.slot, update.value);
                        if matches!(
                            update.value,
                            WorkPackageStatus::Ready { .. } | WorkPackageStatus::Failed(_)
                        ) {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        status_msg = format!("status subscription error: {error}");
                        break;
                    }
                    None => break,
                }
            }
        }
        Ok((
            format!(
                "WorkPackage submitted:\n  - Service: {:08x}\n  - Core: {}\n  - Anchor: {:?}\n  - Payload: {} bytes\n  - Extrinsic: {} bytes\n  - Hash: {}\n  - Status: {}",
                service_id,
                core_idx,
                best.header_hash,
                payload_json.len(),
                extrinsic_data.len(),
                hex::encode(package_hash.0),
                status_msg
            ),
            package_hash,
        ))
    } else {
        Err(anyhow!("Missing service_id"))
    }
}

pub(crate) async fn fetch_chain_state_root(state: &BuilderState) -> Result<StateRoot> {
    let service_id = state
        .service_id
        .ok_or_else(|| anyhow!("Service not configured. Use: service <hex_id>"))?;

    let node = state
        .rpc_client
        .as_ref()
        .ok_or_else(|| anyhow!("RPC client not configured"))?;
    let best = node.best_block().await?;

    let maybe_root_bytes = node
        .service_value(best.header_hash, service_id, b"state_root")
        .await?;

    match maybe_root_bytes {
        Some(bytes) => StateRoot::decode(&mut &bytes[..])
            .map_err(|_| anyhow!("Failed to decode service state_root storage value")),
        None => Ok([0u8; 32]),
    }
}

pub(crate) async fn sync_builder_root_from_chain(state: &mut BuilderState) -> Result<()> {
    let service_id = state
        .service_id
        .ok_or_else(|| anyhow!("Service not configured. Use: service <hex_id>"))?;
    let chain_root = fetch_chain_state_root(state).await?;

    let local_root = state.current_root;
    state.current_root = chain_root;

    println!(
        "✅ Synced builder root from chain for service {:08x}: {} -> {}",
        service_id,
        hex::encode(local_root),
        hex::encode(chain_root)
    );

    Ok(())
}

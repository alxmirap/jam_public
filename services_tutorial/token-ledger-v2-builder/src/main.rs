//! Token Ledger V2 Builder/Client
//!
//! This is an off-chain component that:
//! - Maintains the full account state (balances)
//! - Accepts transfer requests from users
//! - Batches transfers and computes state transitions
//! - Submits WorkPackages to the JAM service via extrinsics
//!
//! It is a sort of "L2 sequencer" - it holds the actual data while
//! the on-chain service only stores a commitment (state root).

use crate::{accounts::*, chain::*, utils::*};
use anyhow::{Result, anyhow};
use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};
use bytes::Bytes;
use clap::Parser;
use codec::{Decode, Encode};
use colored::*;
use ed25519_consensus::{Signature, SigningKey, VerificationKey};
use futures::StreamExt;
use jam_std_common::{Node, NodeExt, VersionedParameters, WorkPackageStatus, hash_raw};
#[cfg(test)]
use jam_token_ledger_v2::decode_fixed_hex;
use jam_token_ledger_v2::{
    AccountId, Payload, StateRoot, Transfer, reset_signing_message, transfer_signing_message,
};
use jam_tooling::CommonArgs;
use jam_types::{
    AuthConfig, Authorization, Authorizer, CoreIndex, ExtrinsicSpec, RefineContext, SEGMENT_LEN,
    WorkItem, WorkPackage, WorkPackageHash, max_accumulate_gas, max_refine_gas,
};
use jsonrpsee::ws_client::{WsClient, WsClientBuilder};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

mod accounts;
mod chain;
mod json;
#[cfg(test)]
mod tests;
mod utils;

type Blake2b256 = Blake2b<U32>;
const DEFAULT_CORE_INDEX: CoreIndex = 0;
const BOOTSTRAP_SERVICE_ID: u32 = 0;

// CommonArgs allows for the specification of a custom RPC endpoint,
// that we can connect to on startup.
#[derive(Parser, Debug)]
#[command(
    name = "tl2-builder",
    about = "Interactive console for token-ledger-v2 builder"
)]
struct Args {
    #[command(flatten)]
    common: CommonArgs,
}

/// The Builder's local state
#[derive(Clone)]
pub struct BuilderState {
    /// All known account state (keypair + balance + nonce), keyed by account index
    pub accounts: BTreeMap<usize, AccountState>,
    /// Current state root (should match on-chain)
    pub current_root: StateRoot,
    /// Pending net transfer deltas keyed by sorted account index pair
    /// Positive value means pair.0 -> pair.1, negative means pair.1 -> pair.0.
    pub pending_net_transfers: BTreeMap<(usize, usize), i128>,
    /// RPC endpoint URL for node connection
    pub rpc_url: Option<String>,
    /// Service ID for the token-ledger-v2 service
    pub service_id: Option<u32>,
    /// List of work packages submitted by this builder since last reset
    pub submitted_packages: Vec<WorkPackageHash>,
    /// The current connection to the node. 
    /// Stored here to reuse the connection across multiple operations.
    pub rpc_client: Option<Arc<WsClient>>,
}

impl Default for BuilderState {
    fn default() -> Self {
        Self {
            accounts: BTreeMap::new(),
            current_root: [0u8; 32],
            pending_net_transfers: BTreeMap::new(),
            rpc_url: None,
            service_id: None,
            rpc_client: None,
            submitted_packages: Vec::new(),
        }
    }
}

impl BuilderState {
    pub fn insert_package(&mut self, package_hash: WorkPackageHash) {
        self.submitted_packages.push(package_hash);
    }

    pub fn clear_packages(&mut self) {
        self.submitted_packages.clear();
    }

    /// Compute state root from current balances
    /// Simple approach: hash of concatenated (account, balance) pairs
    /// Accounts are sorted due to BTreeMap ensuring ordering by key (AccountId)
    pub fn compute_state_root(&self) -> StateRoot {
        let mut hasher = Blake2b256::new();
        for state in self.accounts.values() {
            hasher.update(state.keypair.public_key);
            hasher.update(state.balance.to_le_bytes());
        }
        let result = hasher.finalize();
        let mut root = [0u8; 32];
        root.copy_from_slice(&result);
        root
    }

    /// Compute batch root: hash(old_root || encoded_transfers)
    /// Must match the service's computation exactly
    /// This is a simple placeholder computation - in a real implementation this might involve more complex logic (eg Merkle tree of transfers)
    /// Transfers are not sorted, but instead listed in the order they'll be executed.
    /// The same set of transfers in a different order would produce a different root,
    /// but that's acceptable because they can also generate different final state, if some transfers do not succeed due to insufficient balance.
    pub fn compute_batch_root(&self, transfers: &[Transfer]) -> StateRoot {
        let mut hasher = Blake2b256::new();
        hasher.update(self.current_root);
        hasher.update(transfers.encode());
        let result = hasher.finalize();
        let mut root = [0u8; 32];
        root.copy_from_slice(&result);
        root
    }

    /// Queue a transfer request. Transfers are aggregated by counterpart pair and netted.
    pub fn queue_transfer(
        &mut self,
        from_index: usize,
        to_index: usize,
        amount: u64,
    ) -> Result<()> {
        // Only send from accounts we know the full keypair for.
        let from_pub = self
            .accounts
            .get(&from_index)
            .ok_or_else(|| anyhow!("Unknown sender account index: {}", from_index))?
            .keypair
            .public_key;

        // We can send to unknown accounts; register with zero balance and deterministic keypair.
        if !self.accounts.contains_key(&to_index) {
            self.register_sender_account(to_index, 0);
        }

        let to_pub = self
            .accounts
            .get(&to_index)
            .ok_or_else(|| anyhow!("Unknown recipient account index: {}", to_index))?
            .keypair
            .public_key;

        if from_pub == to_pub {
            return Err(anyhow!(
                "Cannot transfer to self: sender [{}] {} and recipient [{}] {} are the same account",
                from_index,
                &hex::encode(from_pub)[..16],
                to_index,
                &hex::encode(to_pub)[..16]
            ));
        }
        if amount == 0 {
            return Err(anyhow!("Cannot transfer zero amount"));
        }

        let (a, b, delta) = canonical_counterpart_delta(from_index, to_index, amount);
        self.pending_net_transfers
            .entry((a, b))
            .and_modify(|e| *e += delta)
            .or_insert(delta);

        println!(
            "Queued transfer: {} -> {} amount {}",
            from_index, to_index, amount
        );
        Ok(())
    }

    /// Remove all pending net transfer between two counterpart accounts.
    pub fn remove_pending_pair(&mut self, index_a: usize, index_b: usize) -> Result<()> {
        if index_a == index_b {
            return Err(anyhow!("Cannot remove pair for same account index"));
        }

        let (a, b, _) = canonical_counterpart_delta(index_a, index_b, 0);
        let key = (a, b);

        if self.pending_net_transfers.remove(&key).is_none() {
            return Err(anyhow!(
                "No pending transfer pair found between {} and {}",
                index_a,
                index_b
            ));
        }

        println!("Removed pending transfer pair: {} <-> {}", index_a, index_b);
        Ok(())
    }

    /// Display current state
    pub fn show_state(&self, show_pending_details: bool) {
        println!("{}", "=== Builder State ===".bold());
        println!("Root: {}", hex::encode(self.current_root).yellow());
        if let Some(url) = &self.rpc_url {
            println!("RPC: {}", url.cyan());
        } else {
            println!("RPC: {}", "not connected".red());
        }
        if let Some(service_id) = self.service_id {
            println!("Service ID: {}", format!("{:08x}", service_id).cyan());
        } else {
            println!("Service ID: {}", "not set".red());
        }
        println!("Accounts: {}", self.accounts.len());
        for (index, state) in &self.accounts {
            println!(
                "  [{}] {}: {} (nonce {})",
                index,
                &hex::encode(state.keypair.public_key)[..16],
                state.balance.to_string().green(),
                state.nonce
            );
        }
        println!(
            "Number of packages since last reset: {}",
            self.submitted_packages.len()
        );
        println!(
            "Pending transfer pairs: {}",
            self.pending_net_transfers.len()
        );
        if show_pending_details {
            if self.pending_net_transfers.is_empty() {
                println!("Pending transfers: none");
                return;
            }

            println!("Pending transfers:");
            for (&(a, b), &net) in &self.pending_net_transfers {
                if net == 0 {
                    continue;
                }

                let (from_idx, to_idx, amount) = canonical_counterpart_delta(a, b, net);

                let from_pub = self
                    .accounts
                    .get(&from_idx)
                    .map(|s| s.keypair.public_key)
                    .unwrap_or([0u8; 32]);
                let to_pub = self
                    .accounts
                    .get(&to_idx)
                    .map(|s| s.keypair.public_key)
                    .unwrap_or([0u8; 32]);

                println!(
                    "  [{}] {} -> [{}] {} : {}",
                    from_idx,
                    &hex::encode(from_pub)[..16],
                    to_idx,
                    &hex::encode(to_pub)[..16],
                    amount
                );
            }
        }
    }
}

async fn connect_ws_node(rpc_url: &str) -> Result<Arc<WsClient>> {
    let normalized = if rpc_url.starts_with("ws://") || rpc_url.starts_with("wss://") {
        rpc_url.to_string()
    } else {
        format!("ws://{}", rpc_url)
    };

    let client = WsClientBuilder::default()
        .build(&normalized)
        .await
        .map_err(|e| anyhow!("Failed to connect to RPC endpoint {}: {}", normalized, e))?;

    let client = Arc::new(client);

    match client.parameters().await? {
        VersionedParameters::V1(params) => {
            params
                .apply()
                .map_err(|e| anyhow!("Failed to apply protocol parameters from node: {}", e))?;
        }
    }

    println!("Connected to RPC node at {}", format!("{:?}", normalized).red());
    Ok(client)
}

async fn print_state_with_node_info(state: &BuilderState, show_pending_details: bool) {
    state.show_state(show_pending_details);

    let Some(node) = &state.rpc_client else {
        println!("RPC client not configured, cannot fetch node info.");
        return;
    };

    let best = node.best_block().await;
    let finalized = node.finalized_block().await;

    match best {
        Ok(best) => println!(
            "Best block:      {:?} @ slot {}",
            best.header_hash, best.slot
        ),
        Err(error) => println!("⚠️  Could not fetch best block: {}", error),
    }

    match finalized {
        Ok(finalized) => println!(
            "Finalized block: {:?} @ slot {}",
            finalized.header_hash, finalized.slot
        ),
        Err(error) => println!("⚠️  Could not fetch finalized block: {}", error),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut state = BuilderState::default();

        match args.common.connect_rpc(DEFAULT_CORE_INDEX).await {
            Ok(node) => {
                let best_block = node.best_block().await?;
                state.rpc_url = Some(args.common.rpc.clone());
                println!(
                    "✅ Succeeded connecting to RPC node at {}. Best block: {}",
                    args.common.rpc,
                    format!("{} at slot {}", best_block.header_hash, best_block.slot).green()
                );
                state.rpc_client = Some(Arc::new(node));
            }
            Err(error) => {
                println!(
                    "⚠️  Startup RPC connection failed for {}: {}",
                    args.common.rpc, error
                );
                std::process::exit(1);
            }
        }

        run_console(state).await
    })
}

async fn run_console(mut state: BuilderState) -> Result<()> {
    println!("🚀 Token Ledger V2 Builder console ready");
    println!("Type 'help' for commands, 'quit' to exit.");

    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut line = String::new();

    loop {
        print!("tl2> ");
        io::stdout().flush()?;

        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            println!();
            break;
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        let parts: Vec<&str> = input.split_whitespace().collect();
        match parts[0] {
            "help" | "h" => print_help(),
            "quit" | "exit" | "q" => break,
            "state" | "s" => {
                let show_pending_details = parts
                    .get(1)
                    .map(|v| {
                        matches!(
                            v.to_ascii_lowercase().as_str(),
                            "full" | "pending"
                        )
                    })
                    .unwrap_or(false);
                print_state_with_node_info(&state, show_pending_details).await;
            }
            "service" | "sv" => {
                if let Some(id_str) = parts.get(1) {
                    match parse_service_id_hex(id_str) {
                        Ok(id) => {
                            state.service_id = Some(id);
                            println!("✅ Service ID set to: {:08x}", id);
                        }
                        Err(e) => println!("❌ {e}"),
                    }
                } else {
                    println!("Usage: service <hex_id> (example: a2db1bf9 or 0xa2db1bf9)");
                }
            }
            "init" | "i" => {
                let accounts = parts
                    .get(1)
                    .map(|s| s.parse::<usize>())
                    .transpose()
                    .map_err(|_| anyhow!("Invalid accounts value, expected usize"))?
                    .unwrap_or(5);
                let balance = parts
                    .get(2)
                    .map(|s| s.parse::<u64>())
                    .transpose()
                    .map_err(|_| anyhow!("Invalid balance value, expected u64"))?
                    .unwrap_or(1_000_000);

                state.init_accounts(accounts, balance);

                if state.rpc_client.is_some() && state.service_id.is_some() {
                    let mut tentative_state = state.clone();
                    match tentative_state.prepare_reset() {
                        Ok(payload_json) => {
                            match submit_to_chain(&mut tentative_state, &payload_json, &[], 0).await
                            {
                                Ok(msg) => {
                                    state = tentative_state;
                                    state.clear_packages();
                                    println!("✅ Init reset submitted: {}", msg);
                                }
                                Err(error) => {
                                    println!("❌ Init reset submission failed: {}", error);
                                    println!(
                                        "ℹ️  Local state remains initialized; chain state unchanged."
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            println!("❌ Failed to prepare init reset payload: {}", error)
                        }
                    }
                } else {
                    println!("ℹ️  Init reset not submitted (set both rpc and service first)");
                }

                print_state_with_node_info(&state, false).await;
            }
            "transfer" | "t" => {
                if parts.len() != 4 {
                    println!("Usage: transfer <from_index> <to_index> <amount>");
                    continue;
                }
                let from = match parts[1].parse::<usize>() {
                    Ok(v) => v,
                    Err(_) => {
                        println!("Invalid from_index, expected usize");
                        continue;
                    }
                };
                let to = match parts[2].parse::<usize>() {
                    Ok(v) => v,
                    Err(_) => {
                        println!("Invalid to_index, expected usize");
                        continue;
                    }
                };
                let amount = match parts[3].parse::<u64>() {
                    Ok(v) => v,
                    Err(_) => {
                        println!("Invalid amount, expected u64");
                        continue;
                    }
                };

                if let Err(e) = state.queue_transfer(from, to, amount) {
                    println!("❌ {e}");
                }
            }
            "remove-pair" | "remove_pair" | "rp" | "wipe-pair" | "wipe_pair" | "wp" => {
                if parts.len() != 3 {
                    println!("Usage: wipe-pair <sender_index> <receiver_index>");
                    continue;
                }

                let a = match parts[1].parse::<usize>() {
                    Ok(v) => v,
                    Err(_) => {
                        println!("Invalid index_a, expected usize");
                        continue;
                    }
                };
                let b = match parts[2].parse::<usize>() {
                    Ok(v) => v,
                    Err(_) => {
                        println!("Invalid index_b, expected usize");
                        continue;
                    }
                };

                if let Err(e) = state.remove_pending_pair(a, b) {
                    println!("❌ {e}");
                }
            }
            "send-batch" | "send_batch" | "sb" => {
                let mut tentative_state = state.clone();

                match tentative_state.prepare_transfers() {
                    Ok((payload_json, extrinsic)) => {
                        println!("\n{}", "=== Batch Ready ===".bold());
                        println!("Payload (JSON): {} bytes", payload_json.len());
                        println!("Extrinsic (SCALE): {} bytes", extrinsic.len());
                        println!(
                            "\nPayload JSON:\n{}",
                            String::from_utf8_lossy(&payload_json)
                        );
                        println!("\nPayload hex:\n{}", hex::encode(&payload_json));
                        println!("\nExtrinsic hex:\n{}", hex::encode(&extrinsic));

                        if tentative_state.rpc_client.is_some() && tentative_state.service_id.is_some()
                        {
                            match submit_to_chain(
                                &mut tentative_state,
                                &payload_json,
                                &extrinsic,
                                1,
                            )
                            .await
                            {
                                Ok(msg) => {
                                    state = tentative_state;
                                    println!("✅ Submitted to chain: {}", msg);
                                }
                                Err(e) => {
                                    println!("❌ Submission failed: {}", e);
                                    println!(
                                        "ℹ️  Local state unchanged; pending transfers preserved."
                                    );
                                }
                            }
                        } else {
                            println!(
                                "\n⚠️  Batch prepared but not submitted (missing rpc/service config)"
                            );
                            println!("ℹ️  Local state unchanged; pending transfers preserved.");
                        }
                    }
                    Err(e) => println!("❌ {e}"),
                }
            }
            "reset" | "send-reset" | "sr" => {
                let mut tentative_state = state.clone();

                match tentative_state.prepare_reset() {
                    Ok(payload_json) => {
                        println!("\n{}", "=== Reset Ready ===".bold());
                        println!("Payload (JSON): {} bytes", payload_json.len());
                        println!("Payload JSON:\n{}", String::from_utf8_lossy(&payload_json));

                        if tentative_state.rpc_client.is_some() && tentative_state.service_id.is_some()
                        {
                            match submit_to_chain(&mut tentative_state, &payload_json, &[], 0).await
                            {
                                Ok(msg) => {
                                    state = tentative_state;
                                    state.clear_packages();
                                    println!("✅ Reset submitted: {}", msg);
                                }
                                Err(e) => println!("❌ Reset submission failed: {}", e),
                            }
                        } else {
                            println!(
                                "⚠️  Reset prepared but not submitted (missing rpc/service config)"
                            );
                        }
                    }
                    Err(e) => println!("❌ Failed to prepare reset payload: {}", e),
                }
            }
            "last-transfers" | "lt" => {
                if state.rpc_client.is_none() || state.service_id.is_none() {
                    println!("RPC and service ID must be set.");
                    continue;
                }

                let node = state.rpc_client.as_ref().unwrap();

                // Fetch exported segments for this service
                let mut segments = Vec::new();
                for wp_hash in state.submitted_packages.iter() {
                    match node.fetch_work_package_segments(*wp_hash, vec![0]).await {
                        Ok(s) => {
                            println!("Fetched {} segments for package hash {}", s.len(), wp_hash);
                            segments.extend(s);
                        }
                        Err(e) => {
                            println!(
                                "Failed to list exported segments for package {wp_hash:?}: {e}"
                            );
                            continue;
                        }
                    };
                }
                if segments.is_empty() {
                    println!("No exported transfer batches found in D3L.");
                    continue;
                }
                println!("Found {} exported segments.", segments.len());
                let mut found = false;
                for data in segments {
                    // Try to decode as Vec<Transfer>
                    let transfers: Result<Vec<Transfer>, _> = Decode::decode(&mut &data[..]);
                    if let Ok(ts) = transfers {
                        for t in &ts {
                            println!(
                                "[Transfer] from: {} to: {} amount: {} nonce: {}",
                                hex::encode(t.from),
                                hex::encode(t.to),
                                t.amount,
                                t.nonce
                            );
                            found = true;
                        }
                    } else {
                        // Try to decode as TransferBatch payload
                        println!("Not able to decode transfers as Vec<Transfer>");
                    }
                }
                if !found {
                    println!("No transfers found since last reset.");
                }
            }
            _ => {
                println!("Unknown command: {}", parts[0]);
                println!("Type 'help' to see available commands.");
            }
        }
    }

    Ok(())
}

fn print_help() {
    println!(
        r#"Builder commands:
    init [accounts] [balance] | i          Initialize local state and submit Reset (if rpc+service set)
    state [full|pending|details] | s [...] Show current builder state
    rpc <url>                              Override RPC endpoint for chain submission
    service <hex_id>                       Set target service ID and sync root from chain
    transfer <from_index> <to_index> <amt> | t
                                           Queue a transfer
    reset | send-reset | sr                Submit admin-signed Reset(new_root=current builder root)
    wipe-pair <sender_index> <receiver_index> | wp
                                           Remove whole pending pair net transfer (either order)
    send-batch | send_batch | sb           Submit the pending transfers
    help | h                               Show this help
    quit | exit | q                        Leave the console
"#
    );
}

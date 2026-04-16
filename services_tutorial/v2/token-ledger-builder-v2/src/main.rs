//! client exposed operations.
//! - opening state from file and others.
//! - produce refinement payload from json.

use clap::{arg, command, value_parser};
use codec::Encode;
use std::path::Path;

use jam_std_common::{Node, NodeError, NodeExt, hash_raw};
use jam_tooling::CommonArgs;
use jam_types::{
    AuthConfig, Authorization, Authorizer, CodeHash, CoreIndex, ExtrinsicSpec, HeaderHash,
    RefineContext, Slot, VALS_PER_CORE, ValIndex, WorkItem, WorkPackage, WorkPackageHash,
    max_accumulate_gas, max_refine_gas, val_count,
};
use jsonrpsee::ws_client::WsClient;
use std::env;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use token_ledger_builder_v2::state::State;
use token_ledger_common::{Operation, Signature, SignedOperation, Solicit};
use token_ledger_service_v2::RefinePayload;
use token_ledger_state_v2::{Hash, merkle::Witness};

const BASE_NODE_PORT: u16 = 19800;
const DEFAULT_NODE_INDEX: ValIndex = 0;
const DEFAULT_CORE: CoreIndex = 0;
const BOOTSTRAP_SERVICE_ID: u32 = 0;

fn main() {
    // Assumes we are running against the polkajam-testnet,
    // in the tiny config
    let parameters = jam_types::ProtocolParameters::tiny();
    parameters
        .apply()
        .expect("Built-in parameters should always be valid; qed");

    println!(
        "Set tiny config. Val count: {}, Vals per core: {}, Core count: {}",
        val_count(),
        VALS_PER_CORE,
        val_count() / VALS_PER_CORE as CoreIndex
    );

    let matches = command!() // requires `cargo` feature
        .arg(
            arg!(
				[input]  "Input refinement json file"
            )
            .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            arg!(
               [output] "Output refinement payload file. Optional if --connect_rpc is used, otherwise required"
            )
            .required(false)
            .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            arg!(
                --preimage  "Use a preimage"
            )
            .required(false),
        )
        .arg(
            arg!(
                --segment  "Use a segment"
            )
            .required(false),
        )
        .arg(
            arg!(
                --"connect-rpc"  "Connect to a running RPC node. Submit work-packages directly to it instead of writing payload to file"
            )
            .required(false),
        )
        .arg(
            arg!(
                --service <SERVICE> "Required if --connect-rpc is used. Hex-string of the service ID (eg. 0x1234abcd)"
            )
            .required(false)
            .value_parser(value_parser!(String)),
        )
        .arg(
            arg!(
			    --head <String> "Starting state root hash for this state transition, if undefined, latest written state is used (referenced in HEAD file)" 
            )
        )
        .arg(
            arg!(
                --port <PORT> "Specify the port number for the RPC connection"
            )
            .value_parser(value_parser!(u16))
            .required(false),
        )
        .get_matches();

    let Some(input_path) = matches.get_one::<PathBuf>("input") else {
        println!("Missing input param");
        return;
    };
    println!("Input: {}", input_path.display());

    let connect_rpc = matches.get_flag("connect-rpc");

    let rpc_port = if connect_rpc {
        Some(
            matches
                .get_one::<u16>("port")
                .copied()
                .unwrap_or(BASE_NODE_PORT + DEFAULT_NODE_INDEX),
        )
    } else {
        None
    };

    let output_path = matches.get_one::<PathBuf>("output");
    if !connect_rpc && output_path.is_none() {
        println!("Missing output param, or use --connect_rpc to submit directly to a running node");
        return;
    }

    let mut opt_service: Option<u32> = None;
    if connect_rpc {
        if matches.get_one::<String>("service").is_none() {
            println!("Missing required --service param when using --connect_rpc");
            return;
        }

        opt_service = matches
            .get_one::<String>("service")
            .map(|s| parse_service_id_hex(s).unwrap());
    }

    let mut overload_head: Option<Hash> = None;
    if let Some(head_str) = matches.get_one::<String>("head") {
        let hash = hex::decode(head_str).unwrap();
        overload_head = Some(hash.try_into().unwrap());
    }

    let preimage_steps = matches.get_flag("preimage");
    let with_segments = matches.get_flag("segment");
    if preimage_steps && with_segments {
        println!(
            "Incompatible options selected: 'segment' and 'preimage' should not be specified together"
        );
        return;
    }
    let version = if preimage_steps {
        dbg!("Running preimage steps");
        token_ledger_state_v2::Mode::Preimage
    } else if with_segments {
        dbg!("Running segment steps");
        token_ledger_state_v2::Mode::Segment
    } else {
        dbg!("Running direct steps");
        token_ledger_state_v2::Mode::Direct
    };

    let operations = read_ops_from_file(input_path);

    let db_path = std::path::PathBuf::new();
    let witness = compute_transition_witness(&db_path, overload_head, &operations);

    let refine_payload = RefinePayload {
        version,
        operations,
        witness,
    };

    if let Some(output_path) = output_path {
        println!("Output: {}", output_path.display());

        // Create the output file. In direct mode, this is the end result.
        // In preimage mode, we use this to compute a hash, and then
        // include it as the corresponding pre-image to a Solicit operation.

        let output = export_direct_payload(output_path, &refine_payload);

        if preimage_steps {
            std::mem::drop(output);
            export_preimage_payload(output_path, db_path, overload_head, version);
        }
    } else {
        println!("No output file specified, skipping writing payload to file");
    }

    if connect_rpc {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(error) => {
                println!("⚠️  Failed to create Tokio runtime: {}", error);
                std::process::exit(1);
            }
        };

        let rpc_port = rpc_port.expect("Checked RPC port above");
        println!("Submitting to RPC node at port {rpc_port}...");
        match rt.block_on(submit_to_node(rpc_port, opt_service, refine_payload)) {
            Ok(_) => {
                println!("✅ RPC submission successful");
            }
            Err(error) => {
                println!("⚠️  RPC submission failed: {}", error);
                std::process::exit(1);
            }
        }
    }
}

pub(crate) fn parse_service_id_hex(input: &str) -> Option<u32> {
    println!("Parsing service ID from input: '{}'", input);
    let trimmed = input.trim();
    let hex_part = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    if hex_part.is_empty() {
        println!("Service ID cannot be empty");
        return None;
    }

    match u32::from_str_radix(hex_part, 16) {
        Ok(service_id) => Some(service_id),
        Err(e) => {
            println!("Invalid service ID hex '{}': {}", input, e);
            None
        }
    }
}

async fn submit_to_node(
    rpc_port: u16,
    service_id: Option<u32>,
    payload: RefinePayload,
) -> Result<(), NodeError> {
    let node = match connect_to_node(rpc_port).await {
        Ok(node) => node,
        Err(error) => {
            println!("⚠️  Error connecting to RPC node: {}", error);
            std::process::exit(1);
        }
    };

    println!("Connected to RPC node, submitting payload...");

    let (context, _anchor_slot) = create_refine_context(&node).await?;
    let anchor = context.anchor;

    let service_id = service_id.expect("Service ID is required to submit to RPC node");

    let service = match node
        .service_data(anchor, service_id)
        .await?
        .ok_or_else(|| println!("Service {service_id} not found at anchor {:?}", anchor))
    {
        Ok(service) => service,
        Err(_) => {
            println!("⚠️  Service {service_id} not found at anchor {:?}", anchor);
            std::process::exit(1);
        }
    };

    let (null_authorizer_hash, auth_code_preimage_available) =
        get_authorizer(&node, anchor).await?;

    let service_code_preimage_available = node
        .service_preimage(anchor, service_id, service.code_hash.0)
        .await?
        .is_some();

    println!(
        "Is authorizer available: {:?}",
        auth_code_preimage_available
    );
    if !service_code_preimage_available || !auth_code_preimage_available {
        println!(
            "Preflight failed before submit: code preimage missing. service_preimage_available={}, authorizer_preimage_available={}\nservice={:08x}, service_code_hash={}, auth_code_host={:08x}, null_authorizer_hash={}, anchor={:?}\nHint: this commonly happens when targeting externally deployed services whose code preimage is not available to this node.",
            service_code_preimage_available,
            auth_code_preimage_available,
            service_id,
            hex::encode(service.code_hash.0),
            BOOTSTRAP_SERVICE_ID,
            hex::encode(null_authorizer_hash.0),
            anchor
        );
        std::process::exit(1);
    }

    // We create an empty extrinsics list here, for demonstration purposes only.
    let extrinsic_data = &[];
    let _extrinsic_hash = hash_raw(extrinsic_data).into();
    let _extrinsic_specs = [ExtrinsicSpec {
        hash: _extrinsic_hash,
        len: extrinsic_data.len() as u32,
    }];

    let export_count = 0;

    println!("Created context for submission");

    let item = WorkItem {
        service: service_id,
        code_hash: service.code_hash,
        payload: payload.encode().into(),
        refine_gas_limit: max_refine_gas(),
        accumulate_gas_limit: max_accumulate_gas(),
        import_segments: Default::default(),
        // extrinsics: extrinsic_specs
        // .try_into()
        // .expect("We only have one extrinsic, so this should never fail");
        extrinsics: Default::default(),
        export_count,
    };
    println!("Created work item for submission without imports");

    let package = WorkPackage {
        authorization: Authorization::new(),
        auth_code_host: BOOTSTRAP_SERVICE_ID,
        authorizer: Authorizer {
            code_hash: null_authorizer_hash,
            config: AuthConfig::new(),
        },
        context,
        items: vec![item]
            .try_into()
            .expect("We only have one item, so this should never fail"),
    };

    println!("Created work package for submission");

    let encoded_package = package.encode();
    let package_hash: WorkPackageHash = hash_raw(&encoded_package).into();

    println!("Submitting work package");
    let max_core = val_count() / VALS_PER_CORE as CoreIndex;
    let mut core = DEFAULT_CORE;
    let submitted_core: Option<CoreIndex>;

    loop {
        match node
            .submit_encoded_work_package(core, encoded_package.clone().into(), &[])
            .await
        {
            Ok(_) => {
                submitted_core = Some(core);
                break;
            }
            Err(error) => {
                println!(
                    "submit_encoded_work_package to core {core}/{} failed: {}",
                    max_core, error
                );
                core = (core + 1) % max_core;
                if core == DEFAULT_CORE {
                    return Err(error);
                }
            }
        }
    }

    let submitted_core = submitted_core.expect("submitted core is set when submission succeeds");

    println!(
        "✅ Payload submitted successfully to service {service_id} on core {} with package hash {package_hash}",
        submitted_core
    );

    Ok(())
}

async fn create_refine_context(node: &WsClient) -> Result<(RefineContext, Slot), NodeError> {
    // Match the parent-based anchoring logic used by newer tooling.
    let finalized = node.finalized_block().await?;
    let lookup_anchor = node.parent(finalized.header_hash).await?;

    let best_block = node.best_block().await?;
    let parent = node.parent(best_block.header_hash).await?;
    let anchor = parent.header_hash;

    let state_root = node.state_root(anchor).await?;
    let beefy_root = node.beefy_root(anchor).await?;

    let context = RefineContext {
        anchor,
        state_root,
        beefy_root,
        lookup_anchor: lookup_anchor.header_hash,
        lookup_anchor_slot: lookup_anchor.slot,
        prerequisites: Default::default(),
    };

    Ok((context, parent.slot))
}

async fn connect_to_node(rpc_port: u16) -> Result<WsClient, NodeError> {
    let common_args = CommonArgs {
        rpc: format!("ws://localhost:{}", rpc_port).to_string(),
    };

    let node = match common_args.connect_rpc(DEFAULT_NODE_INDEX).await {
        Ok(node) => {
            let best_block = node.best_block().await?;
            println!(
                "✅ Succeeded connecting to RPC node at {}. Best block: {} at slot {}",
                common_args.rpc, best_block.header_hash, best_block.slot
            );
            node
        }
        Err(error) => {
            println!(
                "⚠️  Startup RPC connection failed for {}: {}",
                common_args.rpc, error
            );
            std::process::exit(1);
        }
    };

    Ok(node)
}

fn export_direct_payload(output_path: &PathBuf, refine_payload: &RefinePayload) -> File {
    let mut output = std::fs::File::create(output_path).unwrap();
    refine_payload.encode_to(&mut output);
    output
}

fn export_preimage_payload(
    output_path: &PathBuf,
    db_path: PathBuf,
    overload_head: Option<Hash>,
    version: token_ledger_state_v2::Mode,
) {
    println!("Processing with pre-image steps");

    let (hash, len) = compute_payload_hash(output_path);

    println!(
        "Preimage hash: {}. Preimage length: {}",
        hex::encode(hash),
        len
    );

    let mut state = State::from_db_path(db_path, overload_head);
    let operations: Vec<SignedOperation> = vec![SignedOperation {
        // Dummy, unchecked in tutorial
        signature: Signature([0; 64].into()),
        operation: Operation::Solicit(Solicit {
            on_root: state.get_root(),
            hash,
            len,
        }),
    }];

    let _ = token_ledger_state_v2::state_transition(&mut state, &operations, false);
    // only root as we only check right root for solicit
    let witness = state.take_witness();
    let solicit_payload = RefinePayload {
        version,
        operations,
        witness,
    };
    let mut prep_path = output_path.clone();
    prep_path.set_extension("prepare");
    let mut output = std::fs::File::create(&prep_path).unwrap();
    solicit_payload.encode_to(&mut output);
}

fn read_ops_from_file(path: &PathBuf) -> Vec<token_ledger_common::SignedOperation> {
    let mut input = std::fs::File::open(path).unwrap();
    let mut input_vec = Vec::new();
    input.read_to_end(&mut input_vec).unwrap();
    let operations =
        token_ledger_common::json::parse_signed_operations(input_vec.as_slice()).unwrap();
    dbg!(operations.len());
    operations
}

fn compute_transition_witness(
    db_path: &Path,
    overload_head: Option<Hash>,
    operations: &Vec<SignedOperation>,
) -> Witness {
    let mut opt_db = std::fs::OpenOptions::new();
    opt_db.read(true).write(true);
    let mut state = State::from_db_path(db_path.to_path_buf(), overload_head);

    println!("\nInitial root: {}", hex::encode(state.get_root()));
    let _ = token_ledger_state_v2::state_transition(&mut state, operations, false);
    let witness = state.take_witness();
    println!("Post execution root: {}", hex::encode(state.get_root()));
    // dbg!(&witness);
    print_debug(&witness);
    witness
}

fn compute_payload_hash(file_path: &PathBuf) -> (Hash, u64) {
    let mut payload_file = std::fs::File::open(file_path).unwrap();
    let mut data = Vec::new();
    payload_file.read_to_end(&mut data).unwrap();
    println!(
        "Read {} bytes from file {}",
        data.len(),
        file_path.display()
    );
    let hash_r = blake2b_simd::Params::new().hash_length(32).hash(&data);
    let mut hash: Hash = [0; 32];
    hash.copy_from_slice(hash_r.as_bytes());
    let len = data.len() as u64;
    (hash, len)
}

pub fn print_debug(witness: &Witness) {
    println!("Witness:");
    println!("  Hashes:");
    for (index, hash) in witness.hashes.iter() {
        println!("    {}: {}", index, hex::encode(hash));
    }
    println!("  Key value balances:");
    for (key, value) in witness.key_value_balances.iter() {
        println!("    {}: {}", hex::encode(key), value);
    }
    println!("  Token ids:");
    for token_id in witness.token_ids.iter() {
        println!("    {}", token_id);
    }
}

async fn get_authorizer(node: &WsClient, block: HeaderHash) -> Result<(CodeHash, bool), NodeError> {
    let null_authorizer_hash: CodeHash = hash_raw(jam_null_authorizer_bin::BLOB).into();
    let auth_code_preimage_available = node
        .service_preimage(block, BOOTSTRAP_SERVICE_ID, null_authorizer_hash.0)
        .await?
        .is_some();

    Ok((null_authorizer_hash, auth_code_preimage_available))
}

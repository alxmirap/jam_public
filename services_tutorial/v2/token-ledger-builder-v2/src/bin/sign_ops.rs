//! sign_ops: Reads unsigned operations, signs them, and writes signed operations to output.

use std::env;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use token_ledger_common::json::{
    SignedOperationJson, UnsignedOperationJson, parse_unsigned_operations,
};
use token_ledger_common::{Operation, admin_keypair, generate_keypair};

const HELP: &str = "Usage: sign_ops <input_unsigned_ops_file> <output_signed_ops_file>";

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 || &args[1] == "--help" {
        println!("{}", HELP);
        return;
    }

    let mut input = File::open(&args[1]).expect("Failed to open input file");
    let mut input_vec = Vec::new();
    input
        .read_to_end(&mut input_vec)
        .expect("Failed to read input file");

    let unsigned_ops = parse_unsigned_operations(input_vec.as_slice())
        .expect("Failed to parse unsigned operations");
    let mut signed_ops = Vec::new();

    for op in unsigned_ops {
        let signed_op = match op {
            UnsignedOperationJson::Mint {
                token_id,
                amount,
                to_seed,
            } => {
                let kp = admin_keypair();
                let to_kp = generate_keypair(to_seed);
                let operation = Operation::Mint {
                    to: to_kp.public_key.to_bytes(),
                    token_id,
                    amount,
                };

                println!(
                    "Signing Mint operation: to={}, token_id={}, amount={}",
                    hex::encode(to_kp.public_key.to_bytes()),
                    token_id,
                    amount
                );
                println!(
                    "Signing message {:?} and key {:?}",
                    hex::encode(operation.signing_message()),
                    hex::encode(kp.signing_key.to_bytes())
                );

                let signature = kp.signing_key.sign(&operation.signing_message());
                SignedOperationJson::Mint {
                    token_id,
                    amount,
                    to: hex::encode(to_kp.public_key.to_bytes()),
                    signature: hex::encode(signature.to_bytes()),
                }
            }
            UnsignedOperationJson::Transfer {
                token_id,
                amount,
                from_seed,
                to_seed,
            } => {
                let from_kp = generate_keypair(from_seed);
                let to_kp = generate_keypair(to_seed);
                let operation = Operation::Transfer {
                    from: from_kp.public_key.to_bytes(),
                    to: to_kp.public_key.to_bytes(),
                    token_id,
                    amount,
                };
                let signature = from_kp.signing_key.sign(&operation.signing_message());
                SignedOperationJson::Transfer {
                    from: hex::encode(from_kp.public_key.to_bytes()),
                    to: hex::encode(to_kp.public_key.to_bytes()),
                    token_id,
                    amount,
                    signature: hex::encode(signature.to_bytes()),
                }
            }
        };
        signed_ops.push(signed_op);
    }

    write_signed_operations(&signed_ops, &args[2]).expect("Failed to write signed operations");
    println!("Signed operations written to {}", &args[2]);
}

fn write_signed_operations(
    signed_ops: &[SignedOperationJson],
    output_path: &str,
) -> std::io::Result<()> {
    let mut output = File::create(output_path)?;
    let op_seq_json =
        serde_json::to_string_pretty(signed_ops).expect("Failed to serialize signed operations");
    writeln!(output, "{}", op_seq_json)?;
    Ok(())
}

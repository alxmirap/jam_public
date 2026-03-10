use super::*;
use alloc::format;
use alloc::string::String;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
enum PayloadJson {
    TransferBatch { old_root: String, new_root: String },
    Reset { new_root: String, signature: String },
}

pub fn parse_payload(json_bytes: &[u8]) -> Result<Payload, String> {
    info!("Parsing JSON payload of {} bytes", json_bytes.len());

    let payload = serde_json::from_slice::<PayloadJson>(json_bytes)
        .map_err(|e| format!("Failed to parse JSON payload: {e}"))?;

    match payload {
        PayloadJson::TransferBatch { old_root, new_root } => Ok(Payload::TransferBatch {
            old_root: decode_fixed_hex::<32>(&old_root)
                .map_err(|e| format!("Invalid old_root hex: {e}"))?,
            new_root: decode_fixed_hex::<32>(&new_root)
                .map_err(|e| format!("Invalid new_root hex: {e}"))?,
        }),
        PayloadJson::Reset {
            new_root,
            signature,
        } => Ok(Payload::Reset {
            new_root: decode_fixed_hex::<32>(&new_root)
                .map_err(|e| format!("Invalid new_root hex: {e}"))?,
            signature: decode_fixed_hex::<64>(&signature)
                .map_err(|e| format!("Invalid signature hex: {e}"))?,
        }),
    }
}

pub fn decode_fixed_hex<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let mut result = [0u8; N];
    hex::decode_to_slice(s, &mut result)
        .map_err(|e| format!("Failed to decode fixed-length hex ({N} bytes): {e}"))?;
    Ok(result)
}

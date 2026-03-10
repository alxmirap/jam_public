use super::*;

pub(crate) fn encode_payload_json(payload: &Payload) -> Result<Vec<u8>> {
    match payload {
        Payload::TransferBatch { old_root, new_root } => serde_json::to_vec(&json!({
            "TransferBatch": {
                "old_root": hex::encode(old_root),
                "new_root": hex::encode(new_root),
            }
        }))
        .map_err(|e| anyhow!("Failed to encode transfer batch payload as JSON: {e}")),

        Payload::Reset {
            new_root,
            signature,
        } => serde_json::to_vec(&json!({
            "Reset": {
                "new_root": hex::encode(new_root),
                "signature": hex::encode(signature),
            }
        }))
        .map_err(|e| anyhow!("Failed to encode reset payload as JSON: {e}")),
    }
}

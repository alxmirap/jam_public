use super::*;

pub(crate) fn parse_service_id_hex(input: &str) -> Result<u32> {
    let trimmed = input.trim();
    let hex_part = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    if hex_part.is_empty() {
        return Err(anyhow!("Service ID cannot be empty"));
    }

    u32::from_str_radix(hex_part, 16)
        .map_err(|e| anyhow!("Invalid service ID hex '{}': {}", input, e))
}

#[cfg(test)]
pub(crate) fn verify_transfer_signature(transfer: &Transfer) -> bool {
    let signing_message = transfer_signing_message(
        &transfer.from,
        &transfer.to,
        transfer.nonce,
        transfer.amount,
    );

    let key = match VerificationKey::try_from(transfer.from) {
        Ok(key) => key,
        Err(_) => return false,
    };

    let signature = Signature::from(transfer.signature);
    key.verify(&signature, &signing_message).is_ok()
}

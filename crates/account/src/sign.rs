//! Request signing.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::DateTime;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

fn hmac_sha256(secret: &str, message: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC przyjmuje klucz dowolnej długości");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// OKX: base64(HMAC-SHA256(secret, timestamp + method + path + body)).
pub fn okx(secret: &str, timestamp: &str, method: &str, path: &str, body: &str) -> String {
    STANDARD.encode(hmac_sha256(
        secret,
        &format!("{timestamp}{method}{path}{body}"),
    ))
}

/// Bybit V5: hex(HMAC-SHA256(secret, timestamp + key + recv window + payload)).
pub fn bybit(
    secret: &str,
    timestamp: u64,
    api_key: &str,
    recv_window: u64,
    payload: &str,
) -> String {
    hex::encode(hmac_sha256(
        secret,
        &format!("{timestamp}{api_key}{recv_window}{payload}"),
    ))
}

/// Unix time in ms.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("zegar systemowy przed 1970")
        .as_millis() as u64
}

/// ISO 8601 UTC with milliseconds, as OKX expects: `2020-12-08T09:08:57.715Z`.
pub fn iso_ms(ms: u64) -> String {
    DateTime::from_timestamp_millis(ms as i64)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values computed independently with Python's hmac module.
    #[test]
    fn okx_signature() {
        let sign = okx(
            "secret",
            "2020-12-08T09:08:57.715Z",
            "GET",
            "/api/v5/account/balance",
            "",
        );
        assert_eq!(sign, "5ktoTKif8DCJlIPb/3Kfd1A17bIRye6jpS9QBWj+9AU=");
    }

    #[test]
    fn bybit_signature() {
        let sign = bybit("secret", 1658384314791, "key", 5000, "category=spot");
        assert_eq!(
            sign,
            "9b8c8022390d3cf878af74c4d2438d971c1a1b8330807bff7a952d33ef2f281d"
        );
    }

    #[test]
    fn okx_timestamp_format() {
        assert_eq!(iso_ms(1607418537715), "2020-12-08T09:08:57.715Z");
    }
}

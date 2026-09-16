pub mod dispatcher;
pub mod handler;
pub mod models;

use aes::Aes256;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use cbc::cipher::block_padding::NoPadding;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
pub use dispatcher::{EventDispatcher, EventDispatcherConfig};
pub use handler::{BoxedEventHandler, EventHandler, EventHandlerResult};
pub use models::{ChallengeResponse, Event, EventHeader, EventReq, EventResp};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use thiserror::Error;

type Aes256CbcDec = cbc::Decryptor<Aes256>;

pub const HEADER_REQUEST_NONCE: &str = "x-lark-request-nonce";
pub const HEADER_REQUEST_TIMESTAMP: &str = "x-lark-request-timestamp";
pub const HEADER_SIGNATURE: &str = "x-lark-signature";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventEncryptMsg {
    pub encrypt: Option<String>,
}

#[derive(Debug, Error)]
pub enum EventError {
    #[error("missing event encrypt key")]
    MissingEncryptKey,
    #[error("base64 decode failed: {0}")]
    Base64Decode(String),
    #[error("cipher too short")]
    CipherTooShort,
    #[error("ciphertext is not a multiple of block size")]
    InvalidCipherBlockSize,
    #[error("cipher init failed: {0}")]
    CipherInit(String),
    #[error("cipher decrypt failed")]
    CipherDecrypt,
    #[error("unable to locate json object after decrypt")]
    JsonBounds,
    #[error("json parse failed: {0}")]
    JsonParse(String),
}

pub fn event_signature_sha256(
    timestamp: &str,
    nonce: &str,
    event_encrypt_key: &str,
    body: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(event_encrypt_key.as_bytes());
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn card_signature_sha1(timestamp: &str, nonce: &str, token: &str, body: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(token.as_bytes());
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn decrypt_event_payload(encrypt_b64: &str, secret: &str) -> Result<Vec<u8>, EventError> {
    let raw = BASE64_STANDARD
        .decode(encrypt_b64)
        .map_err(|e| EventError::Base64Decode(e.to_string()))?;
    if raw.len() < 16 {
        return Err(EventError::CipherTooShort);
    }

    let iv = &raw[..16];
    let mut ciphertext = raw[16..].to_vec();
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(EventError::InvalidCipherBlockSize);
    }

    let key = Sha256::digest(secret.as_bytes());
    let decryptor = Aes256CbcDec::new_from_slices(&key, iv)
        .map_err(|e| EventError::CipherInit(e.to_string()))?;
    let plaintext = decryptor
        .decrypt_padded_mut::<NoPadding>(&mut ciphertext)
        .map_err(|_| EventError::CipherDecrypt)?;

    trim_json_object(plaintext)
}

pub fn maybe_decrypt_event_body(
    raw_body: &[u8],
    event_encrypt_key: Option<&str>,
) -> Result<Vec<u8>, EventError> {
    let parsed: Result<EventEncryptMsg, serde_json::Error> = serde_json::from_slice(raw_body);
    if let Ok(encrypt_msg) = parsed
        && let Some(encrypt) = encrypt_msg.encrypt
        && !encrypt.is_empty()
    {
        let key = event_encrypt_key.ok_or(EventError::MissingEncryptKey)?;
        return decrypt_event_payload(&encrypt, key);
    }
    Ok(raw_body.to_vec())
}

pub fn parse_event_fuzzy(
    raw_body: &[u8],
    event_encrypt_key: Option<&str>,
) -> Result<Event, EventError> {
    let plain = maybe_decrypt_event_body(raw_body, event_encrypt_key)?;
    serde_json::from_slice(&plain).map_err(|e| EventError::JsonParse(e.to_string()))
}

fn trim_json_object(plaintext: &[u8]) -> Result<Vec<u8>, EventError> {
    if plaintext.is_empty() {
        return Err(EventError::JsonBounds);
    }
    let start = plaintext.iter().position(|b| *b == b'{').unwrap_or(0);
    let end = plaintext
        .iter()
        .rposition(|b| *b == b'}')
        .unwrap_or(plaintext.len().saturating_sub(1));
    if start > end || end >= plaintext.len() {
        return Err(EventError::JsonBounds);
    }
    Ok(plaintext[start..=end].to_vec())
}

#[cfg(test)]
mod tests {
    use cbc::cipher::BlockEncryptMut;

    use super::*;

    type Aes256CbcEnc = cbc::Encryptor<Aes256>;

    #[test]
    fn signature_functions_match_expected_values() {
        let timestamp = "1700000000";
        let nonce = "abc123";
        let body = "{\"challenge\":\"x\"}";
        assert_eq!(
            event_signature_sha256(timestamp, nonce, "encrypt_key", body),
            "86cc7c84b60af63b24af1c2e76de459afc45c99442dc5e9e26ae3c916464eaeb"
        );
        assert_eq!(
            card_signature_sha1(timestamp, nonce, "verify_token", body),
            "09c276adfee382dc53a6a0e1865a3220ef810629"
        );
    }

    #[test]
    fn decrypt_event_payload_roundtrip() {
        let secret = "test_secret";
        let json = b"{\"schema\":\"2.0\",\"type\":\"event_callback\"}";
        let plaintext = with_noise_and_padding(json);
        let encrypted = encrypt_for_test(&plaintext, secret);

        let plain = decrypt_event_payload(&encrypted, secret).expect("decrypt");
        assert_eq!(plain, json);
    }

    #[test]
    fn parse_event_fuzzy_handles_encrypted_envelope() {
        let secret = "test_secret";
        let payload = b"{\"schema\":\"2.0\",\"type\":\"event_callback\",\"token\":\"t\"}";
        let plaintext = with_noise_and_padding(payload);
        let encrypted = encrypt_for_test(&plaintext, secret);
        let envelope = format!("{{\"encrypt\":\"{}\"}}", encrypted);

        let event = parse_event_fuzzy(envelope.as_bytes(), Some(secret)).expect("parse");
        assert_eq!(event.schema.as_deref(), Some("2.0"));
        assert_eq!(event.type_.as_deref(), Some("event_callback"));
        assert_eq!(event.token.as_deref(), Some("t"));
    }

    fn with_noise_and_padding(json: &[u8]) -> Vec<u8> {
        let mut plaintext = b"prefix-".to_vec();
        plaintext.extend_from_slice(json);
        plaintext.extend_from_slice(b"-suffix");
        while !plaintext.len().is_multiple_of(16) {
            plaintext.push(0);
        }
        plaintext
    }

    fn encrypt_for_test(plaintext: &[u8], secret: &str) -> String {
        let key = Sha256::digest(secret.as_bytes());
        let iv = [7u8; 16];
        let encryptor = Aes256CbcEnc::new_from_slices(&key, &iv).expect("encryptor");
        let mut input = plaintext.to_vec();
        let msg_len = input.len();
        let ciphertext = encryptor
            .encrypt_padded_mut::<NoPadding>(&mut input, msg_len)
            .expect("encrypt")
            .to_vec();

        let mut out = iv.to_vec();
        out.extend_from_slice(&ciphertext);
        BASE64_STANDARD.encode(out)
    }
}

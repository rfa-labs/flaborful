use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use anyhow::{anyhow, Result};
use base64::Engine as _;

const NONCE_SIZE: usize = 12;
const KEY_SIZE: usize = 32;

/// Lazily read and validate the encryption key. The key MUST be exactly 32 bytes via the
/// `CREDENTIAL_KEY` env var. There is NO hardcoded default — a missing or wrong-length key
/// fails loudly rather than silently encrypting every OAuth token / VPN profile / cloud
/// credential with a known weak or zero-padded key. We use a thread-local OnceCell so the
/// env lookup happens once per process; rotating the key requires a restart.
fn key_bytes() -> Result<[u8; KEY_SIZE]> {
    use std::sync::OnceLock;
    static KEY: OnceLock<Result<[u8; KEY_SIZE]>> = OnceLock::new();
    KEY.get_or_init(|| {
        let raw = std::env::var("CREDENTIAL_KEY")
            .map_err(|_| anyhow!("CREDENTIAL_KEY must be set (expected 32-byte raw key for at-rest encryption of OAuth tokens / VPN profiles)"))?;
        let bytes = raw.into_bytes();
        if bytes.len() != KEY_SIZE {
            return Err(anyhow!(
                "CREDENTIAL_KEY must be exactly {} bytes (got {}); generate one with `openssl rand -base64 32 | base64 -d | xxd -p -c 32` and set the raw 32-byte value",
                KEY_SIZE,
                bytes.len()
            ));
        }
        let mut arr = [0u8; KEY_SIZE];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    })
    .as_ref()
    .map_err(|e| anyhow!("{}", e))
    .copied()
}

/// Validate that CREDENTIAL_KEY is present and well-formed without performing any crypto.
/// Called at startup so operators get an early warning instead of a lazy failure on the
/// first integration use.
pub fn validate_key() -> Result<()> {
    key_bytes().map(|_| ())
}

fn cipher() -> Result<Aes256Gcm> {
    Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes()?)))
}

/// Encrypt a JSON string of credentials. Returns base64(nonce || ciphertext).
pub fn encrypt(plaintext: &str) -> Result<String> {
    let cipher = cipher()?;
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow!("Encryption failed: {}", e))?;

    let mut combined = nonce_bytes.to_vec();
    combined.extend_from_slice(&ciphertext);

    Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
}

/// Decrypt a base64(nonce || ciphertext) blob back to plaintext.
pub fn decrypt(encoded: &str) -> Result<String> {
    let combined = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| anyhow!("Base64 decode failed: {}", e))?;

    if combined.len() < NONCE_SIZE {
        return Err(anyhow!("Ciphertext too short"));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_SIZE);
    let nonce = Nonce::from_slice(nonce_bytes);

    let cipher = cipher()?;
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("Decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| anyhow!("UTF-8 decode failed: {}", e))
}

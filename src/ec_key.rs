//! Elliptic-curve key agreement (X25519) + HKDF seed derivation for transform keys.
//! Deterministic, no RNG at call sites: callers must supply 32-byte private keys from SDR.

use anyhow::{Result, anyhow};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// Derive a shared secret from our 32-byte private key and their 32-byte public key.
pub fn derive_shared_secret(our_private: &[u8; 32], their_public: &[u8; 32]) -> Result<[u8; 32]> {
    let sk = StaticSecret::from(*our_private);
    let pk = PublicKey::from(*their_public);
    let shared = sk.diffie_hellman(&pk);
    Ok(shared.to_bytes())
}

/// HKDF-SHA256 to produce a seed for orthogonal transform generation.
pub fn seed_from_shared(shared: &[u8; 32], context: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(context), shared);
    let mut okm = [0u8; 32];
    hk.expand(b"ferricula-transform-seed", &mut okm)
        .expect("hkdf expand");
    okm
}

/// Generate a public key from a 32-byte private key (deterministic).
pub fn public_from_private(private: &[u8; 32]) -> [u8; 32] {
    let sk = StaticSecret::from(*private);
    let pk = PublicKey::from(&sk);
    pk.to_bytes()
}

/// Parse hex-encoded 32-byte key.
pub fn parse_hex_key(hex_str: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(hex_str.trim()).map_err(|e| anyhow!("invalid hex key: {e}"))?;
    if bytes.len() != 32 {
        return Err(anyhow!("expected 32-byte key, got {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

//! RFC 9180 HPKE sealing for push notifications.
//!
//! Ciphersuite: DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + AES-128-GCM.
//!
//! Uses the `hpke` crate's single-shot API (Base mode) so both the Rust
//! daemon and the Swift Notification Service Extension use a vetted
//! implementation of the same standard. CryptoKit on iOS 17+ supports
//! this ciphersuite natively. ChaCha20-Poly1305 is not available in
//! CryptoKit's HPKE, which is why AES-128-GCM is chosen instead.
//!
//! Wire format (unchanged from the bespoke predecessor):
//!   enc (32 bytes) ‖ ciphertext (plaintext.len() + 16 tag)
//!
//! The `HPKE_DOMAIN` constant is passed as the HPKE `info` parameter
//! for domain separation, ensuring keys derived for push cannot be
//! confused with keys from any other subsystem.

use ::hpke::{
    aead::AesGcm128, kdf::HkdfSha256, kem::X25519HkdfSha256, Deserializable, Kem, OpModeR, OpModeS,
    Serializable,
};
use pm_protocol::gateway::HPKE_DOMAIN;
use rand::rngs::OsRng;

/// Seals `plaintext` for `recipient_pk` (32-byte X25519 public key).
/// Returns the concatenation `enc (32) ‖ ciphertext (N + 16)`.
pub fn seal(recipient_pk: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let pk =
        <X25519HkdfSha256 as Kem>::PublicKey::from_bytes(recipient_pk).expect("valid X25519 pk");

    let (enc, ciphertext) = ::hpke::single_shot_seal::<AesGcm128, HkdfSha256, X25519HkdfSha256, _>(
        &OpModeS::Base,
        &pk,
        HPKE_DOMAIN,
        plaintext,
        b"", // no AAD
        &mut OsRng,
    )
    .expect("HPKE seal cannot fail for valid inputs");

    let enc_bytes = enc.to_bytes();
    let mut output = Vec::with_capacity(enc_bytes.len() + ciphertext.len());
    output.extend_from_slice(&enc_bytes);
    output.extend_from_slice(&ciphertext);
    output
}

/// Opens a sealed message using the recipient's secret key.
/// `sealed` must be `enc (32) ‖ ciphertext`.
pub fn open(recipient_sk: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 32 + 16 {
        return None; // Too short: need at least enc + tag.
    }
    let (enc_bytes, ciphertext) = sealed.split_at(32);

    let sk = <X25519HkdfSha256 as Kem>::PrivateKey::from_bytes(recipient_sk).ok()?;
    let enc = <X25519HkdfSha256 as Kem>::EncappedKey::from_bytes(enc_bytes).ok()?;

    ::hpke::single_shot_open::<AesGcm128, HkdfSha256, X25519HkdfSha256>(
        &OpModeR::Base,
        &sk,
        &enc,
        HPKE_DOMAIN,
        ciphertext,
        b"", // no AAD
    )
    .ok()
}

/// Generates a fresh X25519 keypair. Returns (secret_key, public_key)
/// as 32-byte arrays.
pub fn generate_keypair() -> ([u8; 32], [u8; 32]) {
    let (sk, pk) = X25519HkdfSha256::gen_keypair(&mut OsRng);
    let sk_bytes: Vec<u8> = sk.to_bytes().to_vec();
    let pk_bytes: Vec<u8> = pk.to_bytes().to_vec();
    let mut sk_arr = [0u8; 32];
    let mut pk_arr = [0u8; 32];
    sk_arr.copy_from_slice(&sk_bytes);
    pk_arr.copy_from_slice(&pk_bytes);
    (sk_arr, pk_arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_round_trip() {
        let (sk, pk) = generate_keypair();
        let plaintext = b"hello, push notification";
        let sealed = seal(&pk, plaintext);
        assert_eq!(sealed.len(), 32 + plaintext.len() + 16);
        let opened = open(&sk, &sealed).expect("open must succeed");
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let (_sk1, pk1) = generate_keypair();
        let (sk2, _pk2) = generate_keypair();
        let sealed = seal(&pk1, b"secret");
        assert!(open(&sk2, &sealed).is_none(), "wrong key must fail");
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let (sk, pk) = generate_keypair();
        let mut sealed = seal(&pk, b"secret");
        // Flip a byte in the ciphertext portion.
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(open(&sk, &sealed).is_none(), "tampered must fail");
    }

    #[test]
    fn empty_plaintext_works() {
        let (sk, pk) = generate_keypair();
        let sealed = seal(&pk, b"");
        let opened = open(&sk, &sealed).unwrap();
        assert!(opened.is_empty());
    }

    #[test]
    fn too_short_sealed_returns_none() {
        let (sk, _pk) = generate_keypair();
        assert!(open(&sk, &[0u8; 47]).is_none());
    }

    #[test]
    fn large_payload_within_budget() {
        let (sk, pk) = generate_keypair();
        let plaintext = vec![0x42u8; pm_protocol::gateway::SEALED_JSON_BUDGET];
        let sealed = seal(&pk, &plaintext);
        assert_eq!(
            sealed.len(),
            32 + plaintext.len() + 16,
            "overhead must be exactly HPKE_OVERHEAD"
        );
        let opened = open(&sk, &sealed).unwrap();
        assert_eq!(opened, plaintext);
    }
}

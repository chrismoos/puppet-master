//! First-contact proof for a peer that is not yet pinned.
//!
//! A pinned peer is authenticated by the handshake alone. Enrollment is the
//! one moment where the verifier has no pin to check, so both sides prove
//! knowledge of the one-time token instead. The proof is a MAC over material
//! exported from the finished TLS session, which binds it to this exact
//! connection: an interceptor terminates two separate sessions with two
//! different exporters, so a proof captured from one cannot be replayed into
//! the other.
//!
//! The dialer proves first. It is asking for access, and a scanner that
//! reaches the listener without the token learns nothing but a random nonce.

use hmac::{Hmac, Mac as _};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::KeyHash;

/// Exporter label. The version suffix keeps a future proof format from
/// validating against this one.
const EXPORTER_LABEL: &[u8] = b"pm-worker-pair-v1";

pub const NONCE_LEN: usize = 32;
pub const MAC_LEN: usize = 32;

pub type Nonce = [u8; NONCE_LEN];
pub type Mac = [u8; MAC_LEN];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Dialer,
    Listener,
}

impl Side {
    fn tag(self) -> &'static [u8] {
        match self {
            Side::Dialer => b"dialer",
            Side::Listener => b"listener",
        }
    }
}

pub fn nonce() -> Nonce {
    let mut buf = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf
}

/// Keying material bound to the finished handshake.
pub fn exporter<D>(conn: &rustls::ConnectionCommon<D>) -> Result<[u8; 32], rustls::Error> {
    conn.export_keying_material([0u8; 32], EXPORTER_LABEL, None)
}

/// Everything both sides agree on before either proves anything. Both build
/// an identical transcript; only the `Side` passed to `mac` differs, so the
/// two proofs cannot be substituted for one another.
pub struct Transcript {
    pub exporter: [u8; 32],
    pub dialer_key: KeyHash,
    pub listener_key: KeyHash,
    pub dialer_nonce: Nonce,
    pub listener_nonce: Nonce,
}

impl Transcript {
    pub fn mac(&self, secret: &str, side: Side) -> Mac {
        let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes())
            .expect("hmac accepts a key of any length");
        mac.update(side.tag());
        mac.update(&self.exporter);
        mac.update(self.dialer_key.as_bytes());
        mac.update(self.listener_key.as_bytes());
        mac.update(&self.dialer_nonce);
        mac.update(&self.listener_nonce);
        mac.finalize().into_bytes().into()
    }

    pub fn verify(&self, secret: &str, side: Side, presented: &Mac) -> bool {
        self.mac(secret, side).ct_eq(presented).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(exporter: [u8; 32]) -> Transcript {
        Transcript {
            exporter,
            dialer_key: KeyHash::from_hex(&"11".repeat(32)).unwrap(),
            listener_key: KeyHash::from_hex(&"22".repeat(32)).unwrap(),
            dialer_nonce: [3u8; NONCE_LEN],
            listener_nonce: [4u8; NONCE_LEN],
        }
    }

    #[test]
    fn both_sides_agree_on_a_proof() {
        let t = transcript([9u8; 32]);
        let presented = t.mac("token", Side::Dialer);
        assert!(t.verify("token", Side::Dialer, &presented));
    }

    #[test]
    fn the_wrong_token_does_not_prove_anything() {
        let t = transcript([9u8; 32]);
        let presented = t.mac("token", Side::Dialer);
        assert!(!t.verify("guess", Side::Dialer, &presented));
    }

    #[test]
    fn a_dialer_proof_cannot_stand_in_for_the_listener() {
        let t = transcript([9u8; 32]);
        let presented = t.mac("token", Side::Dialer);
        assert!(!t.verify("token", Side::Listener, &presented));
    }

    /// An interceptor relaying a captured proof into its own TLS session
    /// presents it against a different exporter, which is the whole point of
    /// binding the proof to the channel.
    #[test]
    fn a_proof_from_another_session_is_refused() {
        let intercepted = transcript([9u8; 32]).mac("token", Side::Dialer);
        let relayed = transcript([10u8; 32]);
        assert!(!relayed.verify("token", Side::Dialer, &intercepted));
    }

    #[test]
    fn a_proof_is_bound_to_both_peer_keys() {
        let t = transcript([9u8; 32]);
        let presented = t.mac("token", Side::Dialer);
        let mut substituted = transcript([9u8; 32]);
        substituted.listener_key = KeyHash::from_hex(&"33".repeat(32)).unwrap();
        assert!(!substituted.verify("token", Side::Dialer, &presented));
    }

    #[test]
    fn nonces_differ_between_connections() {
        assert_ne!(nonce(), nonce());
    }
}

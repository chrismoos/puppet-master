//! Framing for the worker control WebSocket.
//!
//! The link opens with the listener declaring whether this peer is already
//! known. A known peer proceeds straight to protobuf control messages; an
//! unknown one must first prove it holds the enrollment token, and both
//! sides prove it to each other before either acts on anything the other
//! says.

/// A protobuf control message follows.
pub const TAG_CONTROL: u8 = 0x00;
/// The listener does not recognize this peer and opens enrollment.
pub const TAG_PAIR_HELLO: u8 = 0x01;
/// The dialer's proof, which it sends before it is trusted with anything.
pub const TAG_PAIR_PROOF: u8 = 0x02;
/// The listener's proof, which lets the dialer trust what follows.
pub const TAG_PAIR_ACCEPT: u8 = 0x03;
/// The listener recognizes this peer and expects control messages.
pub const TAG_READY: u8 = 0x04;

pub const NONCE_LEN: usize = 32;
pub const MAC_LEN: usize = 32;

#[derive(Debug, PartialEq)]
pub enum WorkerFrame<'a> {
    Control(&'a [u8]),
    PairHello {
        nonce: [u8; NONCE_LEN],
    },
    PairProof {
        nonce: [u8; NONCE_LEN],
        mac: [u8; MAC_LEN],
    },
    PairAccept {
        mac: [u8; MAC_LEN],
    },
    Ready,
}

pub fn encode_control(payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + payload.len());
    buf.push(TAG_CONTROL);
    buf.extend_from_slice(payload);
    buf
}

pub fn encode_pair_hello(nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut buf = vec![TAG_PAIR_HELLO];
    buf.extend_from_slice(nonce);
    buf
}

pub fn encode_pair_proof(nonce: &[u8; NONCE_LEN], mac: &[u8; MAC_LEN]) -> Vec<u8> {
    let mut buf = vec![TAG_PAIR_PROOF];
    buf.extend_from_slice(nonce);
    buf.extend_from_slice(mac);
    buf
}

pub fn encode_pair_accept(mac: &[u8; MAC_LEN]) -> Vec<u8> {
    let mut buf = vec![TAG_PAIR_ACCEPT];
    buf.extend_from_slice(mac);
    buf
}

pub fn encode_ready() -> Vec<u8> {
    vec![TAG_READY]
}

pub fn decode(buf: &[u8]) -> Option<WorkerFrame<'_>> {
    let (tag, body) = buf.split_first()?;
    match *tag {
        TAG_CONTROL => Some(WorkerFrame::Control(body)),
        TAG_PAIR_HELLO => Some(WorkerFrame::PairHello {
            nonce: body.try_into().ok()?,
        }),
        TAG_PAIR_PROOF => {
            let (nonce, mac) = body.split_at_checked(NONCE_LEN)?;
            Some(WorkerFrame::PairProof {
                nonce: nonce.try_into().ok()?,
                mac: mac.try_into().ok()?,
            })
        }
        TAG_PAIR_ACCEPT => Some(WorkerFrame::PairAccept {
            mac: body.try_into().ok()?,
        }),
        TAG_READY if body.is_empty() => Some(WorkerFrame::Ready),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_frames_round_trip() {
        let frame = encode_control(b"hello");
        match decode(&frame) {
            Some(WorkerFrame::Control(p)) => assert_eq!(p, b"hello"),
            _ => panic!("expected control"),
        }
    }

    #[test]
    fn unknown_frames_are_rejected() {
        assert!(decode(&[]).is_none());
        assert!(decode(&[0x9f]).is_none());
    }

    #[test]
    fn pairing_frames_round_trip() {
        let nonce = [7u8; NONCE_LEN];
        let mac = [8u8; MAC_LEN];
        assert_eq!(
            decode(&encode_pair_hello(&nonce)),
            Some(WorkerFrame::PairHello { nonce })
        );
        assert_eq!(
            decode(&encode_pair_proof(&nonce, &mac)),
            Some(WorkerFrame::PairProof { nonce, mac })
        );
        assert_eq!(
            decode(&encode_pair_accept(&mac)),
            Some(WorkerFrame::PairAccept { mac })
        );
        assert_eq!(decode(&encode_ready()), Some(WorkerFrame::Ready));
    }

    /// A truncated proof must not decode into a shorter one that happens to
    /// verify against padding.
    #[test]
    fn missized_pairing_frames_are_rejected() {
        assert!(decode(&[TAG_PAIR_HELLO, 0, 0]).is_none());
        assert!(decode(&[TAG_PAIR_PROOF; NONCE_LEN + 1]).is_none());
        assert!(decode(&[TAG_PAIR_ACCEPT]).is_none());
        assert!(decode(&[TAG_READY, 0]).is_none());
    }
}

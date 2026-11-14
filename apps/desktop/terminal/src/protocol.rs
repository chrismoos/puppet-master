//! The controller's binary terminal socket frames, byte for byte the layout
//! the browser's `terminalFrame.ts` encodes and decodes.

pub const SUBPROTOCOL: &str = "pm-terminal-v1";

const TAG_OUTPUT: u8 = 0x01;
const TAG_INPUT: u8 = 0x02;
const TAG_RESIZE: u8 = 0x03;
const TAG_INPUT_SUBMIT: u8 = 0x04;
const TAG_ACK: u8 = 0x05;
const TAG_RESYNC: u8 = 0x06;
const TAG_OWNERSHIP: u8 = 0x07;
const TAG_RESIZE_REQUEST: u8 = 0x08;

pub const FLAG_REPLAY: u8 = 0x01;
pub const FLAG_REPLAY_START: u8 = 0x02;
pub const FLAG_REPLAY_END: u8 = 0x04;
pub const FLAG_REPLAY_SNAPSHOT: u8 = 0x08;
pub const FLAG_REPLAY_GHOSTTY: u8 = 0x10;

const GENERATION: usize = 1;
const HEADER: usize = 9;
const OUTPUT_PAYLOAD: usize = 10;
const RESIZE_BYTES: usize = 13;
const OWNERSHIP_BYTES: usize = 37;
const RESIZE_REQUEST_BYTES: usize = 21;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    Output {
        generation: u64,
        flags: u8,
        data: Vec<u8>,
    },
    /// The PTY's size, echoed after any viewer resizes it.
    PtySize {
        generation: u64,
        cols: u16,
        rows: u16,
    },
    Ownership(Ownership),
}

/// Which viewer owns the terminal's size, and what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ownership {
    pub generation: u64,
    pub revision: u64,
    pub owner: u64,
    pub acknowledgment: u64,
    pub cols: u16,
    pub rows: u16,
}

fn u16_at(frame: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([frame[at], frame[at + 1]])
}

fn u64_at(frame: &[u8], at: usize) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&frame[at..at + 8]);
    u64::from_le_bytes(bytes)
}

pub fn decode(frame: &[u8]) -> Option<Incoming> {
    match *frame.first()? {
        TAG_OUTPUT if frame.len() >= OUTPUT_PAYLOAD => Some(Incoming::Output {
            generation: u64_at(frame, GENERATION),
            flags: frame[HEADER],
            data: frame[OUTPUT_PAYLOAD..].to_vec(),
        }),
        TAG_RESIZE if frame.len() == RESIZE_BYTES => Some(Incoming::PtySize {
            generation: u64_at(frame, GENERATION),
            cols: u16_at(frame, HEADER),
            rows: u16_at(frame, HEADER + 2),
        }),
        TAG_OWNERSHIP if frame.len() == OWNERSHIP_BYTES => Some(Incoming::Ownership(Ownership {
            generation: u64_at(frame, GENERATION),
            revision: u64_at(frame, HEADER),
            owner: u64_at(frame, HEADER + 8),
            acknowledgment: u64_at(frame, HEADER + 16),
            cols: u16_at(frame, HEADER + 24),
            rows: u16_at(frame, HEADER + 26),
        })),
        _ => None,
    }
}

fn header(tag: u8, generation: u64, extra: usize) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER + extra);
    frame.push(tag);
    frame.extend_from_slice(&generation.to_le_bytes());
    frame
}

/// Typed or pasted bytes. `submitted` marks input that ends with Enter, which
/// answers a session waiting on the user.
pub fn input(generation: u64, data: &[u8], submitted: bool) -> Vec<u8> {
    let mut frame = header(
        if submitted {
            TAG_INPUT_SUBMIT
        } else {
            TAG_INPUT
        },
        generation,
        data.len(),
    );
    frame.extend_from_slice(data);
    frame
}

pub fn resize(generation: u64, cols: u16, rows: u16) -> Vec<u8> {
    let mut frame = header(TAG_RESIZE, generation, 4);
    frame.extend_from_slice(&cols.to_le_bytes());
    frame.extend_from_slice(&rows.to_le_bytes());
    frame
}

/// A size claim the controller acknowledges by `request` in its next
/// ownership frame.
pub fn resize_request(generation: u64, request: u64, cols: u16, rows: u16) -> Vec<u8> {
    let mut frame = header(TAG_RESIZE_REQUEST, generation, 12);
    frame.extend_from_slice(&request.to_le_bytes());
    frame.extend_from_slice(&cols.to_le_bytes());
    frame.extend_from_slice(&rows.to_le_bytes());
    debug_assert_eq!(frame.len(), RESIZE_REQUEST_BYTES);
    frame
}

/// Output bytes this viewer has parsed, which paces the relay to it.
pub fn ack(generation: u64, bytes: u32) -> Vec<u8> {
    let mut frame = header(TAG_ACK, generation, 4);
    frame.extend_from_slice(&bytes.to_le_bytes());
    frame
}

/// Asks for a fresh snapshot on the open socket.
pub fn resync(generation: u64) -> Vec<u8> {
    header(TAG_RESYNC, generation, 0)
}

/// Collects a replay's chunks until its last one arrives.
#[derive(Default)]
pub struct Replay {
    chunks: Vec<u8>,
    snapshot: bool,
    ghostty: bool,
    open: bool,
}

/// A whole replay: a libghostty snapshot to restore, or raw output to write
/// onto a reset screen.
#[derive(Debug, PartialEq, Eq)]
pub struct CompleteReplay {
    pub data: Vec<u8>,
    pub ghostty_snapshot: bool,
}

impl Replay {
    /// Takes one replay frame's flags and data, and returns the replay once
    /// its end flag arrives.
    pub fn push(&mut self, flags: u8, data: &[u8]) -> Option<CompleteReplay> {
        if flags & FLAG_REPLAY_START != 0 || !self.open {
            self.chunks.clear();
            self.snapshot = false;
            self.ghostty = false;
            self.open = true;
        }
        self.snapshot |= flags & FLAG_REPLAY_SNAPSHOT != 0;
        self.ghostty |= flags & FLAG_REPLAY_GHOSTTY != 0;
        self.chunks.extend_from_slice(data);
        if flags & FLAG_REPLAY_END == 0 {
            return None;
        }
        self.open = false;
        Some(CompleteReplay {
            data: std::mem::take(&mut self.chunks),
            ghostty_snapshot: self.ghostty,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_frames_have_the_browser_layout() {
        assert_eq!(
            input(7, b"ls", false),
            [&[0x02][..], &7u64.to_le_bytes(), b"ls"].concat()
        );
        assert_eq!(input(7, b"\r", true)[0], 0x04);
        assert_eq!(
            resize(7, 120, 40),
            [
                &[0x03][..],
                &7u64.to_le_bytes(),
                &120u16.to_le_bytes(),
                &40u16.to_le_bytes()
            ]
            .concat()
        );
        assert_eq!(
            ack(7, 4096),
            [&[0x05][..], &7u64.to_le_bytes(), &4096u32.to_le_bytes()].concat()
        );
        assert_eq!(resync(7), [&[0x06][..], &7u64.to_le_bytes()].concat());
        let request = resize_request(7, 3, 100, 30);
        assert_eq!(request.len(), 21);
        assert_eq!(
            (
                request[0],
                u64_at(&request, 9),
                u16_at(&request, 17),
                u16_at(&request, 19)
            ),
            (0x08, 3, 100, 30)
        );
    }

    #[test]
    fn server_frames_decode() {
        let output = [&[0x01][..], &9u64.to_le_bytes(), &[FLAG_REPLAY], b"hi"].concat();
        assert_eq!(
            decode(&output),
            Some(Incoming::Output {
                generation: 9,
                flags: FLAG_REPLAY,
                data: b"hi".to_vec()
            })
        );
        let size = resize(9, 80, 24);
        assert_eq!(
            decode(&size),
            Some(Incoming::PtySize {
                generation: 9,
                cols: 80,
                rows: 24
            })
        );
        let ownership = [
            &[0x07][..],
            &9u64.to_le_bytes(),
            &2u64.to_le_bytes(),
            &5u64.to_le_bytes(),
            &3u64.to_le_bytes(),
            &100u16.to_le_bytes(),
            &30u16.to_le_bytes(),
        ]
        .concat();
        assert_eq!(ownership.len(), 37);
        assert_eq!(
            decode(&ownership),
            Some(Incoming::Ownership(Ownership {
                generation: 9,
                revision: 2,
                owner: 5,
                acknowledgment: 3,
                cols: 100,
                rows: 30
            }))
        );
        assert_eq!(decode(&[0x01, 0, 0]), None);
        assert_eq!(decode(&[0xff; 20]), None);
    }

    #[test]
    fn a_replay_is_assembled_across_chunks() {
        let mut replay = Replay::default();
        assert_eq!(
            replay.push(
                FLAG_REPLAY | FLAG_REPLAY_START | FLAG_REPLAY_SNAPSHOT | FLAG_REPLAY_GHOSTTY,
                b"ab"
            ),
            None
        );
        assert_eq!(replay.push(FLAG_REPLAY, b"cd"), None);
        assert_eq!(
            replay.push(FLAG_REPLAY | FLAG_REPLAY_END, b"e"),
            Some(CompleteReplay {
                data: b"abcde".to_vec(),
                ghostty_snapshot: true
            })
        );
        assert_eq!(
            replay.push(FLAG_REPLAY | FLAG_REPLAY_START | FLAG_REPLAY_END, b"raw"),
            Some(CompleteReplay {
                data: b"raw".to_vec(),
                ghostty_snapshot: false
            })
        );
    }
}

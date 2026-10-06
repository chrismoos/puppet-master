use bytes::Bytes;

pub const TAG_OUTPUT: u8 = 0x01;
pub const TAG_INPUT: u8 = 0x02;
pub const TAG_RESIZE: u8 = 0x03;
pub const TAG_INPUT_SUBMIT: u8 = 0x04;
/// A viewer's credit: output bytes it has parsed, so the relay can pace
/// a flood to the slowest viewer rather than queue it without bound.
pub const TAG_ACK: u8 = 0x05;
/// A viewer asking for a fresh snapshot on its open socket, which it uses
/// after a width change instead of reconnecting.
pub const TAG_RESYNC: u8 = 0x06;
pub const TAG_OWNERSHIP: u8 = 0x07;
pub const TAG_RESIZE_REQUEST: u8 = 0x08;

pub const FLAG_REPLAY: u8 = 0x01;
pub const FLAG_REPLAY_START: u8 = 0x02;
pub const FLAG_REPLAY_END: u8 = 0x04;
/// The replay payload is a serialized terminal state snapshot rather
/// than a raw scrollback tail; the client can skip repaint workarounds.
pub const FLAG_REPLAY_SNAPSHOT: u8 = 0x08;

pub const MAX_REPLAY_CHUNK_BYTES: usize = 32 * 1024;

const GENERATION_BYTES: usize = 8;
const OUTPUT_HEADER_BYTES: usize = 1 + GENERATION_BYTES + 1;
const INPUT_HEADER_BYTES: usize = 1 + GENERATION_BYTES;
const RESIZE_FRAME_BYTES: usize = 1 + GENERATION_BYTES + 2 + 2;
const ACK_FRAME_BYTES: usize = 1 + GENERATION_BYTES + 4;
const REQUEST_OFFSET: usize = 1 + GENERATION_BYTES;
const REQUEST_COLS_OFFSET: usize = REQUEST_OFFSET + 8;
const REQUEST_ROWS_OFFSET: usize = REQUEST_COLS_OFFSET + 2;
const REQUEST_FRAME_BYTES: usize = REQUEST_ROWS_OFFSET + 2;
const OWNERSHIP_FRAME_BYTES: usize = 1 + GENERATION_BYTES + 8 + 8 + 8 + 2 + 2;
const RESYNC_FRAME_BYTES: usize = 1 + GENERATION_BYTES;

#[derive(Debug, PartialEq, Eq)]
pub enum TerminalFrame<'a> {
    Output {
        generation: u64,
        flags: u8,
        data: &'a [u8],
    },
    Input {
        generation: u64,
        submitted: bool,
        data: &'a [u8],
    },
    ResizeRequest {
        generation: u64,
        request: u64,
        cols: u16,
        rows: u16,
    },
    Resize {
        generation: u64,
        cols: u16,
        rows: u16,
    },
    Ack {
        generation: u64,
        bytes: u32,
    },
    Resync {
        generation: u64,
    },
}

pub fn encode_output(generation: u64, flags: u8, data: &[u8]) -> Bytes {
    let mut frame = Vec::with_capacity(OUTPUT_HEADER_BYTES + data.len());
    frame.push(TAG_OUTPUT);
    frame.extend_from_slice(&generation.to_le_bytes());
    frame.push(flags);
    frame.extend_from_slice(data);
    frame.into()
}

pub fn encode_input(generation: u64, data: &[u8]) -> Bytes {
    encode_input_with_submission(generation, data, false)
}

pub fn encode_input_with_submission(generation: u64, data: &[u8], submitted: bool) -> Bytes {
    let mut frame = Vec::with_capacity(INPUT_HEADER_BYTES + data.len());
    frame.push(if submitted {
        TAG_INPUT_SUBMIT
    } else {
        TAG_INPUT
    });
    frame.extend_from_slice(&generation.to_le_bytes());
    frame.extend_from_slice(data);
    frame.into()
}

pub fn encode_ownership(
    generation: u64,
    revision: u64,
    owner: u64,
    acknowledgment: u64,
    cols: u16,
    rows: u16,
) -> Bytes {
    let mut frame = Vec::with_capacity(OWNERSHIP_FRAME_BYTES);
    frame.push(TAG_OWNERSHIP);
    for value in [generation, revision, owner, acknowledgment] {
        frame.extend_from_slice(&value.to_le_bytes());
    }
    frame.extend_from_slice(&cols.to_le_bytes());
    frame.extend_from_slice(&rows.to_le_bytes());
    Bytes::from(frame)
}

pub fn encode_resize(generation: u64, cols: u16, rows: u16) -> Bytes {
    let mut frame = Vec::with_capacity(RESIZE_FRAME_BYTES);
    frame.push(TAG_RESIZE);
    frame.extend_from_slice(&generation.to_le_bytes());
    frame.extend_from_slice(&cols.to_le_bytes());
    frame.extend_from_slice(&rows.to_le_bytes());
    frame.into()
}

pub fn encode_ack(generation: u64, bytes: u32) -> Bytes {
    let mut frame = Vec::with_capacity(ACK_FRAME_BYTES);
    frame.push(TAG_ACK);
    frame.extend_from_slice(&generation.to_le_bytes());
    frame.extend_from_slice(&bytes.to_le_bytes());
    frame.into()
}

pub fn encode_resync(generation: u64) -> Bytes {
    let mut frame = Vec::with_capacity(RESYNC_FRAME_BYTES);
    frame.push(TAG_RESYNC);
    frame.extend_from_slice(&generation.to_le_bytes());
    frame.into()
}

pub fn decode(frame: &[u8]) -> Option<TerminalFrame<'_>> {
    match *frame.first()? {
        TAG_OUTPUT if frame.len() >= OUTPUT_HEADER_BYTES => Some(TerminalFrame::Output {
            generation: u64::from_le_bytes(frame[1..9].try_into().ok()?),
            flags: frame[9],
            data: &frame[OUTPUT_HEADER_BYTES..],
        }),
        TAG_INPUT | TAG_INPUT_SUBMIT if frame.len() >= INPUT_HEADER_BYTES => {
            Some(TerminalFrame::Input {
                generation: u64::from_le_bytes(frame[1..9].try_into().ok()?),
                submitted: frame[0] == TAG_INPUT_SUBMIT,
                data: &frame[INPUT_HEADER_BYTES..],
            })
        }
        TAG_RESIZE_REQUEST if frame.len() == REQUEST_FRAME_BYTES => {
            Some(TerminalFrame::ResizeRequest {
                generation: u64::from_le_bytes(frame[1..REQUEST_OFFSET].try_into().ok()?),
                request: u64::from_le_bytes(
                    frame[REQUEST_OFFSET..REQUEST_COLS_OFFSET].try_into().ok()?,
                ),
                cols: u16::from_le_bytes(
                    frame[REQUEST_COLS_OFFSET..REQUEST_ROWS_OFFSET]
                        .try_into()
                        .ok()?,
                ),
                rows: u16::from_le_bytes(
                    frame[REQUEST_ROWS_OFFSET..REQUEST_FRAME_BYTES]
                        .try_into()
                        .ok()?,
                ),
            })
        }
        TAG_RESIZE if frame.len() == RESIZE_FRAME_BYTES => Some(TerminalFrame::Resize {
            generation: u64::from_le_bytes(frame[1..9].try_into().ok()?),
            cols: u16::from_le_bytes(frame[9..11].try_into().ok()?),
            rows: u16::from_le_bytes(frame[11..13].try_into().ok()?),
        }),
        TAG_RESYNC if frame.len() == RESYNC_FRAME_BYTES => Some(TerminalFrame::Resync {
            generation: u64::from_le_bytes(frame[1..9].try_into().ok()?),
        }),
        TAG_ACK if frame.len() == ACK_FRAME_BYTES => Some(TerminalFrame::Ack {
            generation: u64::from_le_bytes(frame[1..9].try_into().ok()?),
            bytes: u32::from_le_bytes(frame[9..13].try_into().ok()?),
        }),
        _ => None,
    }
}

pub fn replay_frames(generation: u64, replay: &[u8]) -> Vec<Bytes> {
    replay_frames_with_flags(generation, replay, 0)
}

pub fn replay_frames_with_flags(generation: u64, replay: &[u8], extra_flags: u8) -> Vec<Bytes> {
    if replay.is_empty() {
        return vec![encode_output(
            generation,
            FLAG_REPLAY | FLAG_REPLAY_START | FLAG_REPLAY_END | extra_flags,
            &[],
        )];
    }
    let chunks: Vec<&[u8]> = replay.chunks(MAX_REPLAY_CHUNK_BYTES).collect();
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let mut flags = FLAG_REPLAY | extra_flags;
            if index == 0 {
                flags |= FLAG_REPLAY_START;
            }
            if index == last {
                flags |= FLAG_REPLAY_END;
            }
            encode_output(generation, flags, chunk)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_frames_round_trip() {
        let output = encode_output(7, FLAG_REPLAY, b"out");
        assert_eq!(
            decode(&output),
            Some(TerminalFrame::Output {
                generation: 7,
                flags: FLAG_REPLAY,
                data: b"out",
            })
        );
        let input = encode_input(8, b"in");
        assert_eq!(
            decode(&input),
            Some(TerminalFrame::Input {
                generation: 8,
                submitted: false,
                data: b"in",
            })
        );
        let submit = encode_input_with_submission(8, b"\r", true);
        assert_eq!(
            decode(&submit),
            Some(TerminalFrame::Input {
                generation: 8,
                submitted: true,
                data: b"\r",
            })
        );
        let ack = encode_ack(9, 4096);
        assert_eq!(ack.len(), 13);
        assert_eq!(
            decode(&ack),
            Some(TerminalFrame::Ack {
                generation: 9,
                bytes: 4096,
            })
        );
        assert_eq!(decode(&ack[..12]), None);
        let resync = encode_resync(11);
        assert_eq!(resync.len(), 9);
        assert_eq!(
            decode(&resync),
            Some(TerminalFrame::Resync { generation: 11 })
        );
        assert_eq!(decode(&resync[..8]), None);
        let resize = encode_resize(9, 120, 40);
        assert_eq!(
            decode(&resize),
            Some(TerminalFrame::Resize {
                generation: 9,
                cols: 120,
                rows: 40,
            })
        );
    }

    #[test]
    fn replay_is_chunked_with_one_start_and_end() {
        let replay = vec![b'x'; MAX_REPLAY_CHUNK_BYTES * 2 + 1];
        let frames = replay_frames(3, &replay);
        assert_eq!(frames.len(), 3);
        let flags: Vec<u8> = frames
            .iter()
            .map(|frame| match decode(frame).unwrap() {
                TerminalFrame::Output { flags, .. } => flags,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(flags[0], FLAG_REPLAY | FLAG_REPLAY_START);
        assert_eq!(flags[1], FLAG_REPLAY);
        assert_eq!(flags[2], FLAG_REPLAY | FLAG_REPLAY_END);
    }

    #[test]
    fn malformed_frames_are_rejected() {
        assert!(decode(&[]).is_none());
        assert!(decode(&[TAG_OUTPUT]).is_none());
        assert!(decode(&[TAG_RESIZE; RESIZE_FRAME_BYTES - 1]).is_none());
    }
    #[test]
    fn numbered_resize_request_decodes_without_changing_legacy_frames() {
        let mut request = vec![TAG_RESIZE_REQUEST];
        request.extend_from_slice(&3_u64.to_le_bytes());
        request.extend_from_slice(&5_u64.to_le_bytes());
        request.extend_from_slice(&120_u16.to_le_bytes());
        request.extend_from_slice(&40_u16.to_le_bytes());
        assert_eq!(
            decode(&request),
            Some(TerminalFrame::ResizeRequest {
                generation: 3,
                request: 5,
                cols: 120,
                rows: 40
            })
        );
        assert!(decode(&request[..request.len() - 1]).is_none());
        assert_eq!(
            decode(&encode_resize(3, 120, 40)),
            Some(TerminalFrame::Resize {
                generation: 3,
                cols: 120,
                rows: 40
            })
        );
    }
}

//! Decodes raw terminal input bytes into the small key vocabulary the
//! dashboard uses. One stdin byte stream serves both the dashboard
//! (decoded here) and attach passthrough (forwarded verbatim), so the
//! two modes never compete for reads.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    CtrlC,
}

/// CSI final bytes occupy this range; anything before it is a
/// parameter or intermediate byte.
const CSI_FINAL_RANGE: std::ops::RangeInclusive<u8> = 0x40..=0x7e;

const BYTE_ESC: u8 = 0x1b;
const BYTE_CTRL_C: u8 = 0x03;
const BYTE_BACKSPACE_DEL: u8 = 0x7f;
const BYTE_BACKSPACE_BS: u8 = 0x08;

/// Decodes a chunk of input bytes. Escape sequences are assumed not to
/// split across chunks, which holds for interactive typing; unknown
/// sequences and control bytes are dropped.
pub fn decode(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            BYTE_ESC => i = decode_escape(bytes, i, &mut keys),
            b'\r' | b'\n' => {
                keys.push(Key::Enter);
                i += 1;
            }
            b'\t' => {
                keys.push(Key::Tab);
                i += 1;
            }
            BYTE_BACKSPACE_DEL | BYTE_BACKSPACE_BS => {
                keys.push(Key::Backspace);
                i += 1;
            }
            BYTE_CTRL_C => {
                keys.push(Key::CtrlC);
                i += 1;
            }
            0x00..=0x1f => i += 1,
            first => {
                let end = (i + utf8_len(first)).min(bytes.len());
                match std::str::from_utf8(&bytes[i..end]) {
                    Ok(s) => {
                        if let Some(c) = s.chars().next() {
                            keys.push(Key::Char(c));
                        }
                        i = end;
                    }
                    Err(_) => i += 1,
                }
            }
        }
    }
    keys
}

fn decode_escape(bytes: &[u8], start: usize, keys: &mut Vec<Key>) -> usize {
    match bytes.get(start + 1) {
        Some(b'[') => {
            let mut j = start + 2;
            while j < bytes.len() && !CSI_FINAL_RANGE.contains(&bytes[j]) {
                j += 1;
            }
            let Some(&final_byte) = bytes.get(j) else {
                return bytes.len();
            };
            if j == start + 2 {
                match final_byte {
                    b'A' => keys.push(Key::Up),
                    b'B' => keys.push(Key::Down),
                    b'C' => keys.push(Key::Right),
                    b'D' => keys.push(Key::Left),
                    b'Z' => keys.push(Key::BackTab),
                    _ => {}
                }
            }
            j + 1
        }
        Some(b'O') => {
            match bytes.get(start + 2) {
                Some(b'A') => keys.push(Key::Up),
                Some(b'B') => keys.push(Key::Down),
                Some(b'C') => keys.push(Key::Right),
                Some(b'D') => keys.push(Key::Left),
                _ => {}
            }
            start + 3
        }
        _ => {
            keys.push(Key::Esc);
            start + 1
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_chars() {
        assert_eq!(decode(b"jk"), vec![Key::Char('j'), Key::Char('k')]);
    }

    #[test]
    fn enter_tab_backspace_ctrl_c() {
        assert_eq!(decode(b"\r"), vec![Key::Enter]);
        assert_eq!(decode(b"\n"), vec![Key::Enter]);
        assert_eq!(decode(b"\t"), vec![Key::Tab]);
        assert_eq!(decode(&[0x7f]), vec![Key::Backspace]);
        assert_eq!(decode(&[0x08]), vec![Key::Backspace]);
        assert_eq!(decode(&[0x03]), vec![Key::CtrlC]);
    }

    #[test]
    fn csi_arrows_and_backtab() {
        assert_eq!(decode(b"\x1b[A"), vec![Key::Up]);
        assert_eq!(decode(b"\x1b[B"), vec![Key::Down]);
        assert_eq!(decode(b"\x1b[C"), vec![Key::Right]);
        assert_eq!(decode(b"\x1b[D"), vec![Key::Left]);
        assert_eq!(decode(b"\x1b[Z"), vec![Key::BackTab]);
    }

    #[test]
    fn ss3_arrows() {
        assert_eq!(decode(b"\x1bOA"), vec![Key::Up]);
        assert_eq!(decode(b"\x1bOB"), vec![Key::Down]);
    }

    #[test]
    fn lone_esc() {
        assert_eq!(decode(b"\x1b"), vec![Key::Esc]);
        assert_eq!(decode(b"\x1bx"), vec![Key::Esc, Key::Char('x')]);
    }

    #[test]
    fn parameterized_csi_ignored() {
        assert_eq!(decode(b"\x1b[1;5A"), Vec::<Key>::new());
        assert_eq!(decode(b"\x1b[1;5Aq"), vec![Key::Char('q')]);
    }

    #[test]
    fn utf8_chars() {
        assert_eq!(decode("é".as_bytes()), vec![Key::Char('é')]);
        assert_eq!(decode("日".as_bytes()), vec![Key::Char('日')]);
    }

    #[test]
    fn mixed_sequence() {
        assert_eq!(
            decode(b"j\x1b[Bq"),
            vec![Key::Char('j'), Key::Down, Key::Char('q')]
        );
    }

    #[test]
    fn truncated_csi_is_dropped() {
        assert_eq!(decode(b"\x1b["), Vec::<Key>::new());
    }
}

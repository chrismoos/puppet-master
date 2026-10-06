//! Diagnostic timestamps for terminal latency probes. When `PM_PROBE_TRACE`
//! names a file, every hop that sees a frame containing `pmprobe-<id>`
//! appends one line: epoch microseconds, hop name, probe id. Off, this
//! costs one environment read per process and a memchr per frame.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

const MARKER: &[u8] = b"pmprobe-";
const ID_MAX: usize = 24;

fn sink() -> Option<&'static Mutex<std::fs::File>> {
    static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("PM_PROBE_TRACE")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
}

/// Records `hop` for every probe id inside `bytes`.
pub fn mark(hop: &str, bytes: &[u8]) {
    let Some(file) = sink() else { return };
    let mut rest = bytes;
    while let Some(at) = rest.windows(MARKER.len()).position(|w| w == MARKER) {
        let start = at + MARKER.len();
        let id: Vec<u8> = rest[start..]
            .iter()
            .take(ID_MAX)
            .take_while(|b| b.is_ascii_alphanumeric())
            .copied()
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(file, "{now} {hop} {}", String::from_utf8_lossy(&id));
        }
        rest = &rest[start..];
    }
}

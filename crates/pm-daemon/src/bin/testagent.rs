//! Scripted stand-in for a real agent CLI, driven by the test suite
//! through a PTY. Prints READY with its prompt argument, then executes
//! line commands: `echo <text>`, `needs`, `osc52 <selection;payload>`,
//! `mouseon`, `mouseoff`, `lineout <lines>`, `bigout <bytes>`,
//! `pacedout <bytes> <chunk>`, `syncout <lines> <delayms>`,
//! `claudestream <history> <frames> <delayms> <linelen>`, `gridtui`,
//! `exit <code>`.
//! Output lines are prefixed with OUT so tests can tell agent output
//! from the PTY's echo of their own input.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const PACED_OUTPUT_DELAY: Duration = Duration::from_millis(50);
const GRID_TICK: Duration = Duration::from_millis(100);
const GRID_STATUS_TICKS: u64 = 3;
/// Rule, prompt, and status rows under the numbered grid rows.
const GRID_FOOTER_ROWS: u16 = 3;
const GRID_PROMPT: &str = "> ";

static WINCH: AtomicBool = AtomicBool::new(false);
static WINCH_WATCHER: AtomicBool = AtomicBool::new(false);
static CLAUDE_HISTORY: Mutex<Vec<String>> = Mutex::new(Vec::new());

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A real raw-mode, full-screen input composer that models the agent TUI
/// contract relevant to supervisor submission. Rapid unbracketed characters
/// enter paste-burst mode, so Enter stages a newline. An explicit bracketed
/// paste is one paste event, and — matching the measured behavior of the
/// real agent TUIs — an Enter that arrives in the same read chunk as the
/// paste-end marker is folded into the paste as a staged newline; only an
/// Enter from a later read submits. It runs behind a PTY in the integration
/// test; this is intentionally not a line-oriented shell stand-in.
fn tui_composer(out: &mut impl Write) {
    let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
    assert_eq!(unsafe { libc::tcgetattr(0, &mut original) }, 0);
    let mut raw = original;
    unsafe { libc::cfmakeraw(&mut raw) };
    assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) }, 0);

    write!(out, "\x1b[?1049h\x1b[?2004h\x1b[2J\x1b[HCOMPOSER READY\r\n").unwrap();
    out.flush().unwrap();

    let mut pending = Vec::new();
    let mut composer = Vec::new();
    let mut explicit_paste = false;
    let mut fold_after_paste;
    let mut raw_burst = false;
    let mut raw_chars = 0usize;
    let mut read_buf = [0u8; 4096];
    loop {
        let count = unsafe {
            libc::read(
                0,
                read_buf.as_mut_ptr() as *mut libc::c_void,
                read_buf.len(),
            )
        };
        if count <= 0 {
            break;
        }
        // A fresh read chunk means the paste-burst quiescence window has
        // passed, the way the real TUIs stop folding input into a paste
        // once their event queue drains.
        fold_after_paste = false;
        pending.extend_from_slice(&read_buf[..count as usize]);
        loop {
            if explicit_paste {
                let Some(end) = find_bytes(&pending, BRACKETED_PASTE_END) else {
                    let keep = pending.len().min(BRACKETED_PASTE_END.len() - 1);
                    let emit = pending.len() - keep;
                    composer.extend_from_slice(&pending[..emit]);
                    pending.drain(..emit);
                    break;
                };
                composer.extend_from_slice(&pending[..end]);
                pending.drain(..end + BRACKETED_PASTE_END.len());
                explicit_paste = false;
                fold_after_paste = true;
                raw_burst = false;
                raw_chars = 0;
                continue;
            }
            if pending.starts_with(BRACKETED_PASTE_START) {
                pending.drain(..BRACKETED_PASTE_START.len());
                explicit_paste = true;
                continue;
            }
            if pending.len() < BRACKETED_PASTE_START.len()
                && BRACKETED_PASTE_START.starts_with(&pending)
            {
                break;
            }
            let Some(byte) = pending.first().copied() else {
                break;
            };
            pending.remove(0);
            if matches!(byte, b'\r' | b'\n') {
                if raw_burst || fold_after_paste {
                    composer.push(b'\n');
                    writeln!(out, "STAGED {}", composer.len()).unwrap();
                } else {
                    writeln!(
                        out,
                        "SUBMITTED {} {}",
                        composer.len(),
                        String::from_utf8_lossy(&composer)
                    )
                    .unwrap();
                    composer.clear();
                }
                out.flush().unwrap();
                raw_chars = 0;
                continue;
            }
            composer.push(byte);
            raw_chars += 1;
            raw_burst |= raw_chars >= 2;
        }
    }
    let _ = unsafe { libc::tcsetattr(0, libc::TCSANOW, &original) };
}

extern "C" fn on_winch(_signal: libc::c_int) {
    WINCH.store(true, Ordering::SeqCst);
}

fn install_winch_handler() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_winch as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
    }
}

/// Renders the way Claude Code and similar AI CLIs do: the app owns its
/// transcript, erases the terminal scrollback, and repaints everything in
/// one synchronized frame. Between full paints it only appends and redraws
/// the bottom status box, so the byte stream alone cannot reconstruct the
/// transcript without a full frame.
fn claude_full_frame() -> String {
    let history = CLAUDE_HISTORY.lock().unwrap();
    let mut buffer = String::new();
    buffer.push_str("\x1b[?2026h\x1b[3J\x1b[2J\x1b[H");
    for line in history.iter() {
        buffer.push_str(line);
        buffer.push_str("\r\n");
    }
    claude_box(&mut buffer, history.len());
    buffer.push_str("\x1b[?2026l");
    buffer
}

fn claude_full_render(out: &mut impl Write) {
    out.write_all(claude_full_frame().as_bytes()).unwrap();
    out.flush().unwrap();
}

/// The main loop holds the stdout lock for the whole program, so the resize
/// watcher thread must write through the raw descriptor instead.
fn write_stdout_raw(bytes: &[u8]) {
    let mut offset = 0;
    while offset < bytes.len() {
        let written = unsafe {
            libc::write(
                1,
                bytes[offset..].as_ptr() as *const libc::c_void,
                bytes.len() - offset,
            )
        };
        if written <= 0 {
            return;
        }
        offset += written as usize;
    }
}

fn claude_box(buffer: &mut String, frame: usize) {
    for boxline in 0..6 {
        buffer.push_str(&format!(
            "\x1b[38;5;244m│ input box line {boxline} after {frame:05} │\x1b[0m\r\n"
        ));
    }
}

fn claude_incremental(out: &mut impl Write, line: String) {
    let mut history = CLAUDE_HISTORY.lock().unwrap();
    history.push(line.clone());
    let mut buffer = String::new();
    buffer.push_str("\x1b[?2026h\x1b[6A\r\x1b[0J");
    buffer.push_str(&line);
    buffer.push_str("\r\n");
    claude_box(&mut buffer, history.len());
    buffer.push_str("\x1b[?2026l");
    out.write_all(buffer.as_bytes()).unwrap();
    out.flush().unwrap();
}

fn pty_size() -> (u16, u16) {
    let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } != 0 {
        return (0, 0);
    }
    (size.ws_col, size.ws_row)
}

fn padded(text: &str, fill: char, width: usize) -> String {
    let mut line: String = text.chars().take(width).collect();
    while line.chars().count() < width {
        line.push(fill);
    }
    line
}

/// The rule, prompt, and status rows, ending with the cursor after the
/// prompt the way an agent composer leaves it.
fn grid_footer(buffer: &mut String, cols: u16, rows: u16, tick: u64) {
    let width = cols as usize;
    buffer.push_str(&padded("", '─', width));
    buffer.push_str("\r\n");
    buffer.push_str(GRID_PROMPT);
    buffer.push_str("\r\n");
    buffer.push_str(&padded(
        &format!("STATUS {cols}x{rows} tick {tick:06} "),
        '=',
        width,
    ));
    buffer.push_str(&format!("\x1b[1A\r\x1b[{}G", GRID_PROMPT.len() + 1));
}

/// A full-screen TUI laid out for the PTY's size: numbered rows exactly as
/// wide as the terminal, a rule, a prompt, and one status line. It clears and
/// repaints everything on SIGWINCH and otherwise redraws only its footer
/// relative to the cursor, as Ink-based agent CLIs do, so a viewer whose
/// buffer does not match the PTY's geometry shows it as a torn or doubled
/// footer.
fn grid_tui() -> ! {
    install_winch_handler();
    let mut tick = 0u64;
    let mut paint = true;
    loop {
        let (cols, rows) = pty_size();
        let mut buffer = String::from("\x1b[?2026h");
        if paint {
            buffer.push_str("\x1b[3J\x1b[2J\x1b[H");
            for row in 1..=rows.saturating_sub(GRID_FOOTER_ROWS) {
                let label = format!("GRID-{row:03} {cols}x{rows} ");
                buffer.push_str(&padded(&label, '.', cols as usize - 1));
                buffer.push_str("#\r\n");
            }
        } else {
            buffer.push_str("\x1b[1A\r\x1b[J");
        }
        grid_footer(&mut buffer, cols, rows, tick);
        buffer.push_str("\x1b[?2026l");
        write_stdout_raw(buffer.as_bytes());
        loop {
            std::thread::sleep(GRID_TICK);
            tick += 1;
            paint = WINCH.swap(false, Ordering::SeqCst);
            if paint || tick.is_multiple_of(GRID_STATUS_TICKS) {
                break;
            }
        }
    }
}

fn main() {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if prompt == "tui-composer" {
        tui_composer(&mut out);
        return;
    }
    writeln!(out, "READY {prompt}").unwrap();
    out.flush().unwrap();

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match cmd {
            "echo" => {
                writeln!(out, "OUT {arg}").unwrap();
            }
            "needs" => {
                write!(out, "\x1b]9;needs-input\x07").unwrap();
            }
            "osc52" => {
                write!(out, "\x1b]52;{arg}\x07").unwrap();
            }
            "mouseon" => {
                write!(out, "\x1b[?1000h\x1b[?1006h").unwrap();
            }
            "mouseoff" => {
                write!(out, "\x1b[?1000l\x1b[?1006l").unwrap();
            }
            "bigout" => {
                let n: usize = arg.parse().unwrap_or(0);
                let chunk = vec![b'x'; n];
                out.write_all(&chunk).unwrap();
                writeln!(out).unwrap();
            }
            "lineout" => {
                let lines: usize = arg.parse().unwrap_or(0);
                for line in 0..lines {
                    writeln!(out, "OUT BOARD-RETURN-HISTORY-{line:04}").unwrap();
                }
            }
            "syncout" => {
                let mut args = arg.split_whitespace();
                let lines: usize = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                let delay_ms: u64 = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                for block_start in (0..lines).step_by(50) {
                    write!(out, "\x1b[?2026h").unwrap();
                    for index in block_start..(block_start + 50).min(lines) {
                        writeln!(out, "OUT CODEX-SYNC-{index:05}").unwrap();
                    }
                    write!(out, "\x1b[2K\r\x1b[?2026l").unwrap();
                    out.flush().unwrap();
                    if delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(delay_ms));
                    }
                }
                writeln!(out, "OUT CODEX-SYNC-DONE").unwrap();
            }
            "claudestream" => {
                let mut args = arg.split_whitespace();
                let history_lines: usize = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1500);
                let frames: usize = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(500);
                let delay_ms: u64 = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(15);
                let line_len: usize = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(200);
                let filler = "x".repeat(line_len);
                {
                    let mut history = CLAUDE_HISTORY.lock().unwrap();
                    history.clear();
                    for index in 0..history_lines {
                        history.push(format!("OUT CLAUDE-HISTORY-{index:05} {filler}"));
                    }
                }
                install_winch_handler();
                claude_full_render(&mut out);
                for _frame in 0..frames {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    if WINCH.swap(false, Ordering::SeqCst) {
                        claude_full_render(&mut out);
                        continue;
                    }
                    let next = {
                        let history = CLAUDE_HISTORY.lock().unwrap();
                        format!("OUT CLAUDE-HISTORY-{:05} {filler}", history.len())
                    };
                    claude_incremental(&mut out, next);
                }
                // Claude Code repaints on resize even while idle. Keep a
                // watcher so a SIGWINCH after the stream ends still redraws.
                if !WINCH_WATCHER.swap(true, Ordering::SeqCst) {
                    std::thread::spawn(|| loop {
                        std::thread::sleep(Duration::from_millis(25));
                        if WINCH.swap(false, Ordering::SeqCst) {
                            write_stdout_raw(claude_full_frame().as_bytes());
                        }
                    });
                }
                writeln!(out, "OUT CLAUDE-STREAM-DONE").unwrap();
            }
            "pacedout" => {
                let mut args = arg.split_whitespace();
                let bytes: usize = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                let chunk_bytes: usize = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1);
                let chunk = vec![b'x'; chunk_bytes];
                let mut remaining = bytes;
                while remaining > 0 {
                    let length = remaining.min(chunk.len());
                    out.write_all(&chunk[..length]).unwrap();
                    out.flush().unwrap();
                    remaining -= length;
                    std::thread::sleep(PACED_OUTPUT_DELAY);
                }
                writeln!(out).unwrap();
            }
            "gridtui" => {
                out.flush().unwrap();
                grid_tui();
            }
            "exit" => {
                std::process::exit(arg.parse().unwrap_or(0));
            }
            "" => {}
            _ => {
                writeln!(out, "? {cmd}").unwrap();
            }
        }
        out.flush().unwrap();
    }
}

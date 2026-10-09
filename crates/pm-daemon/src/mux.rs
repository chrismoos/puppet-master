//! The terminal mux: owns every session's PTY, keeps scrollback, and
//! fans output out to any number of attached viewers. Viewers receive
//! byte-identical streams; attach is ring-buffer replay followed by
//! live output with no gap or duplication in between.

use crate::program_status::{parse_body, query_reply, RecordStore, Report, ScanEvent, Scanner};
use crate::term_model::Retained;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use pm_adapters::CommandSpec;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::{broadcast, mpsc};
use tracing::debug;

/// Per-session scrollback kept in memory and replayed on attach.
pub const SCROLLBACK_CAP_BYTES: usize = 2 * 1024 * 1024;

/// Buffered chunks a slow viewer may fall behind before being
/// force-detached (it can re-attach and get a fresh replay).
const VIEWER_CHANNEL_CHUNKS: usize = 1024;

const PTY_READ_BUF_BYTES: usize = 8 * 1024;

/// Output a terminal's primary consumer may hold unsent before the PTY
/// reader waits for it, which is what bounds how far a flood can run
/// ahead of the viewer and therefore how long a keystroke's echo queues.
pub const PRIMARY_CONSUMER_BUDGET_BYTES: usize = 16 * 1024;

/// A consumer's hold on an [`OutputFlow`], released on drop.
pub struct FlowAttachment(Arc<OutputFlow>);

impl FlowAttachment {
    /// Credits bytes the consumer has handed to the transport.
    pub fn release(&self, bytes: usize) {
        self.0.release(bytes);
    }
}

impl Drop for FlowAttachment {
    fn drop(&mut self) {
        self.0.disable();
    }
}

/// Flow control between the PTY reader thread and the one consumer that
/// carries output off the machine. Enabled only while such a consumer is
/// attached: with nobody carrying output, the reader must never block,
/// or the agent would stall while unobserved.
#[derive(Default)]
pub struct OutputFlow {
    inflight: Mutex<(bool, usize)>,
    drained: std::sync::Condvar,
}

impl OutputFlow {
    fn acquire(&self, bytes: usize) {
        let mut state = self.inflight.lock().unwrap();
        while state.0 && state.1 > PRIMARY_CONSUMER_BUDGET_BYTES {
            state = self.drained.wait(state).unwrap();
        }
        state.1 += bytes;
    }

    /// Credits bytes the consumer has handed to the transport.
    pub fn release(&self, bytes: usize) {
        let mut state = self.inflight.lock().unwrap();
        state.1 = state.1.saturating_sub(bytes);
        self.drained.notify_all();
    }

    /// Turns the budget on for a consumer and returns the handle it
    /// credits through. Dropping the handle turns the budget off, so a
    /// consumer task that is aborted, errors or ends never leaves the
    /// reader blocked.
    pub fn attach(self: &Arc<Self>) -> FlowAttachment {
        self.enable();
        FlowAttachment(self.clone())
    }

    pub fn enable(&self) {
        let mut state = self.inflight.lock().unwrap();
        state.0 = true;
        state.1 = 0;
    }

    pub fn disable(&self) {
        let mut state = self.inflight.lock().unwrap();
        state.0 = false;
        state.1 = 0;
        self.drained.notify_all();
    }
}

/// A busy PTY can produce thousands of chunks per second. Activity is
/// observational metadata, so coalesce those chunks before crossing into
/// the daemon's checkpoint path.
const ACTIVITY_SIGNAL_INTERVAL: Duration = Duration::from_secs(1);

/// After the child exits, how long to wait for the PTY reader to
/// drain remaining output before finalizing the session anyway
/// (grandchildren holding the PTY open must not wedge exit handling).
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Geometry used between spawn and the first viewer resize.
pub(crate) const DEFAULT_COLS: u16 = 120;
pub(crate) const DEFAULT_ROWS: u16 = 32;

/// Protocol floor: reject resize messages below this. A viewer sending
/// cols < 10 is a bug (FitAddon computed against a zero-width container),
/// not a real geometry. Log the rejection loudly so the client-side cause
/// is visible rather than silently papered over.
pub(crate) const MIN_COLS: u16 = 10;
pub(crate) const MIN_ROWS: u16 = 2;
const DEFAULT_TERM: &str = "xterm-256color";
const DEFAULT_COLORTERM: &str = "truecolor";
const NO_COLOR_ENV: &str = "NO_COLOR";

/// ASCII ETX, what Ctrl-C produces; interrupt delivers it through the
/// PTY so the agent sees exactly a user keypress.
const INTERRUPT_BYTE: u8 = 0x03;

/// How long a killed agent has to exit after SIGTERM before it is
/// SIGKILLed. Long enough for an agent to flush its conversation.
const KILL_GRACE_PERIOD: Duration = Duration::from_secs(5);

const KILL_POLL_INTERVAL: Duration = Duration::from_millis(100);

const FOREGROUND_COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(250);
const FOREGROUND_COMMAND_MAX_CHARS: usize = 256;
const FOREGROUND_COMMAND_OSC: u16 = 777;
const FOREGROUND_COMMAND_OSC_PREFIX: &str = "pm-command;";

#[derive(Debug, thiserror::Error)]
pub enum MuxError {
    #[error("terminal {0} is not running")]
    NotRunning(u64),
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("pty: {0}")]
    Pty(String),
}

#[derive(Debug)]
pub struct SessionExit {
    pub terminal_id: u64,
    pub generation: u64,
    pub semantic_session_id: u64,
    pub exit_code: Option<i32>,
    /// Scrollback contents at exit, for the transcript file.
    pub scrollback: Bytes,
}

/// A terminal's serialized state at attach. The bytes are only meaningful
/// to an emulator of exactly `size`, so the two travel together.
pub struct TerminalSnapshot {
    pub bytes: Bytes,
    pub size: (u16, u16),
    pub output: broadcast::Receiver<Bytes>,
}

/// A needs-input signal detected in a session's PTY output (an agent
/// that reports approval pauses via an OSC9 escape rather than a hook).
#[derive(Debug)]
pub struct PtyNeedsInput {
    pub session_id: u64,
}

/// A change to an agent terminal's Program Status records.
#[derive(Debug)]
pub struct ProgramStatusUpdate {
    pub terminal_id: u64,
    pub generation: u64,
    pub session_id: u64,
    pub changes: crate::program_status::Changes,
}

/// A coalesced signal that a terminal produced live output.
#[derive(Debug)]
pub struct PtyActivity {
    pub terminal_id: u64,
    pub generation: u64,
    pub session_id: u64,
}

/// The escape an agent emits into the PTY when it pauses for approval
/// (OSC 9, a desktop-notification sequence). We only enable it for that
/// one event, so any occurrence means the session is waiting.
const OSC9_MARKER: &[u8] = b"\x1b]9;";

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

struct SessionEntry {
    generation: u64,
    semantic_session_id: u64,
    kind: pm_protocol::domain::TerminalKind,
    input_tx: std::sync::mpsc::Sender<Bytes>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    /// OS pid, and with it the id of the process group the PTY child
    /// leads, which is what termination signals go to. None if the
    /// platform didn't expose it, leaving only the killer.
    pid: Option<u32>,
    /// Clear once the child has been reaped, after which the OS may
    /// hand its pid to an unrelated process. An agent addressed by pid
    /// must not be looked up through a pid past that point.
    pid_held: Arc<AtomicBool>,
    /// Guards both the ring and out_tx subscription: broadcast sends
    /// happen under this lock, which is what makes attach's
    /// replay-then-live handoff gapless.
    ring: Arc<Mutex<Retained>>,
    out_tx: broadcast::Sender<Bytes>,
    exited: Arc<AtomicBool>,
    flow: Arc<OutputFlow>,
    program_status: Option<Arc<Mutex<RecordStore>>>,
}

pub struct Mux {
    /// Keyed by terminal id, not by session id: one session owns an
    /// agent terminal and any number of shell terminals, and the two id
    /// spaces are independent.
    terminals: Mutex<HashMap<u64, Arc<SessionEntry>>>,
    exit_tx: mpsc::UnboundedSender<SessionExit>,
    needs_input_tx: mpsc::UnboundedSender<PtyNeedsInput>,
    activity_tx: mpsc::UnboundedSender<PtyActivity>,
    program_status_tx: mpsc::UnboundedSender<ProgramStatusUpdate>,
}

pub struct MuxChannels {
    pub exit_rx: mpsc::UnboundedReceiver<SessionExit>,
    pub needs_input_rx: mpsc::UnboundedReceiver<PtyNeedsInput>,
    pub activity_rx: mpsc::UnboundedReceiver<PtyActivity>,
    pub program_status_rx: mpsc::UnboundedReceiver<ProgramStatusUpdate>,
}

/// What the child will actually see for `name`: an adapter override if
/// there is one, otherwise what this process holds. Agents locate their
/// own state through the environment, so a worker running with a
/// different HOME than the operator's shell looks somewhere else
/// entirely, and nothing about the failure says so.
fn effective_env(spec: &CommandSpec, name: &str) -> String {
    spec.env
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var(name).ok())
        .unwrap_or_default()
}

impl Mux {
    pub fn new() -> (Self, MuxChannels) {
        let (exit_tx, exit_rx) = mpsc::unbounded_channel();
        let (needs_input_tx, needs_input_rx) = mpsc::unbounded_channel();
        let (activity_tx, activity_rx) = mpsc::unbounded_channel();
        let (program_status_tx, program_status_rx) = mpsc::unbounded_channel();
        (
            Mux {
                terminals: Mutex::new(HashMap::new()),
                exit_tx,
                needs_input_tx,
                activity_tx,
                program_status_tx,
            },
            MuxChannels {
                exit_rx,
                needs_input_rx,
                activity_rx,
                program_status_rx,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        &self,
        terminal_id: u64,
        generation: u64,
        semantic_session_id: u64,
        spec: &CommandSpec,
        detect_osc9_needs_input: bool,
        track_foreground_command: bool,
        truecolor: bool,
        initial_size: Option<(u16, u16)>,
        program_status: bool,
    ) -> Result<(), MuxError> {
        let (cols, rows) = initial_size
            .map(|(c, r)| (c.max(MIN_COLS), r.max(MIN_ROWS)))
            .unwrap_or((DEFAULT_COLS, DEFAULT_ROWS));
        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| MuxError::Pty(e.to_string()))?;

        let program = if pm_protocol::domain::AgentKind::SELECTABLE
            .iter()
            .any(|agent| agent.program() == spec.program)
        {
            crate::harness_install::resolve_in(
                &spec.program,
                std::ffi::OsStr::new(&effective_env(spec, "PATH")),
                Some(std::path::Path::new(&effective_env(spec, "HOME"))),
            )
            .unwrap_or_else(|| spec.program.clone().into())
        } else {
            spec.program.clone().into()
        };
        let mut cmd = CommandBuilder::new(program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        cmd.env("TERM", DEFAULT_TERM);
        cmd.env("COLUMNS", cols.to_string());
        cmd.env("LINES", rows.to_string());
        // COLORTERM promises 24-bit color to the agent. When the user's
        // attach terminals cannot render RGB, leaving it unset drops
        // agents back to the 256-color palette every viewer can show.
        if truecolor {
            cmd.env("COLORTERM", DEFAULT_COLORTERM);
        } else {
            cmd.env_remove("COLORTERM");
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        cmd.env_remove(NO_COLOR_ENV);

        // Values are deliberately absent: an adapter puts provider
        // credentials in this environment, and only the names are needed
        // to tell what the child was handed.
        debug!(
            program = %spec.program,
            args = ?spec.args,
            cwd = %spec.cwd.display(),
            home = %effective_env(spec, "HOME"),
            path = %effective_env(spec, "PATH"),
            env_overrides = ?spec.env.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            "spawning agent"
        );

        let mut child = pty
            .slave
            .spawn_command(cmd)
            .map_err(|e| MuxError::Spawn(e.to_string()))?;
        drop(pty.slave);

        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pty
            .master
            .try_clone_reader()
            .map_err(|e| MuxError::Pty(e.to_string()))?;
        let mut writer = pty
            .master
            .take_writer()
            .map_err(|e| MuxError::Pty(e.to_string()))?;

        let ring = Arc::new(Mutex::new(Retained::new(SCROLLBACK_CAP_BYTES, cols, rows)));
        let (out_tx, _) = broadcast::channel(VIEWER_CHANNEL_CHUNKS);
        let (input_tx, input_rx) = std::sync::mpsc::channel::<Bytes>();
        let (drained_tx, drained_rx) = std::sync::mpsc::channel::<()>();
        let exited = Arc::new(AtomicBool::new(false));
        let pid_held = Arc::new(AtomicBool::new(pid.is_some()));
        let flow = Arc::new(OutputFlow::default());
        let flow_for_reader = flow.clone();
        let program_status = program_status.then(|| Arc::new(Mutex::new(RecordStore::default())));
        let reply_tx = input_tx.clone();

        let entry = Arc::new(SessionEntry {
            generation,
            semantic_session_id,
            kind: if track_foreground_command {
                pm_protocol::domain::TerminalKind::Shell
            } else {
                pm_protocol::domain::TerminalKind::Agent
            },
            input_tx,
            master: Mutex::new(pty.master),
            killer: Arc::new(Mutex::new(killer)),
            pid,
            pid_held: pid_held.clone(),
            ring: ring.clone(),
            out_tx: out_tx.clone(),
            exited: exited.clone(),
            flow,
            program_status: program_status.clone(),
        });
        self.terminals
            .lock()
            .unwrap()
            .insert(terminal_id, entry.clone());

        if track_foreground_command {
            let entry_for_command = entry.clone();
            std::thread::spawn(move || {
                let mut previous_process_group = None;
                let mut previous = None;
                while !entry_for_command.exited.load(Ordering::SeqCst) {
                    let viewed = entry_for_command.out_tx.receiver_count() > 0;
                    let process_group = viewed
                        .then(|| foreground_process_group(&entry_for_command))
                        .flatten();
                    if !foreground_process_changed(
                        &mut previous_process_group,
                        process_group,
                        viewed,
                    ) {
                        if !viewed {
                            previous = None;
                        }
                    } else if let Some(command) = process_group.and_then(command_for_process_group)
                    {
                        if Some(&command) != previous.as_ref() {
                            let chunk = terminal_command_sequence(&command);
                            let mut ring = entry_for_command.ring.lock().unwrap();
                            ring.push(&chunk);
                            let _ = entry_for_command.out_tx.send(chunk);
                            previous = Some(command);
                        }
                    } else {
                        // The command is unreadable while the process is still
                        // calling execve. Forget the group so the next poll
                        // looks again rather than treating it as reported.
                        previous_process_group = None;
                    }
                    std::thread::sleep(FOREGROUND_COMMAND_POLL_INTERVAL);
                }
            });
        }

        let ring_for_reader = ring.clone();
        let needs_input_tx = self.needs_input_tx.clone();
        let activity_tx = self.activity_tx.clone();
        let mut status_reader = program_status.clone().map(|store| ProgramStatusReader {
            store,
            scanner: Scanner::default(),
            reply_tx,
            updates: self.program_status_tx.clone(),
            terminal_id,
            generation,
            session_id: semantic_session_id,
        });
        std::thread::spawn(move || {
            let mut buf = [0u8; PTY_READ_BUF_BYTES];
            let mut last_activity_signal: Option<std::time::Instant> = None;
            // Bytes carried between reads so an OSC9 marker split across
            // a read boundary is still matched.
            let mut carry: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = match &mut status_reader {
                            Some(status) => Bytes::from(status.consume(&buf[..n])),
                            None => Bytes::copy_from_slice(&buf[..n]),
                        };
                        flow_for_reader.acquire(chunk.len());
                        crate::probe_trace::mark("w_pty_rx", &chunk);
                        let now = std::time::Instant::now();
                        if last_activity_signal
                            .is_none_or(|last| now.duration_since(last) >= ACTIVITY_SIGNAL_INTERVAL)
                        {
                            let _ = activity_tx.send(PtyActivity {
                                terminal_id,
                                generation,
                                session_id: semantic_session_id,
                            });
                            last_activity_signal = Some(now);
                        }
                        if detect_osc9_needs_input {
                            carry.extend_from_slice(&buf[..n]);
                            if find_subsequence(&carry, OSC9_MARKER).is_some() {
                                let _ = needs_input_tx.send(PtyNeedsInput {
                                    session_id: semantic_session_id,
                                });
                            }
                            let keep = carry.len().min(OSC9_MARKER.len() - 1);
                            carry.drain(..carry.len() - keep);
                        }
                        if chunk.is_empty() {
                            continue;
                        }
                        let mut ring = ring_for_reader.lock().unwrap();
                        ring.push(&chunk);
                        let _ = out_tx.send(chunk);
                    }
                }
            }
            if let Some(status) = &mut status_reader {
                let mut tail = Vec::new();
                status.scanner.end_of_stream(&mut tail);
                if !tail.is_empty() {
                    let tail = Bytes::from(tail);
                    ring_for_reader.lock().unwrap().push(&tail);
                    let _ = out_tx.send(tail);
                }
            }
            let _ = drained_tx.send(());
        });

        std::thread::spawn(move || {
            for data in input_rx {
                crate::probe_trace::mark("w_pty_write", &data);
                if writer.write_all(&data).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });

        let exit_tx = self.exit_tx.clone();
        let program_status_tx = self.program_status_tx.clone();
        std::thread::spawn(move || {
            // Reaping frees the pid for reuse, and an agent that names
            // its inbox after its own pid is addressed by it. Waiting
            // without reaping leaves the child a zombie, which holds the
            // pid until the reaping wait below collects it, so the claim
            // drops while the pid is still unusable by anyone else.
            if let Some(pid) = pid {
                await_exit_leaving_zombie(pid);
                pid_held.store(false, Ordering::SeqCst);
            }
            let status = child.wait();
            let _ = drained_rx.recv_timeout(READER_DRAIN_TIMEOUT);
            if let Some(store) = &program_status {
                let changes = store.lock().unwrap().drop_transient();
                if !changes.is_empty() {
                    let _ = program_status_tx.send(ProgramStatusUpdate {
                        terminal_id,
                        generation,
                        session_id: semantic_session_id,
                        changes,
                    });
                }
            }
            exited.store(true, Ordering::SeqCst);
            let exit_code = status.ok().map(|s| s.exit_code() as i32);
            let scrollback = ring.lock().unwrap().ring.snapshot();
            let _ = exit_tx.send(SessionExit {
                terminal_id,
                generation,
                semantic_session_id,
                exit_code,
                scrollback,
            });
        });

        Ok(())
    }

    /// Returns the scrollback so far plus a live receiver; under the
    /// ring lock so no output falls between replay and subscription.
    pub fn attach(
        &self,
        terminal_id: u64,
    ) -> Result<(Bytes, broadcast::Receiver<Bytes>), MuxError> {
        let entry = self.entry(terminal_id)?;
        let retained = entry.ring.lock().unwrap();
        let rx = entry.out_tx.subscribe();
        Ok((retained.ring.snapshot(), rx))
    }

    /// Serialized terminal state, the PTY size it is laid out for, and a
    /// live receiver, all under the retained lock so no output or resize
    /// falls between the snapshot and subscription.
    pub fn attach_snapshot(&self, terminal_id: u64) -> Result<TerminalSnapshot, MuxError> {
        let entry = self.entry(terminal_id)?;
        let retained = entry.ring.lock().unwrap();
        let output = entry.out_tx.subscribe();
        Ok(TerminalSnapshot {
            bytes: Bytes::from(retained.model.snapshot()),
            size: retained.size(),
            output,
        })
    }

    /// The flow budget a terminal's primary consumer credits as it sends.
    pub fn output_flow(&self, terminal_id: u64) -> Result<Arc<OutputFlow>, MuxError> {
        Ok(self.entry(terminal_id)?.flow.clone())
    }

    pub fn input(&self, terminal_id: u64, data: Bytes) -> Result<(), MuxError> {
        let entry = self.entry(terminal_id)?;
        entry
            .input_tx
            .send(data)
            .map_err(|_| MuxError::NotRunning(terminal_id))
    }

    pub fn interrupt(&self, terminal_id: u64) -> Result<(), MuxError> {
        self.input(terminal_id, Bytes::from_static(&[INTERRUPT_BYTE]))
    }

    pub fn resize(&self, terminal_id: u64, cols: u16, rows: u16) -> Result<(), MuxError> {
        if cols < MIN_COLS || rows < MIN_ROWS {
            tracing::warn!(
                terminal_id,
                cols,
                rows,
                min_cols = MIN_COLS,
                min_rows = MIN_ROWS,
                "rejecting degenerate PTY resize — viewer likely computed \
                 a fit against a zero-width container"
            );
            return Ok(());
        }
        let entry = self.entry(terminal_id)?;
        if entry.ring.lock().unwrap().size() == (cols, rows) {
            return Ok(());
        }
        let result = entry
            .master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| MuxError::Pty(e.to_string()));
        if result.is_ok() {
            entry.ring.lock().unwrap().resize(cols, rows);
        }
        result
    }

    pub fn current_size(&self, terminal_id: u64) -> Result<(u16, u16), MuxError> {
        let entry = self.entry(terminal_id)?;
        let size = entry.ring.lock().unwrap().size();
        Ok(size)
    }

    /// Terminates a session gracefully: SIGTERM first so the agent can
    /// flush its own state (a coding agent persists its conversation on
    /// shutdown, which agent-native resume then depends on), escalating
    /// to SIGKILL only if it does not exit within the grace period.
    ///
    /// Both signals go to the child's whole process group. Some agents
    /// launch through a wrapper that ignores termination signals and runs
    /// the real agent as a child, expecting the terminal to signal the
    /// foreground group; signalling the leader alone leaves such a
    /// session running with the PTY still open.
    pub fn kill(&self, terminal_id: u64) -> Result<(), MuxError> {
        let entry = self.entry(terminal_id)?;
        match entry.pid {
            Some(pid) => {
                signal_child_group(pid, libc::SIGTERM);
                let exited = entry.exited.clone();
                std::thread::spawn(move || {
                    let deadline = std::time::Instant::now() + KILL_GRACE_PERIOD;
                    while std::time::Instant::now() < deadline {
                        if exited.load(Ordering::SeqCst) {
                            return;
                        }
                        std::thread::sleep(KILL_POLL_INTERVAL);
                    }
                    signal_child_group(pid, libc::SIGKILL);
                });
                Ok(())
            }
            None => entry
                .killer
                .lock()
                .unwrap()
                .kill()
                .map_err(|e| MuxError::Pty(e.to_string())),
        }
    }

    /// Gracefully terminates every live session and returns how many
    /// termination requests were started.
    pub fn terminate_all(&self) -> usize {
        let terminal_ids = self.live_terminal_ids();
        let count = terminal_ids.len();
        for terminal_id in terminal_ids {
            let _ = self.kill(terminal_id);
        }
        count
    }

    /// Drops the terminal entry once the exit has been fully processed.
    pub fn remove(&self, terminal_id: u64) {
        self.terminals.lock().unwrap().remove(&terminal_id);
    }

    /// The pid of the PTY child, which an agent that names resources
    /// after its own process id needs in order to be addressed.
    ///
    /// Answers only while the child still holds the pid: the reaper
    /// clears that before it reaps, so a pid returned here cannot have
    /// been reassigned to another process. Addressing an agent by pid
    /// depends on it.
    pub fn child_pid(&self, terminal_id: u64) -> Option<u32> {
        self.terminals
            .lock()
            .unwrap()
            .get(&terminal_id)
            .filter(|e| e.pid_held.load(Ordering::SeqCst))
            .filter(|e| !e.exited.load(Ordering::SeqCst))
            .and_then(|e| e.pid)
    }

    /// Whether the mux holds an entry at all, including one that has exited
    /// but whose exit is still being reported.
    pub fn contains(&self, terminal_id: u64) -> bool {
        self.terminals.lock().unwrap().contains_key(&terminal_id)
    }

    pub fn is_running(&self, terminal_id: u64) -> bool {
        self.terminals
            .lock()
            .unwrap()
            .get(&terminal_id)
            .map(|e| !e.exited.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Ids of sessions still running, for a worker to re-announce on
    /// reconnect.
    pub fn live_session_ids(&self) -> Vec<u64> {
        self.terminals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| e.semantic_session_id != 0 && !e.exited.load(Ordering::SeqCst))
            .map(|(_, e)| e.semantic_session_id)
            .collect()
    }

    pub fn live_terminals(&self) -> Vec<pm_protocol::domain::WorkerTerminal> {
        self.terminals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| !entry.exited.load(Ordering::SeqCst))
            .map(|(terminal_id, entry)| pm_protocol::domain::WorkerTerminal {
                terminal_id: *terminal_id,
                generation: entry.generation,
                kind: entry.kind,
                state: pm_protocol::domain::TerminalRunState::Running,
                agent_resumable: false,
                transcript_available: false,
            })
            .collect()
    }

    /// Every running terminal that holds Program Status records, as a
    /// change that replaces whatever a mirror of it held.
    pub fn program_status_snapshots(&self) -> Vec<ProgramStatusUpdate> {
        self.terminals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| !entry.exited.load(Ordering::SeqCst))
            .filter_map(|(terminal_id, entry)| {
                let records = entry.program_status.as_ref()?.lock().unwrap().snapshot();
                (!records.is_empty()).then(|| ProgramStatusUpdate {
                    terminal_id: *terminal_id,
                    generation: entry.generation,
                    session_id: entry.semantic_session_id,
                    changes: crate::program_status::Changes {
                        reset: true,
                        records,
                        removed: Vec::new(),
                    },
                })
            })
            .collect()
    }

    pub fn live_terminal_ids(&self) -> Vec<u64> {
        self.terminals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| !entry.exited.load(Ordering::SeqCst))
            .map(|(terminal_id, _)| *terminal_id)
            .collect()
    }

    fn entry(&self, terminal_id: u64) -> Result<Arc<SessionEntry>, MuxError> {
        let entry = self
            .terminals
            .lock()
            .unwrap()
            .get(&terminal_id)
            .cloned()
            .ok_or(MuxError::NotRunning(terminal_id))?;
        if entry.exited.load(Ordering::SeqCst) {
            return Err(MuxError::NotRunning(terminal_id));
        }
        Ok(entry)
    }
}

/// Blocks until `pid` has exited, without collecting it.
///
/// `WNOWAIT` is what leaves the zombie in place: the caller learns the
/// child is gone while the kernel still reserves its pid, and reaps in a
/// second wait once it has finished with the pid. Returns on any error
/// it cannot retry, because the caller treats "cannot prove the pid is
/// still held" the same as released.
fn await_exit_leaving_zombie(pid: u32) {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    loop {
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            return;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
}

/// The Program Status side of one PTY reader: it removes OSC 7501 from
/// the output, answers the feature query on the terminal's input, and keeps
/// the terminal's records.
struct ProgramStatusReader {
    store: Arc<Mutex<RecordStore>>,
    scanner: Scanner,
    reply_tx: std::sync::mpsc::Sender<Bytes>,
    updates: mpsc::UnboundedSender<ProgramStatusUpdate>,
    terminal_id: u64,
    generation: u64,
    session_id: u64,
}

impl ProgramStatusReader {
    /// Returns the bytes viewers see.
    fn consume(&mut self, raw: &[u8]) -> Vec<u8> {
        let mut visible = Vec::with_capacity(raw.len());
        let mut events = Vec::new();
        self.scanner.feed(raw, &mut visible, &mut events);
        for event in events {
            self.handle(event);
        }
        visible
    }

    fn handle(&mut self, event: ScanEvent) {
        let changes = match event {
            ScanEvent::Sequence { body, terminator } => match parse_body(&body) {
                Ok(Report::Query) => {
                    debug!(
                        terminal = self.terminal_id,
                        "answering program status query"
                    );
                    let _ = self
                        .reply_tx
                        .send(Bytes::from_static(query_reply(terminator)));
                    return;
                }
                Ok(Report::Update(update)) => self
                    .store
                    .lock()
                    .unwrap()
                    .apply(update, crate::daemon::now_unix_ms()),
                Err(reason) => {
                    debug!(
                        terminal = self.terminal_id,
                        reason = reason.as_str(),
                        "ignored program status report"
                    );
                    return;
                }
            },
            ScanEvent::Oversized => {
                debug!(
                    terminal = self.terminal_id,
                    "discarded program status report over the size limit"
                );
                return;
            }
            ScanEvent::FullReset => self.store.lock().unwrap().reset(),
            ScanEvent::PromptStart => self.store.lock().unwrap().drop_transient(),
        };
        if changes.is_empty() {
            return;
        }
        let _ = self.updates.send(ProgramStatusUpdate {
            terminal_id: self.terminal_id,
            generation: self.generation,
            session_id: self.session_id,
            changes,
        });
    }
}

fn command_from_cmdline(cmdline: &[u8]) -> Option<String> {
    let mut args = cmdline
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned());
    let executable = args.next()?;
    let executable = std::path::Path::new(&executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&executable);
    let command = std::iter::once(executable.to_string())
        .chain(args)
        .collect::<Vec<_>>()
        .join(" ");
    let command: String = command
        .chars()
        .filter(|ch| !ch.is_control())
        .take(FOREGROUND_COMMAND_MAX_CHARS)
        .collect();
    (!command.trim().is_empty()).then_some(command)
}

fn terminal_command_sequence(title: &str) -> Bytes {
    Bytes::from(format!(
        "\x1b]{FOREGROUND_COMMAND_OSC};{FOREGROUND_COMMAND_OSC_PREFIX}{title}\x07"
    ))
}

/// Delivers `signal` to every process the PTY child leads. The child is
/// its own session and process-group leader (portable-pty calls setsid
/// before handing it the terminal), so its group holds the agent and
/// anything it spawned. Falls back to the pid alone if the child no
/// longer leads its group, so the signal is never aimed at an unrelated
/// group.
/// Signals a child's whole process group, or the child alone when it does not
/// lead one.
///
/// `kill(-pid)` without that check does not fail when the child is not a group
/// leader: it signals whichever group happens to have that id, which is some
/// other process tree.
pub(crate) fn signal_child_group(pid: u32, signal: libc::c_int) {
    let pid = pid as libc::pid_t;
    let leads_group = unsafe { libc::getpgid(pid) } == pid;
    let target = if leads_group { -pid } else { pid };
    unsafe {
        libc::kill(target, signal);
    }
}

fn foreground_process_group(entry: &SessionEntry) -> Option<i32> {
    #[cfg(unix)]
    return entry.master.lock().unwrap().process_group_leader();
    #[cfg(not(unix))]
    None
}

fn foreground_process_changed(
    previous: &mut Option<i32>,
    current: Option<i32>,
    viewed: bool,
) -> bool {
    if !viewed {
        *previous = None;
        return false;
    }
    if *previous == current {
        return false;
    }
    *previous = current;
    true
}

fn command_for_process_group(process_group: i32) -> Option<String> {
    #[cfg(target_os = "linux")]
    let cmdline = std::fs::read(format!("/proc/{process_group}/cmdline")).ok()?;
    #[cfg(all(unix, not(target_os = "linux")))]
    let cmdline = {
        let output = std::process::Command::new("ps")
            .args(["-o", "command=", "-p", &process_group.to_string()])
            .output()
            .ok()?;
        output.stdout
    };

    command_from_cmdline(&cmdline)
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use std::time::Duration;

    /// Whether `acquire` returns within a short wait, run on its own thread
    /// because a reader over budget is meant to block.
    fn acquires_promptly(flow: &Arc<OutputFlow>, bytes: usize) -> bool {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let flow = flow.clone();
        std::thread::spawn(move || {
            flow.acquire(bytes);
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(Duration::from_millis(200)).is_ok()
    }

    #[test]
    fn a_reader_over_budget_waits_until_the_consumer_credits_or_detaches() {
        let flow = Arc::new(OutputFlow::default());
        assert!(
            acquires_promptly(&flow, PRIMARY_CONSUMER_BUDGET_BYTES * 4),
            "nothing waits while no consumer is attached"
        );
        flow.enable();
        assert!(
            acquires_promptly(&flow, PRIMARY_CONSUMER_BUDGET_BYTES + 1),
            "the first read past the budget is admitted"
        );
        assert!(
            !acquires_promptly(&flow, 1),
            "the next read waits on the consumer"
        );
        flow.release(PRIMARY_CONSUMER_BUDGET_BYTES);
        assert!(
            acquires_promptly(&flow, 1),
            "credit from the consumer admits the waiting read"
        );
        assert!(acquires_promptly(&flow, PRIMARY_CONSUMER_BUDGET_BYTES));
        assert!(
            !acquires_promptly(&flow, 1),
            "over budget again, the reader waits"
        );
        flow.disable();
        assert!(
            acquires_promptly(&flow, PRIMARY_CONSUMER_BUDGET_BYTES * 4),
            "a detached consumer never holds the reader"
        );
    }
}

#[cfg(test)]
mod flow_release_tests {
    use super::*;
    use std::time::Duration;

    /// The worker stops a terminal's stream by aborting its task. An
    /// aborted task must still release the reader, or an agent writing
    /// past the budget blocks forever once its last viewer leaves.
    #[tokio::test]
    async fn an_aborted_consumer_releases_a_blocked_reader() {
        let flow = Arc::new(OutputFlow::default());
        let attachment = flow.attach();
        flow.acquire(PRIMARY_CONSUMER_BUDGET_BYTES + 1);
        let reader = {
            let flow = flow.clone();
            std::thread::spawn(move || flow.acquire(1))
        };
        let consumer = tokio::spawn(async move {
            let _attachment = attachment;
            std::future::pending::<()>().await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !reader.is_finished(),
            "the reader waits while the consumer holds the budget"
        );
        consumer.abort();
        let _ = consumer.await;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !reader.is_finished() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            reader.is_finished(),
            "an aborted consumer left the reader blocked"
        );
    }
}

#[cfg(test)]
mod env_tests {
    use super::*;

    fn spec(env: Vec<(String, String)>) -> CommandSpec {
        CommandSpec {
            program: "codex".into(),
            args: Vec::new(),
            env,
            cwd: std::path::PathBuf::from("/srv/repo"),
        }
    }

    /// The diagnostic is only worth logging if it reports what the child
    /// actually gets, which is the adapter's value when it sets one.
    #[test]
    fn an_override_is_what_the_child_sees() {
        let spec = spec(vec![("HOME".into(), "/home/agent".into())]);
        assert_eq!(effective_env(&spec, "HOME"), "/home/agent");
    }

    /// Otherwise the child inherits this process's value, which is the
    /// case that matters: a worker started with a different HOME than the
    /// operator's shell sends every agent looking somewhere else.
    #[test]
    fn without_an_override_the_process_value_is_reported() {
        let name = "PM_MUX_ENV_TEST_VALUE";
        // SAFETY: single-threaded test, and the variable is unique to it.
        unsafe { std::env::set_var(name, "/inherited") };
        assert_eq!(effective_env(&spec(Vec::new()), name), "/inherited");
        unsafe { std::env::remove_var(name) };
        assert_eq!(effective_env(&spec(Vec::new()), name), "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term_model::Ring;
    use pm_protocol::domain::ProgramStatusState;

    #[tokio::test]
    async fn launches_user_installed_harness_and_honors_session_path_override() {
        use std::os::unix::fs::PermissionsExt;
        const EXECUTABLE_MODE: u32 = 0o755;
        let home = tempfile::tempdir().unwrap();
        let local_bin = home.path().join(".local/bin");
        let custom_bin = home.path().join("custom/bin");
        for (dir, marker) in [
            (&local_bin, "user-installed"),
            (&custom_bin, "path-selected"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            let program = dir.join("claude");
            std::fs::write(&program, format!("#!/bin/sh\nprintf {marker} > \"$1\"\n")).unwrap();
            std::fs::set_permissions(program, std::fs::Permissions::from_mode(EXECUTABLE_MODE))
                .unwrap();
        }
        let (mux, mut channels) = Mux::new();
        let output = home.path().join("launched");
        for (index, (path, expected)) in [
            ("/usr/bin:/bin".to_string(), "user-installed"),
            (custom_bin.display().to_string(), "path-selected"),
        ]
        .into_iter()
        .enumerate()
        {
            let spec = CommandSpec {
                program: "claude".into(),
                args: vec![output.display().to_string()],
                env: vec![
                    ("HOME".into(), home.path().display().to_string()),
                    ("PATH".into(), path),
                ],
                cwd: home.path().into(),
            };
            mux.spawn(
                index as u64 + 1,
                1,
                index as u64 + 1,
                &spec,
                false,
                false,
                true,
                None,
                false,
            )
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), channels.exit_rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(std::fs::read_to_string(&output).unwrap(), expected);
        }
    }

    /// The map is keyed by terminal, and an agent's pid is what its own
    /// inbox is addressed by, so a lookup with a session id has to find
    /// nothing rather than whichever terminal happens to carry that
    /// number. The two id spaces are independent, so they collide.
    #[tokio::test]
    async fn a_child_pid_answers_to_a_terminal_id_and_not_to_a_session_id() {
        const TERMINAL_ID: u64 = 41;
        const SESSION_ID: u64 = 7;
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "sleep".into(),
            args: vec!["30".into()],
            env: vec![],
            cwd: std::env::temp_dir(),
        };
        mux.spawn(
            TERMINAL_ID,
            1,
            SESSION_ID,
            &spec,
            false,
            false,
            true,
            None,
            false,
        )
        .unwrap();
        assert!(mux.child_pid(TERMINAL_ID).is_some());
        assert_eq!(mux.child_pid(SESSION_ID), None);
        mux.kill(TERMINAL_ID).unwrap();
        tokio::time::timeout(Duration::from_secs(10), channels.exit_rx.recv())
            .await
            .unwrap()
            .unwrap();
    }

    /// Once the child is gone its pid belongs to the OS again, so
    /// handing it out would address whichever process is given it next.
    #[tokio::test]
    async fn an_exited_child_pid_is_no_longer_handed_out() {
        const TERMINAL_ID: u64 = 12;
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "true".into(),
            args: vec![],
            env: vec![],
            cwd: std::env::temp_dir(),
        };
        mux.spawn(TERMINAL_ID, 1, 99, &spec, false, false, true, None, false)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), channels.exit_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mux.child_pid(TERMINAL_ID), None);
    }

    #[test]
    fn osc9_marker_is_found_across_a_split() {
        // Marker split across two logical reads is still matched via
        // the carry buffer semantics (prefix kept between reads).
        let a = b"output \x1b]";
        let b = b"9;Codex wants to edit\x07 more";
        let mut carry = Vec::new();
        carry.extend_from_slice(a);
        assert!(find_subsequence(&carry, OSC9_MARKER).is_none());
        let keep = carry.len().min(OSC9_MARKER.len() - 1);
        carry.drain(..carry.len() - keep);
        carry.extend_from_slice(b);
        assert!(find_subsequence(&carry, OSC9_MARKER).is_some());
    }

    #[test]
    fn ring_keeps_only_the_newest_bytes() {
        let mut r = Ring::new(8);
        r.push(b"abcd");
        assert_eq!(&r.snapshot()[..], b"abcd");
        r.push(b"efgh");
        assert_eq!(&r.snapshot()[..], b"abcdefgh");
        r.push(b"XY");
        assert_eq!(&r.snapshot()[..], b"cdefghXY");
    }

    #[test]
    fn ring_handles_single_write_larger_than_capacity() {
        let mut r = Ring::new(4);
        r.push(b"0123456789");
        assert_eq!(&r.snapshot()[..], b"6789");
    }

    #[test]
    fn managed_ptys_advertise_color_support() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf '%s,%s,%s' \"$TERM\" \"$COLORTERM\" \"${NO_COLOR-unset}\"".into(),
            ],
            env: vec![(NO_COLOR_ENV.into(), "1".into())],
            cwd: std::env::temp_dir(),
        };
        mux.spawn(42, 1, 8, &spec, false, false, true, None, false)
            .unwrap();
        let exit = channels.exit_rx.blocking_recv().unwrap();
        assert!(find_subsequence(&exit.scrollback, b"xterm-256color,truecolor,unset").is_some());
        let activity = channels.activity_rx.blocking_recv().unwrap();
        assert_eq!(activity.terminal_id, 42);
        assert_eq!(activity.generation, 1);
        assert_eq!(activity.session_id, 8);
    }

    #[test]
    fn truecolor_off_leaves_colorterm_unset() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf '%s,%s' \"$TERM\" \"${COLORTERM-unset}\"".into(),
            ],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(43, 1, 9, &spec, false, false, false, None, false)
            .unwrap();
        let exit = channels.exit_rx.blocking_recv().unwrap();
        assert!(find_subsequence(&exit.scrollback, b"xterm-256color,unset").is_some());
    }

    #[test]
    fn foreground_command_uses_the_executable_name_and_sanitizes_controls() {
        assert_eq!(
            command_from_cmdline(b"/usr/bin/cargo\0nextest\0run\0"),
            Some("cargo nextest run".into())
        );
        assert_eq!(
            command_from_cmdline(b"/bin/sh\0-c\0echo bad\x1b]0;title\x07\0"),
            Some("sh -c echo bad]0;title".into())
        );
        assert_eq!(command_from_cmdline(b""), None);
    }

    #[test]
    fn foreground_command_is_a_private_osc_sequence() {
        assert_eq!(
            terminal_command_sequence("cargo build"),
            Bytes::from_static(b"\x1b]777;pm-command;cargo build\x07")
        );
    }

    #[test]
    fn a_group_with_an_unreadable_command_is_looked_up_again() {
        let mut previous = None;
        assert!(foreground_process_changed(&mut previous, Some(41), true));
        // The tracker forgets the group when the command is unreadable, so
        // the same group has to read as new on the next poll.
        previous = None;
        assert!(foreground_process_changed(&mut previous, Some(41), true));
    }

    #[test]
    fn foreground_command_lookup_runs_only_on_viewed_process_changes() {
        let mut previous = None;
        assert!(!foreground_process_changed(&mut previous, Some(7), false));
        assert_eq!(previous, None);
        assert!(foreground_process_changed(&mut previous, Some(7), true));
        assert!(!foreground_process_changed(&mut previous, Some(7), true));
        assert!(foreground_process_changed(&mut previous, Some(8), true));
        assert!(!foreground_process_changed(&mut previous, Some(8), false));
        assert_eq!(previous, None);
    }

    #[test]
    fn resize_updates_current_size() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sleep".into(),
            args: vec!["5".into()],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(45, 1, 11, &spec, false, false, true, None, false)
            .unwrap();
        assert_eq!(mux.current_size(45).unwrap(), (DEFAULT_COLS, DEFAULT_ROWS));
        mux.resize(45, 80, 24).unwrap();
        assert_eq!(mux.current_size(45).unwrap(), (80, 24));
        mux.kill(45).unwrap();
        assert_eq!(channels.exit_rx.blocking_recv().unwrap().terminal_id, 45);
    }

    #[test]
    fn spawn_with_initial_size_sets_geometry() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sleep".into(),
            args: vec!["5".into()],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(48, 1, 14, &spec, false, false, true, Some((152, 45)), false)
            .unwrap();
        assert_eq!(mux.current_size(48).unwrap(), (152, 45));
        mux.kill(48).unwrap();
        assert_eq!(channels.exit_rx.blocking_recv().unwrap().terminal_id, 48);
    }

    #[test]
    fn resize_noop_when_size_matches_current() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sleep".into(),
            args: vec!["5".into()],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(47, 1, 13, &spec, false, false, true, None, false)
            .unwrap();
        assert_eq!(mux.current_size(47).unwrap(), (DEFAULT_COLS, DEFAULT_ROWS));
        mux.resize(47, DEFAULT_COLS, DEFAULT_ROWS).unwrap();
        assert_eq!(mux.current_size(47).unwrap(), (DEFAULT_COLS, DEFAULT_ROWS));
        mux.kill(47).unwrap();
        assert_eq!(channels.exit_rx.blocking_recv().unwrap().terminal_id, 47);
    }

    /// Regression test for item #180: a degenerate resize (cols < MIN_COLS)
    /// must be rejected — the PTY size must not change, because the viewer
    /// computed a fit against a zero-width container and the value does not
    /// represent a real geometry.
    #[test]
    fn resize_rejects_degenerate_dimensions() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sleep".into(),
            args: vec!["5".into()],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(46, 1, 12, &spec, false, false, true, None, false)
            .unwrap();
        mux.resize(46, 80, 24).unwrap();
        assert_eq!(mux.current_size(46).unwrap(), (80, 24));

        // cols=2 is what FitAddon returns from a zero-width container.
        // The mux must reject it — the size should stay at 80x24.
        mux.resize(46, 2, 1).unwrap();
        assert_eq!(
            mux.current_size(46).unwrap(),
            (80, 24),
            "degenerate resize should be rejected, not applied"
        );

        // A valid resize should still work afterwards.
        mux.resize(46, 100, 30).unwrap();
        assert_eq!(mux.current_size(46).unwrap(), (100, 30));

        mux.kill(46).unwrap();
        assert_eq!(channels.exit_rx.blocking_recv().unwrap().terminal_id, 46);
    }

    #[tokio::test]
    async fn terminate_all_escalates_sigterm_resistant_sessions() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "trap '' TERM; printf 'READY\\n'; exec sleep 60".into(),
            ],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(44, 1, 10, &spec, false, false, true, None, false)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), channels.activity_rx.recv())
            .await
            .expect("SIGTERM-resistant session did not start")
            .unwrap();

        let started = std::time::Instant::now();
        assert_eq!(mux.terminate_all(), 1);
        let exit = tokio::time::timeout(
            KILL_GRACE_PERIOD + READER_DRAIN_TIMEOUT + Duration::from_secs(3),
            channels.exit_rx.recv(),
        )
        .await
        .expect("SIGTERM-resistant session was not reaped after escalation")
        .unwrap();
        assert!(started.elapsed() >= KILL_GRACE_PERIOD);
        assert_eq!(exit.terminal_id, 44);
    }

    /// Waits for a line the spawned script printed into the PTY and
    /// returns the pid it carries.
    fn child_pid_from_output(mux: &Mux, terminal_id: u64) -> libc::pid_t {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let (replay, _) = mux.attach(terminal_id).unwrap();
            let text = String::from_utf8_lossy(&replay);
            if let Some(rest) = text.split("CHILD ").nth(1) {
                if let Some(pid) = rest.split_whitespace().next() {
                    if let Ok(pid) = pid.parse::<libc::pid_t>() {
                        return pid;
                    }
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "script never reported its child pid"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn process_gone(pid: libc::pid_t) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(KILL_POLL_INTERVAL);
        }
    }

    /// Some agents launch through a wrapper that ignores termination
    /// signals and runs the real agent as a child in the same process
    /// group. Signalling the leader alone leaves that child running with
    /// the PTY open, so the session never exits and the row never clears.
    #[tokio::test]
    async fn kill_reaps_a_child_the_leader_shields_from_signals() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "trap '' TERM HUP; sleep 60 & printf 'CHILD %s\\n' \"$!\"; wait".into(),
            ],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(51, 1, 21, &spec, false, false, true, None, false)
            .unwrap();
        let child = child_pid_from_output(&mux, 51);

        assert_eq!(mux.terminate_all(), 1);
        let exit = tokio::time::timeout(
            KILL_GRACE_PERIOD + READER_DRAIN_TIMEOUT + Duration::from_secs(5),
            channels.exit_rx.recv(),
        )
        .await
        .expect("signal-shielded session was never reaped")
        .unwrap();
        assert_eq!(exit.terminal_id, 51);
        assert!(
            process_gone(child),
            "the shielded child outlived the killed session"
        );
    }

    /// The graceful SIGTERM has to reach the whole group too: with a
    /// leader that ignores it and a child that does not, the session must
    /// end without waiting for the escalation, so the agent still gets
    /// its chance to flush state instead of being SIGKILLed.
    #[tokio::test]
    async fn graceful_kill_reaches_a_child_of_a_signal_ignoring_leader() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "sleep 60 & printf 'CHILD %s\\n' \"$!\"; trap '' TERM HUP; wait".into(),
            ],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(52, 1, 22, &spec, false, false, true, None, false)
            .unwrap();
        child_pid_from_output(&mux, 52);

        let started = std::time::Instant::now();
        mux.kill(52).unwrap();
        let exit = tokio::time::timeout(
            KILL_GRACE_PERIOD + READER_DRAIN_TIMEOUT + Duration::from_secs(5),
            channels.exit_rx.recv(),
        )
        .await
        .expect("session was never reaped")
        .unwrap();
        assert_eq!(exit.terminal_id, 52);
        assert!(
            started.elapsed() < KILL_GRACE_PERIOD,
            "the graceful SIGTERM did not reach the child, so the session \
             only ended once the escalation fired"
        );
    }

    /// A shell script that probes for Program Status the way an agent does,
    /// prints the reply it read back in hex, then reports a state.
    fn program_status_probe() -> CommandSpec {
        CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                concat!(
                    "stty raw -echo; ",
                    "printf 'before\\033]7501;?\\033\\\\after\\n'; ",
                    "reply=$(head -c 10 | od -An -tx1 | tr -d ' \\n'); ",
                    "printf 'REPLY %s\\n' \"$reply\"; ",
                    "printf '\\033]7501;state=working:app=sh\\007'; ",
                    "printf '\\033]7501;state=done:id=task\\033\\\\'; ",
                    "printf 'end\\n'",
                )
                .into(),
            ],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        }
    }

    #[tokio::test]
    async fn program_status_answers_the_query_on_input_and_removes_the_sequences() {
        const TERMINAL_ID: u64 = 61;
        let (mux, mut channels) = Mux::new();
        mux.spawn(
            TERMINAL_ID,
            3,
            31,
            &program_status_probe(),
            false,
            false,
            true,
            None,
            true,
        )
        .unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(10), channels.exit_rx.recv())
            .await
            .expect("the probe never read a reply")
            .unwrap();
        let output = String::from_utf8_lossy(&exit.scrollback);
        assert!(output.contains("beforeafter"), "{output:?}");
        assert!(
            output.contains("REPLY 1b5d373530313b3f1b5c"),
            "the child read back something other than the reply: {output:?}"
        );
        assert!(!output.contains("7501"), "{output:?}");

        let mut updates = Vec::new();
        while let Ok(update) = channels.program_status_rx.try_recv() {
            updates.push(update);
        }
        assert!(updates
            .iter()
            .all(|u| u.terminal_id == TERMINAL_ID && u.generation == 3 && u.session_id == 31));
        let reported: Vec<_> = updates
            .iter()
            .flat_map(|u| u.changes.records.iter().map(|r| (r.id.clone(), r.state)))
            .collect();
        assert_eq!(
            reported,
            vec![
                (String::new(), ProgramStatusState::Working),
                ("task".into(), ProgramStatusState::Done)
            ]
        );
        let last = updates.last().unwrap();
        assert_eq!(
            last.changes.removed,
            vec![String::new()],
            "the working root ends with the process and the done record stays"
        );
    }

    /// Off, the terminal is exactly what it was: nothing is answered, so
    /// the PTY echoes nothing back, and every byte reaches viewers.
    #[tokio::test]
    async fn program_status_off_leaves_the_stream_untouched_and_answers_nothing() {
        const TERMINAL_ID: u64 = 62;
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf 'a\\033]7501;?\\033\\\\b\\033]7501;state=working\\007c'; sleep 1; printf 'end'"
                    .into(),
            ],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(TERMINAL_ID, 1, 32, &spec, false, false, true, None, false)
            .unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(10), channels.exit_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            &exit.scrollback[..],
            b"a\x1b]7501;?\x1b\\b\x1b]7501;state=working\x07cend"
        );
        assert!(channels.program_status_rx.try_recv().is_err());
    }

    /// Attach replays a terminal serialized from its emulator state rather
    /// than the raw bytes, so even with the setting off a replayed viewer
    /// never receives a query its own terminal could answer into the agent.
    #[test]
    fn an_attach_replay_never_carries_program_status_sequences() {
        let mut retained = Retained::new(SCROLLBACK_CAP_BYTES, DEFAULT_COLS, DEFAULT_ROWS);
        retained.push(b"one\x1b]7501;?\x1b\\two\x1b]7501;state=working\x07three");
        let replay = retained.model.snapshot();
        assert!(find_subsequence(&replay, b"7501").is_none());
        assert!(find_subsequence(&replay, b"onetwothree").is_some());
    }

    #[tokio::test]
    async fn program_status_snapshots_list_terminals_that_hold_records() {
        const TERMINAL_ID: u64 = 63;
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf '\\033]7501;state=blocked:kind=question\\007READY\\n'; sleep 30".into(),
            ],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(TERMINAL_ID, 2, 33, &spec, false, false, true, None, true)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), channels.program_status_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let snapshots = mux.program_status_snapshots();
        assert_eq!(snapshots.len(), 1);
        let snapshot = &snapshots[0];
        assert_eq!(
            (snapshot.terminal_id, snapshot.generation),
            (TERMINAL_ID, 2)
        );
        assert!(snapshot.changes.reset);
        assert_eq!(snapshot.changes.records.len(), 1);
        assert_eq!(
            snapshot.changes.records[0].state,
            ProgramStatusState::Blocked
        );
        mux.kill(TERMINAL_ID).unwrap();
        tokio::time::timeout(Duration::from_secs(10), channels.exit_rx.recv())
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tracked_terminal_emits_its_foreground_command_title() {
        let (mux, mut channels) = Mux::new();
        let spec = CommandSpec {
            program: "/bin/sleep".into(),
            args: vec!["2".into()],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(41, 1, 7, &spec, false, true, true, None, false)
            .unwrap();

        let (_, _viewer) = mux.attach(41).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let expected = b"\x1b]777;pm-command;sleep 2\x07";
        loop {
            let (replay, _) = mux.attach(41).unwrap();
            if find_subsequence(&replay, expected).is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "foreground command title was not emitted"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        mux.kill(41).unwrap();
        assert_eq!(channels.exit_rx.blocking_recv().unwrap().terminal_id, 41);
    }
}

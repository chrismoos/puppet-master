pub mod domain;
pub mod frame;
pub mod gateway;
pub mod terminal_frame;
pub mod worker_frame;

/// The newest worker protocol this build speaks. The number names the
/// capabilities a peer may use, not a compatibility boundary: a controller
/// registers a worker announcing any version and adapts to what that
/// version supports.
pub const WORKER_PROTOCOL_VERSION: u32 = 20;

/// First protocol in which a worker serves a published directory from a
/// loopback server it owns. An older worker cannot be asked to, so
/// publishing a directory on one of its sessions is refused with that
/// reason rather than left waiting for a message it will never decode.
pub const WORKER_PROTOCOL_DIR_SHARE: u32 = 17;

/// First protocol in which a worker's directory server answers HTTP/2
/// over cleartext, which lets the controller carry every request for one
/// share down a single multiplexed connection. A worker below this
/// serves HTTP/1.1 only, so the controller reuses a pool of HTTP/1
/// connections to it instead. The two are not interchangeable: the
/// tunnel carries no ALPN, so the controller has to know before it
/// writes a byte, and an h2 preface sent to an HTTP/1.1 server is
/// rejected rather than negotiated down.
///
/// Unlike the versions below it, this number identifies one capability
/// set and nothing else, because no release has ever announced it.
pub const WORKER_PROTOCOL_DIR_SHARE_H2: u32 = 18;

// Serving a directory at all came before answering it over HTTP/2, so a
// worker can have the one without the other and the two gates must stay
// separate. Collapsing them would send an HTTP/2 preface to a worker
// that only ever spoke HTTP/1.1.
const _: () = assert!(WORKER_PROTOCOL_DIR_SHARE_H2 > WORKER_PROTOCOL_DIR_SHARE);
const _: () = assert!(WORKER_PROTOCOL_DIR_SHARE_H2 <= WORKER_PROTOCOL_VERSION);

/// First protocol in which a worker delivers a message to a session's
/// agent over the agent's own inbound channel. The addresses that
/// channel needs only resolve on the host running the agent, so an
/// older worker cannot be asked: its sessions keep the terminal write.
///
/// Not 17, although that is the number the first attempt at this
/// capability announced: v0.7.0 shipped it, it was withdrawn, and 17
/// now means `WORKER_PROTOCOL_DIR_SHARE`. A worker from that release
/// announces 17 and can serve neither, so the gate has to sit above
/// every number either meaning has been released under.
pub const WORKER_PROTOCOL_AGENT_INBOX: u32 = 19;

const _: () = assert!(WORKER_PROTOCOL_AGENT_INBOX <= WORKER_PROTOCOL_VERSION);
const _: () = assert!(WORKER_PROTOCOL_AGENT_INBOX > WORKER_PROTOCOL_DIR_SHARE_H2);

/// First protocol in which a worker reports the container runtime and
/// container name it runs under. The controller cannot discover either —
/// nothing on the worker plane reaches the runtime — so a worker below
/// this simply reports neither and the Hosts page shows it as running on
/// the host, which is what every worker looked like before.
pub const WORKER_PROTOCOL_WORKER_RUNTIME: u32 = 20;

const _: () = assert!(WORKER_PROTOCOL_WORKER_RUNTIME <= WORKER_PROTOCOL_VERSION);
const _: () = assert!(WORKER_PROTOCOL_WORKER_RUNTIME > WORKER_PROTOCOL_AGENT_INBOX);

/// First protocol in which a worker sizes the PTY to the requested
/// initial geometry on spawn. An older worker ignores the fields and
/// spawns at default size, so the controller sends the geometry only to
/// workers that support it.
pub const WORKER_PROTOCOL_SPAWN_INITIAL_SIZE: u32 = 16;

/// First protocol in which a worker gives the PTY the size a terminal
/// attach asks for before taking the snapshot it relays. An older worker
/// ignores the size and snapshots at whatever size its PTY holds, as it
/// always has, so the controller sends the size to every worker.
pub const WORKER_PROTOCOL_ATTACH_SIZE: u32 = 15;

pub const WORKER_PROTOCOL_HARNESS_INSTALL: u32 = 14;

/// Adding a scalar to an existing message needs no send-side gate: an
/// older controller ignores a field it does not know, and an older
/// worker omits it, which decodes to the default the controller already
/// assumed. The constant is here so a future capability that cannot
/// degrade that way has a version to test against.
///
/// First protocol in which a worker reports that a turn ended while the
/// agent still had background work running. An older worker sends no
/// such flag, which reads as false: its sessions go idle at the end of a
/// turn the way they always have, rather than being held in a state the
/// worker cannot tell the controller to leave.
pub const WORKER_PROTOCOL_HOOK_BACKGROUND_WORK: u32 = 13;

/// First protocol in which a worker can launch Antigravity. Older workers do
/// not recognize its AgentKind wire value, so the controller refuses that
/// launch instead of sending them a message they cannot decode.
pub const WORKER_PROTOCOL_ANTIGRAVITY: u32 = 12;

/// First protocol in which a worker answers whether a configured project
/// path is usable on its filesystem. An older worker can only be asked
/// whether it is reachable, so a project path it holds is reported as
/// configured but unchecked rather than as usable or broken.
pub const WORKER_PROTOCOL_PATH_CHECK: u32 = 10;

/// First protocol in which a worker serves repository reads for a
/// review. An older worker holds a tree the controller cannot read, so
/// a review on it is refused with that reason rather than left to time
/// out against a message the worker will never understand.
pub const WORKER_PROTOCOL_REVIEW_REPO: u32 = 8;

/// First protocol in which a worker binds its own MCP relay in both link
/// directions. Earlier workers compose the controller's browser-plane URL
/// from the address fields in the registration reply.
pub const WORKER_PROTOCOL_LOCAL_MCP_RELAY: u32 = 6;

/// The protocol a registered peer announced, and what it can therefore do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerProtocol {
    version: u32,
}

impl PeerProtocol {
    pub fn version(self) -> u32 {
        self.version
    }

    /// Whether the peer speaks at least `version`, which is how new
    /// behaviour is gated: a peer that does not keeps the older path.
    pub fn supports(self, version: u32) -> bool {
        self.version >= version
    }

    /// Whether the peer needs the controller's MCP address in the
    /// registration reply because it cannot bind its own relay.
    pub fn needs_controller_mcp_address(self) -> bool {
        !self.supports(WORKER_PROTOCOL_LOCAL_MCP_RELAY)
    }

    /// Whether the peer accepts initial cols and rows on session/shell spawn.
    pub fn supports_spawn_initial_size(self) -> bool {
        self.supports(WORKER_PROTOCOL_SPAWN_INITIAL_SIZE)
    }

    /// Whether the peer can hand a message to a session's agent over
    /// the agent's own inbound channel.
    pub fn supports_agent_inbox(self) -> bool {
        self.supports(WORKER_PROTOCOL_AGENT_INBOX)
    }
}

/// Why a controller declines to talk to a worker at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolRefusal {
    pub announced: u32,
    pub ours: u32,
}

impl std::fmt::Display for ProtocolRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this worker speaks worker protocol {} but the controller speaks {} and cannot serve \
             a worker that old; update the worker, or the controller if the worker is newer",
            self.announced, self.ours
        )
    }
}

impl std::error::Error for ProtocolRefusal {}

/// Settles the protocol a registering worker will be served with. Every
/// version is accepted: the controller shims what an older worker lacks and
/// a newer worker only uses what this build offers. A floor for versions too
/// old to shim would be introduced here, as a deliberate choice, and would
/// be the only reason this returns `Err`.
pub fn negotiate_worker_protocol(announced: u32) -> Result<PeerProtocol, ProtocolRefusal> {
    Ok(PeerProtocol { version: announced })
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn every_announced_version_is_accepted() {
        for announced in [
            0,
            1,
            WORKER_PROTOCOL_VERSION - 1,
            WORKER_PROTOCOL_VERSION,
            WORKER_PROTOCOL_VERSION + 1,
        ] {
            assert_eq!(
                negotiate_worker_protocol(announced).map(PeerProtocol::version),
                Ok(announced)
            );
        }
    }

    #[test]
    fn peers_before_the_local_relay_get_the_controller_address() {
        let old = negotiate_worker_protocol(WORKER_PROTOCOL_LOCAL_MCP_RELAY - 1).unwrap();
        assert!(old.needs_controller_mcp_address());
        assert!(!old.supports(WORKER_PROTOCOL_VERSION));
        let current = negotiate_worker_protocol(WORKER_PROTOCOL_VERSION).unwrap();
        assert!(!current.needs_controller_mcp_address());
        assert!(current.supports(WORKER_PROTOCOL_LOCAL_MCP_RELAY));
        let newer = negotiate_worker_protocol(WORKER_PROTOCOL_VERSION + 1).unwrap();
        assert!(!newer.needs_controller_mcp_address());
    }

    #[test]
    fn peers_before_the_agent_inbox_keep_the_terminal_write() {
        let old = negotiate_worker_protocol(WORKER_PROTOCOL_AGENT_INBOX - 1).unwrap();
        assert!(!old.supports_agent_inbox());
        assert!(negotiate_worker_protocol(WORKER_PROTOCOL_AGENT_INBOX)
            .unwrap()
            .supports_agent_inbox());
        assert!(negotiate_worker_protocol(WORKER_PROTOCOL_VERSION)
            .unwrap()
            .supports_agent_inbox());
    }

    #[test]
    fn a_refusal_names_both_sides() {
        let text = ProtocolRefusal {
            announced: 2,
            ours: 6,
        }
        .to_string();
        assert!(text.contains("worker speaks worker protocol 2"));
        assert!(text.contains("controller speaks 6"));
        assert!(text.contains("update the worker"));
    }
}

mod convert;

pub mod wire {
    include!(concat!(env!("OUT_DIR"), "/pm.v1.rs"));
}

pub use convert::DecodeError;

//! Wire <-> domain conversions. Wire types never leave this crate's
//! boundary; anything unrepresentable in the domain (missing oneof,
//! unknown enum value) decodes to an error rather than a guess.

use prost::Message;

use crate::domain::*;
use crate::wire;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("protobuf decode: {0}")]
    Proto(#[from] prost::DecodeError),
    #[error("missing required field: {0}")]
    Missing(&'static str),
    #[error("unknown enum value for {0}: {1}")]
    UnknownEnum(&'static str, i32),
    #[error("value out of range for {0}")]
    OutOfRange(&'static str),
}

impl ClientEnvelope {
    pub fn encode_to_vec(&self) -> Vec<u8> {
        wire::ClientMessage::from(self.clone()).encode_to_vec()
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        wire::ClientMessage::decode(buf)?.try_into()
    }
}

impl ServerMsg {
    pub fn encode_to_vec(&self) -> Vec<u8> {
        wire::ServerMessage::from(self.clone()).encode_to_vec()
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        wire::ServerMessage::decode(buf)?.try_into()
    }
}

impl WorkerMsg {
    pub fn encode_to_vec(&self) -> Vec<u8> {
        wire::WorkerMessage::from(self.clone()).encode_to_vec()
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        wire::WorkerMessage::decode(buf)?.try_into()
    }
}

impl ControllerMsg {
    pub fn encode_to_vec(&self) -> Vec<u8> {
        wire::ControllerMessage::from(self.clone()).encode_to_vec()
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        wire::ControllerMessage::decode(buf)?.try_into()
    }
}

impl From<AgentKind> for wire::AgentKind {
    fn from(v: AgentKind) -> Self {
        match v {
            AgentKind::ClaudeCode => wire::AgentKind::ClaudeCode,
            AgentKind::Codex => wire::AgentKind::Codex,
            AgentKind::Gemini => wire::AgentKind::Gemini,
            AgentKind::OpenCode => wire::AgentKind::Opencode,
            AgentKind::Antigravity => wire::AgentKind::Antigravity,
            AgentKind::Test => wire::AgentKind::Test,
        }
    }
}

impl TryFrom<i32> for AgentKind {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::AgentKind::try_from(v) {
            Ok(wire::AgentKind::ClaudeCode) => Ok(AgentKind::ClaudeCode),
            Ok(wire::AgentKind::Codex) => Ok(AgentKind::Codex),
            Ok(wire::AgentKind::Gemini) => Ok(AgentKind::Gemini),
            Ok(wire::AgentKind::Opencode) => Ok(AgentKind::OpenCode),
            Ok(wire::AgentKind::Antigravity) => Ok(AgentKind::Antigravity),
            Ok(wire::AgentKind::Test) => Ok(AgentKind::Test),
            _ => Err(DecodeError::UnknownEnum("AgentKind", v)),
        }
    }
}

fn optional_agent(v: i32) -> Result<Option<AgentKind>, DecodeError> {
    if v == wire::AgentKind::Unspecified as i32 {
        Ok(None)
    } else {
        v.try_into().map(Some)
    }
}

impl From<ModelDialect> for wire::ModelDialect {
    fn from(v: ModelDialect) -> Self {
        match v {
            ModelDialect::AnthropicMessages => Self::AnthropicMessages,
            ModelDialect::OpenaiResponses => Self::OpenaiResponses,
            ModelDialect::GoogleGenai => Self::GoogleGenai,
        }
    }
}

impl TryFrom<i32> for ModelDialect {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::ModelDialect::try_from(v) {
            Ok(wire::ModelDialect::AnthropicMessages) => Ok(Self::AnthropicMessages),
            Ok(wire::ModelDialect::OpenaiResponses) => Ok(Self::OpenaiResponses),
            Ok(wire::ModelDialect::GoogleGenai) => Ok(Self::GoogleGenai),
            _ => Err(DecodeError::UnknownEnum("ModelDialect", v)),
        }
    }
}

impl From<Option<ModelProfileSource>> for wire::ModelProfileSource {
    fn from(v: Option<ModelProfileSource>) -> Self {
        match v {
            None => Self::Unspecified,
            Some(ModelProfileSource::Explicit) => Self::Explicit,
            Some(ModelProfileSource::Project) => Self::Project,
            Some(ModelProfileSource::Bucket) => Self::Bucket,
        }
    }
}

fn model_profile_source(v: i32) -> Result<Option<ModelProfileSource>, DecodeError> {
    match wire::ModelProfileSource::try_from(v) {
        Ok(wire::ModelProfileSource::Unspecified) => Ok(None),
        Ok(wire::ModelProfileSource::Explicit) => Ok(Some(ModelProfileSource::Explicit)),
        Ok(wire::ModelProfileSource::Project) => Ok(Some(ModelProfileSource::Project)),
        Ok(wire::ModelProfileSource::Bucket) => Ok(Some(ModelProfileSource::Bucket)),
        _ => Err(DecodeError::UnknownEnum("ModelProfileSource", v)),
    }
}

impl From<ModelProfileEndpoint> for wire::ModelProfileEndpoint {
    fn from(v: ModelProfileEndpoint) -> Self {
        wire::ModelProfileEndpoint {
            profile_id: v.profile_id,
            dialect: wire::ModelDialect::from(v.dialect) as i32,
            model: v.model,
            base_url: v.base_url,
            background_model: v.background_model,
        }
    }
}

impl TryFrom<wire::ModelProfileEndpoint> for ModelProfileEndpoint {
    type Error = DecodeError;
    fn try_from(v: wire::ModelProfileEndpoint) -> Result<Self, DecodeError> {
        Ok(ModelProfileEndpoint {
            profile_id: v.profile_id,
            dialect: v.dialect.try_into()?,
            model: v.model,
            base_url: v.base_url,
            background_model: v.background_model,
        })
    }
}

impl From<ModelProfile> for wire::ModelProfile {
    fn from(v: ModelProfile) -> Self {
        wire::ModelProfile {
            id: v.id,
            name: v.name,
            key_set: v.key_set,
            endpoints: v.endpoints.into_iter().map(Into::into).collect(),
            created_at_unix_ms: v.created_at_unix_ms,
            updated_at_unix_ms: v.updated_at_unix_ms,
        }
    }
}

impl TryFrom<wire::ModelProfile> for ModelProfile {
    type Error = DecodeError;
    fn try_from(v: wire::ModelProfile) -> Result<Self, DecodeError> {
        Ok(ModelProfile {
            id: v.id,
            name: v.name,
            key_set: v.key_set,
            endpoints: v
                .endpoints
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            created_at_unix_ms: v.created_at_unix_ms,
            updated_at_unix_ms: v.updated_at_unix_ms,
        })
    }
}

impl From<AgentDialects> for wire::AgentDialects {
    fn from(v: AgentDialects) -> Self {
        wire::AgentDialects {
            agent: wire::AgentKind::from(v.agent) as i32,
            dialects: v
                .dialects
                .into_iter()
                .map(|d| wire::ModelDialect::from(d) as i32)
                .collect(),
            supports_background_model: v.supports_background_model,
        }
    }
}

impl TryFrom<wire::AgentDialects> for AgentDialects {
    type Error = DecodeError;
    fn try_from(v: wire::AgentDialects) -> Result<Self, DecodeError> {
        Ok(AgentDialects {
            agent: v.agent.try_into()?,
            dialects: v
                .dialects
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            supports_background_model: v.supports_background_model,
        })
    }
}

impl From<ResolvedModelEndpoint> for wire::ResolvedModelEndpoint {
    fn from(v: ResolvedModelEndpoint) -> Self {
        wire::ResolvedModelEndpoint {
            dialect: wire::ModelDialect::from(v.dialect) as i32,
            model: v.model,
            base_url: v.base_url,
            background_model: v.background_model,
            api_key: v.api_key,
            provider_name: v.provider_name,
        }
    }
}

impl TryFrom<wire::ResolvedModelEndpoint> for ResolvedModelEndpoint {
    type Error = DecodeError;
    fn try_from(v: wire::ResolvedModelEndpoint) -> Result<Self, DecodeError> {
        Ok(ResolvedModelEndpoint {
            dialect: v.dialect.try_into()?,
            model: v.model,
            base_url: v.base_url,
            background_model: v.background_model,
            api_key: v.api_key,
            provider_name: v.provider_name,
        })
    }
}

impl From<AgentSelectionSource> for wire::AgentSelectionSource {
    fn from(v: AgentSelectionSource) -> Self {
        match v {
            AgentSelectionSource::Explicit => Self::Explicit,
            AgentSelectionSource::Project => Self::Project,
            AgentSelectionSource::Bucket => Self::Bucket,
            AgentSelectionSource::Fallback => Self::Fallback,
        }
    }
}

fn agent_selection_source(v: i32) -> Result<AgentSelectionSource, DecodeError> {
    match wire::AgentSelectionSource::try_from(v) {
        // Older peers did not send this field; their agent was required.
        Ok(wire::AgentSelectionSource::Unspecified | wire::AgentSelectionSource::Explicit) => {
            Ok(AgentSelectionSource::Explicit)
        }
        Ok(wire::AgentSelectionSource::Project) => Ok(AgentSelectionSource::Project),
        Ok(wire::AgentSelectionSource::Bucket) => Ok(AgentSelectionSource::Bucket),
        Ok(wire::AgentSelectionSource::Fallback) => Ok(AgentSelectionSource::Fallback),
        _ => Err(DecodeError::UnknownEnum("AgentSelectionSource", v)),
    }
}

impl From<SessionState> for wire::SessionState {
    fn from(v: SessionState) -> Self {
        match v {
            SessionState::Starting => wire::SessionState::Starting,
            SessionState::Working => wire::SessionState::Working,
            SessionState::NeedsInput => wire::SessionState::NeedsInput,
            SessionState::Idle => wire::SessionState::Idle,
            SessionState::Exited => wire::SessionState::Exited,
            SessionState::Failed => wire::SessionState::Failed,
            SessionState::AwaitingWorker => wire::SessionState::AwaitingWorker,
        }
    }
}

impl TryFrom<i32> for SecurityNoticeKind {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, Self::Error> {
        match wire::SecurityNoticeKind::try_from(v) {
            Ok(wire::SecurityNoticeKind::DeviceEnrolled) => Ok(SecurityNoticeKind::DeviceEnrolled),
            Ok(wire::SecurityNoticeKind::HostEnrolled) => Ok(SecurityNoticeKind::HostEnrolled),
            Ok(wire::SecurityNoticeKind::HostKeyReplaced) => {
                Ok(SecurityNoticeKind::HostKeyReplaced)
            }
            Ok(wire::SecurityNoticeKind::InstructionsRewritten) => {
                Ok(SecurityNoticeKind::InstructionsRewritten)
            }
            _ => Err(DecodeError::UnknownEnum("SecurityNoticeKind", v)),
        }
    }
}

impl TryFrom<i32> for SessionAlertKind {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::SessionAlertKind::try_from(v) {
            Ok(wire::SessionAlertKind::NeedsInput) => Ok(SessionAlertKind::NeedsInput),
            Ok(wire::SessionAlertKind::Failed) => Ok(SessionAlertKind::Failed),
            Ok(wire::SessionAlertKind::Completed) => Ok(SessionAlertKind::Completed),
            _ => Err(DecodeError::UnknownEnum("SessionAlertKind", v)),
        }
    }
}

impl TryFrom<i32> for SessionState {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::SessionState::try_from(v) {
            Ok(wire::SessionState::Starting) => Ok(SessionState::Starting),
            Ok(wire::SessionState::Working) => Ok(SessionState::Working),
            Ok(wire::SessionState::NeedsInput) => Ok(SessionState::NeedsInput),
            Ok(wire::SessionState::Idle) => Ok(SessionState::Idle),
            Ok(wire::SessionState::Exited) => Ok(SessionState::Exited),
            Ok(wire::SessionState::Failed) => Ok(SessionState::Failed),
            Ok(wire::SessionState::AwaitingWorker) => Ok(SessionState::AwaitingWorker),
            _ => Err(DecodeError::UnknownEnum("SessionState", v)),
        }
    }
}

impl From<ProgramStatusRecord> for wire::ProgramStatusRecord {
    fn from(v: ProgramStatusRecord) -> Self {
        wire::ProgramStatusRecord {
            id: v.id,
            state: match v.state {
                ProgramStatusState::Idle => wire::ProgramStatusState::Idle,
                ProgramStatusState::Working => wire::ProgramStatusState::Working,
                ProgramStatusState::Done => wire::ProgramStatusState::Done,
                ProgramStatusState::Blocked => wire::ProgramStatusState::Blocked,
                ProgramStatusState::Error => wire::ProgramStatusState::Error,
            } as i32,
            kind: match v.kind {
                None => wire::ProgramStatusKind::Unspecified,
                Some(ProgramStatusKind::Permission) => wire::ProgramStatusKind::Permission,
                Some(ProgramStatusKind::Question) => wire::ProgramStatusKind::Question,
                Some(ProgramStatusKind::Auth) => wire::ProgramStatusKind::Auth,
            } as i32,
            progress: v.progress,
            app: v.app,
            title: v.title,
            msg: v.msg,
            updated_at_unix_ms: v.updated_at_unix_ms,
        }
    }
}

/// A record whose state this build does not know is dropped rather than
/// failing the message that carries it, so a newer peer's records never
/// make a session undecodable.
fn program_status_records(records: Vec<wire::ProgramStatusRecord>) -> Vec<ProgramStatusRecord> {
    records
        .into_iter()
        .filter_map(|v| {
            let state = match wire::ProgramStatusState::try_from(v.state) {
                Ok(wire::ProgramStatusState::Idle) => ProgramStatusState::Idle,
                Ok(wire::ProgramStatusState::Working) => ProgramStatusState::Working,
                Ok(wire::ProgramStatusState::Done) => ProgramStatusState::Done,
                Ok(wire::ProgramStatusState::Blocked) => ProgramStatusState::Blocked,
                Ok(wire::ProgramStatusState::Error) => ProgramStatusState::Error,
                _ => return None,
            };
            let kind = match wire::ProgramStatusKind::try_from(v.kind) {
                Ok(wire::ProgramStatusKind::Permission) => Some(ProgramStatusKind::Permission),
                Ok(wire::ProgramStatusKind::Question) => Some(ProgramStatusKind::Question),
                Ok(wire::ProgramStatusKind::Auth) => Some(ProgramStatusKind::Auth),
                _ => None,
            };
            Some(ProgramStatusRecord {
                id: v.id,
                state,
                kind,
                progress: v.progress,
                app: v.app,
                title: v.title,
                msg: v.msg,
                updated_at_unix_ms: v.updated_at_unix_ms,
            })
        })
        .collect()
}

impl From<SessionRole> for wire::SessionRole {
    fn from(v: SessionRole) -> Self {
        match v {
            SessionRole::Worker => Self::Worker,
            SessionRole::Supervisor => Self::Supervisor,
        }
    }
}
impl TryFrom<i32> for SessionRole {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, Self::Error> {
        match wire::SessionRole::try_from(v) {
            Ok(wire::SessionRole::Worker) => Ok(Self::Worker),
            Ok(wire::SessionRole::Supervisor) => Ok(Self::Supervisor),
            _ => Err(DecodeError::UnknownEnum("SessionRole", v)),
        }
    }
}
impl From<InstructionTarget> for wire::InstructionTarget {
    fn from(v: InstructionTarget) -> Self {
        match v {
            InstructionTarget::All => Self::All,
            InstructionTarget::Worker => Self::Worker,
            InstructionTarget::Supervisor => Self::Supervisor,
        }
    }
}
impl TryFrom<i32> for InstructionTarget {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, Self::Error> {
        match wire::InstructionTarget::try_from(v) {
            Ok(wire::InstructionTarget::All) => Ok(Self::All),
            Ok(wire::InstructionTarget::Worker) => Ok(Self::Worker),
            Ok(wire::InstructionTarget::Supervisor) => Ok(Self::Supervisor),
            _ => Err(DecodeError::UnknownEnum("InstructionTarget", v)),
        }
    }
}
impl From<InstructionLayer> for wire::InstructionLayer {
    fn from(v: InstructionLayer) -> Self {
        Self {
            id: v.id,
            bucket_id: v.bucket_id,
            project_id: v.project_id,
            target: wire::InstructionTarget::from(v.target) as i32,
            markdown: v.markdown,
            revision: v.revision,
            updated_at_unix_ms: v.updated_at_unix_ms,
            updated_by_session_id: v.updated_by_session_id,
        }
    }
}
impl TryFrom<wire::InstructionLayer> for InstructionLayer {
    type Error = DecodeError;
    fn try_from(v: wire::InstructionLayer) -> Result<Self, Self::Error> {
        Ok(Self {
            id: v.id,
            bucket_id: v.bucket_id,
            project_id: v.project_id,
            target: v.target.try_into()?,
            markdown: v.markdown,
            revision: v.revision,
            updated_at_unix_ms: v.updated_at_unix_ms,
            updated_by_session_id: v.updated_by_session_id,
        })
    }
}

impl From<Worker> for wire::Worker {
    fn from(v: Worker) -> Self {
        wire::Worker {
            id: v.id,
            name: v.name,
            hostname: v.hostname,
            platform: v.platform,
            online: v.online,
            default_project_root: v.default_project_root,
            last_seen_at_unix_ms: v.last_seen_at_unix_ms,
            pm_version: v.pm_version,
            runtime: v.runtime,
            container: v.container,
            connect_mode: wire::ConnectMode::from(v.connect_mode) as i32,
            endpoint: v.endpoint,
        }
    }
}

impl From<ConnectMode> for wire::ConnectMode {
    fn from(v: ConnectMode) -> Self {
        match v {
            ConnectMode::Dial => Self::Dial,
            ConnectMode::Accept => Self::Accept,
        }
    }
}

impl TryFrom<i32> for ConnectMode {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, Self::Error> {
        match wire::ConnectMode::try_from(v) {
            Ok(wire::ConnectMode::Dial) => Ok(Self::Dial),
            Ok(wire::ConnectMode::Accept) => Ok(Self::Accept),
            _ => Err(DecodeError::UnknownEnum("ConnectMode", v)),
        }
    }
}

impl TryFrom<wire::Worker> for Worker {
    type Error = DecodeError;
    fn try_from(v: wire::Worker) -> Result<Self, DecodeError> {
        Ok(Worker {
            id: v.id,
            name: v.name,
            hostname: v.hostname,
            platform: v.platform,
            online: v.online,
            default_project_root: v.default_project_root,
            last_seen_at_unix_ms: v.last_seen_at_unix_ms,
            pm_version: v.pm_version,
            runtime: v.runtime,
            container: v.container,
            connect_mode: v.connect_mode.try_into()?,
            endpoint: v.endpoint,
        })
    }
}

impl From<Bucket> for wire::Bucket {
    fn from(v: Bucket) -> Self {
        wire::Bucket {
            id: v.id,
            name: v.name,
            position: v.position,
            permission_mode: wire::PermissionMode::from(v.permission_mode) as i32,
            default_worker_id: v.default_worker_id,
            allowed_worker_ids: v.allowed_worker_ids,
            is_default: v.is_default,
            default_agent: v
                .default_agent
                .map(|agent| wire::AgentKind::from(agent) as i32),
            model_profile_id: v.model_profile_id,
        }
    }
}

impl TryFrom<wire::Bucket> for Bucket {
    type Error = DecodeError;
    fn try_from(v: wire::Bucket) -> Result<Self, DecodeError> {
        Ok(Bucket {
            id: v.id,
            name: v.name,
            position: v.position,
            permission_mode: v.permission_mode.try_into()?,
            default_worker_id: v.default_worker_id,
            allowed_worker_ids: v.allowed_worker_ids,
            is_default: v.is_default,
            default_agent: v.default_agent.map(optional_agent).transpose()?.flatten(),
            model_profile_id: v.model_profile_id,
        })
    }
}

impl From<Project> for wire::Project {
    fn from(v: Project) -> Self {
        wire::Project {
            id: v.id,
            bucket_id: v.bucket_id,
            name: v.name,
            path: v.path,
            permission_mode: wire::PermissionMode::from(v.permission_mode) as i32,
            worker_id: v.worker_id,
            allowed_worker_ids: v.allowed_worker_ids,
            worker_paths: v
                .worker_paths
                .into_iter()
                .map(|path| wire::ProjectPath {
                    worker_id: path.worker_id,
                    path: path.path,
                })
                .collect(),
            default_agent: v
                .default_agent
                .map(|agent| wire::AgentKind::from(agent) as i32),
            model_profile_id: v.model_profile_id,
        }
    }
}

impl TryFrom<wire::Project> for Project {
    type Error = DecodeError;
    fn try_from(v: wire::Project) -> Result<Self, DecodeError> {
        Ok(Project {
            id: v.id,
            bucket_id: v.bucket_id,
            name: v.name,
            path: v.path,
            permission_mode: v.permission_mode.try_into()?,
            worker_id: v.worker_id,
            allowed_worker_ids: v.allowed_worker_ids,
            worker_paths: v
                .worker_paths
                .into_iter()
                .map(|path| ProjectPath {
                    worker_id: path.worker_id,
                    path: path.path,
                })
                .collect(),
            default_agent: v.default_agent.map(optional_agent).transpose()?.flatten(),
            model_profile_id: v.model_profile_id,
        })
    }
}

impl From<TerminalKind> for wire::TerminalKind {
    fn from(v: TerminalKind) -> Self {
        match v {
            TerminalKind::Agent => Self::Agent,
            TerminalKind::Shell => Self::Shell,
        }
    }
}
impl TryFrom<i32> for TerminalKind {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, Self::Error> {
        match wire::TerminalKind::try_from(v) {
            Ok(wire::TerminalKind::Agent) => Ok(Self::Agent),
            Ok(wire::TerminalKind::Shell) => Ok(Self::Shell),
            _ => Err(DecodeError::UnknownEnum("TerminalKind", v)),
        }
    }
}
impl From<TerminalRunState> for wire::TerminalRunState {
    fn from(v: TerminalRunState) -> Self {
        match v {
            TerminalRunState::Starting => Self::Starting,
            TerminalRunState::Running => Self::Running,
            TerminalRunState::Exited => Self::Exited,
            TerminalRunState::Failed => Self::Failed,
        }
    }
}
impl TryFrom<i32> for TerminalRunState {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, Self::Error> {
        match wire::TerminalRunState::try_from(v) {
            Ok(wire::TerminalRunState::Starting) => Ok(Self::Starting),
            Ok(wire::TerminalRunState::Running) => Ok(Self::Running),
            Ok(wire::TerminalRunState::Exited) => Ok(Self::Exited),
            Ok(wire::TerminalRunState::Failed) => Ok(Self::Failed),
            _ => Err(DecodeError::UnknownEnum("TerminalRunState", v)),
        }
    }
}
impl From<Terminal> for wire::Terminal {
    fn from(v: Terminal) -> Self {
        Self {
            id: v.id,
            session_id: v.session_id,
            kind: wire::TerminalKind::from(v.kind) as i32,
            title: v.title,
            cwd: v.cwd,
            created_at_unix_ms: v.created_at_unix_ms,
            generation: v.generation,
            state: wire::TerminalRunState::from(v.state) as i32,
            started_at_unix_ms: v.started_at_unix_ms,
            ended_at_unix_ms: v.ended_at_unix_ms,
            exit_code: v.exit_code,
            scrollback_available: v.scrollback_available,
        }
    }
}
impl TryFrom<wire::Terminal> for Terminal {
    type Error = DecodeError;
    fn try_from(v: wire::Terminal) -> Result<Self, Self::Error> {
        Ok(Self {
            id: v.id,
            session_id: v.session_id,
            kind: v.kind.try_into()?,
            title: v.title,
            cwd: v.cwd,
            created_at_unix_ms: v.created_at_unix_ms,
            generation: v.generation,
            state: v.state.try_into()?,
            started_at_unix_ms: v.started_at_unix_ms,
            ended_at_unix_ms: v.ended_at_unix_ms,
            exit_code: v.exit_code,
            scrollback_available: v.scrollback_available,
        })
    }
}

impl From<ContextKind> for wire::ContextKind {
    fn from(v: ContextKind) -> Self {
        match v {
            ContextKind::Text => Self::Text,
            ContextKind::Code => Self::Code,
            ContextKind::Url => Self::Url,
            ContextKind::Badge => Self::Badge,
            ContextKind::Metric => Self::Metric,
            ContextKind::Progress => Self::Progress,
            ContextKind::Timestamp => Self::Timestamp,
        }
    }
}
impl From<i32> for ContextKind {
    fn from(v: i32) -> Self {
        match wire::ContextKind::try_from(v) {
            Ok(wire::ContextKind::Code) => Self::Code,
            Ok(wire::ContextKind::Url) => Self::Url,
            Ok(wire::ContextKind::Badge) => Self::Badge,
            Ok(wire::ContextKind::Metric) => Self::Metric,
            Ok(wire::ContextKind::Progress) => Self::Progress,
            Ok(wire::ContextKind::Timestamp) => Self::Timestamp,
            _ => Self::Text,
        }
    }
}
impl From<ContextSeverity> for wire::ContextSeverity {
    fn from(v: ContextSeverity) -> Self {
        match v {
            ContextSeverity::Neutral => Self::Neutral,
            ContextSeverity::Info => Self::Info,
            ContextSeverity::Good => Self::Good,
            ContextSeverity::Warn => Self::Warn,
            ContextSeverity::Bad => Self::Bad,
        }
    }
}
impl From<i32> for ContextSeverity {
    fn from(v: i32) -> Self {
        match wire::ContextSeverity::try_from(v) {
            Ok(wire::ContextSeverity::Info) => Self::Info,
            Ok(wire::ContextSeverity::Good) => Self::Good,
            Ok(wire::ContextSeverity::Warn) => Self::Warn,
            Ok(wire::ContextSeverity::Bad) => Self::Bad,
            _ => Self::Neutral,
        }
    }
}
impl From<ContextField> for wire::ContextField {
    fn from(v: ContextField) -> Self {
        Self {
            key: v.key,
            label: v.label,
            value: v.value,
            kind: wire::ContextKind::from(v.kind) as i32,
            severity: wire::ContextSeverity::from(v.severity) as i32,
        }
    }
}
impl From<wire::ContextField> for ContextField {
    fn from(v: wire::ContextField) -> Self {
        Self {
            key: v.key,
            label: v.label,
            value: v.value,
            kind: v.kind.into(),
            severity: v.severity.into(),
        }
    }
}
impl From<SessionContext> for wire::SessionContext {
    fn from(v: SessionContext) -> Self {
        Self {
            session_id: v.session_id,
            glance: v.glance.into_iter().map(Into::into).collect(),
            detail: v.detail.into_iter().map(Into::into).collect(),
        }
    }
}
impl From<wire::SessionContext> for SessionContext {
    fn from(v: wire::SessionContext) -> Self {
        Self {
            session_id: v.session_id,
            glance: v.glance.into_iter().map(Into::into).collect(),
            detail: v.detail.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<SessionForward> for wire::SessionForward {
    fn from(v: SessionForward) -> Self {
        Self {
            id: v.id,
            session_id: v.session_id,
            worker_port: v.worker_port as u32,
            listener_port: v.listener_port as u32,
            slug: v.slug,
            label: v.label,
            scheme: v.scheme,
            created_at_unix_ms: v.created_at_unix_ms,
            url: v.url,
            target_reachable: v.target_reachable,
            source_path: v.source_path,
        }
    }
}

impl TryFrom<wire::SessionForward> for SessionForward {
    type Error = DecodeError;
    fn try_from(v: wire::SessionForward) -> Result<Self, DecodeError> {
        Ok(Self {
            id: v.id,
            session_id: v.session_id,
            worker_port: u16::try_from(v.worker_port)
                .map_err(|_| DecodeError::OutOfRange("worker_port"))?,
            listener_port: u16::try_from(v.listener_port)
                .map_err(|_| DecodeError::OutOfRange("listener_port"))?,
            slug: v.slug,
            label: v.label,
            scheme: v.scheme,
            created_at_unix_ms: v.created_at_unix_ms,
            url: v.url,
            target_reachable: v.target_reachable,
            source_path: v.source_path,
        })
    }
}

impl From<ItemStatus> for wire::ItemStatus {
    fn from(v: ItemStatus) -> Self {
        match v {
            ItemStatus::Inbox => Self::Inbox,
            ItemStatus::Planned => Self::Planned,
            ItemStatus::InProgress => Self::InProgress,
            ItemStatus::Blocked => Self::Blocked,
            ItemStatus::BlockedExternal => Self::BlockedExternal,
            ItemStatus::Done => Self::Done,
            ItemStatus::Dropped => Self::Dropped,
        }
    }
}

impl TryFrom<i32> for ItemStatus {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::ItemStatus::try_from(v) {
            Ok(wire::ItemStatus::Inbox) => Ok(ItemStatus::Inbox),
            Ok(wire::ItemStatus::Planned) => Ok(ItemStatus::Planned),
            Ok(wire::ItemStatus::InProgress) => Ok(ItemStatus::InProgress),
            Ok(wire::ItemStatus::Blocked) => Ok(ItemStatus::Blocked),
            Ok(wire::ItemStatus::BlockedExternal) => Ok(ItemStatus::BlockedExternal),
            Ok(wire::ItemStatus::Done) => Ok(ItemStatus::Done),
            Ok(wire::ItemStatus::Dropped) => Ok(ItemStatus::Dropped),
            _ => Err(DecodeError::UnknownEnum("ItemStatus", v)),
        }
    }
}

impl From<ItemSummaryFilter> for wire::ItemSummaryFilter {
    fn from(value: ItemSummaryFilter) -> Self {
        match value {
            ItemSummaryFilter::NeedsYou => Self::NeedsYou,
            ItemSummaryFilter::InProgress => Self::InProgress,
            ItemSummaryFilter::Planned => Self::Planned,
            ItemSummaryFilter::BlockedExternal => Self::BlockedExternal,
            ItemSummaryFilter::DoneRecently => Self::DoneRecently,
            ItemSummaryFilter::LiveLinked => Self::LiveLinked,
        }
    }
}

impl TryFrom<i32> for ItemSummaryFilter {
    type Error = DecodeError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match wire::ItemSummaryFilter::try_from(value) {
            Ok(wire::ItemSummaryFilter::NeedsYou) => Ok(Self::NeedsYou),
            Ok(wire::ItemSummaryFilter::InProgress) => Ok(Self::InProgress),
            Ok(wire::ItemSummaryFilter::Planned) => Ok(Self::Planned),
            Ok(wire::ItemSummaryFilter::BlockedExternal) => Ok(Self::BlockedExternal),
            Ok(wire::ItemSummaryFilter::DoneRecently) => Ok(Self::DoneRecently),
            Ok(wire::ItemSummaryFilter::LiveLinked) => Ok(Self::LiveLinked),
            _ => Err(DecodeError::UnknownEnum("ItemSummaryFilter", value)),
        }
    }
}

impl From<ItemPriority> for wire::ItemPriority {
    fn from(v: ItemPriority) -> Self {
        match v {
            ItemPriority::Urgent => Self::Urgent,
            ItemPriority::High => Self::High,
            ItemPriority::Normal => Self::Normal,
            ItemPriority::Low => Self::Low,
        }
    }
}

impl TryFrom<i32> for ItemPriority {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::ItemPriority::try_from(v) {
            Ok(wire::ItemPriority::Urgent) => Ok(ItemPriority::Urgent),
            Ok(wire::ItemPriority::High) => Ok(ItemPriority::High),
            Ok(wire::ItemPriority::Normal) => Ok(ItemPriority::Normal),
            Ok(wire::ItemPriority::Low) => Ok(ItemPriority::Low),
            _ => Err(DecodeError::UnknownEnum("ItemPriority", v)),
        }
    }
}

impl From<ItemSourceKind> for wire::ItemSourceKind {
    fn from(v: ItemSourceKind) -> Self {
        match v {
            ItemSourceKind::Email => Self::Email,
            ItemSourceKind::Slack => Self::Slack,
            ItemSourceKind::Github => Self::Github,
            ItemSourceKind::Jira => Self::Jira,
            ItemSourceKind::Teams => Self::Teams,
            ItemSourceKind::Telegram => Self::Telegram,
            ItemSourceKind::Human => Self::Human,
            ItemSourceKind::Agent => Self::Agent,
            ItemSourceKind::Other => Self::Other,
        }
    }
}

impl TryFrom<i32> for ItemSourceKind {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::ItemSourceKind::try_from(v) {
            Ok(wire::ItemSourceKind::Email) => Ok(ItemSourceKind::Email),
            Ok(wire::ItemSourceKind::Slack) => Ok(ItemSourceKind::Slack),
            Ok(wire::ItemSourceKind::Github) => Ok(ItemSourceKind::Github),
            Ok(wire::ItemSourceKind::Jira) => Ok(ItemSourceKind::Jira),
            Ok(wire::ItemSourceKind::Teams) => Ok(ItemSourceKind::Teams),
            Ok(wire::ItemSourceKind::Telegram) => Ok(ItemSourceKind::Telegram),
            Ok(wire::ItemSourceKind::Human) => Ok(ItemSourceKind::Human),
            Ok(wire::ItemSourceKind::Agent) => Ok(ItemSourceKind::Agent),
            Ok(wire::ItemSourceKind::Other) => Ok(ItemSourceKind::Other),
            _ => Err(DecodeError::UnknownEnum("ItemSourceKind", v)),
        }
    }
}

/// Optional-enum encoding for item writes: UNSPECIFIED carries "leave
/// the stored value untouched".
fn item_status_opt(v: i32) -> Result<Option<ItemStatus>, DecodeError> {
    if v == 0 {
        return Ok(None);
    }
    Ok(Some(v.try_into()?))
}

fn item_priority_opt(v: i32) -> Result<Option<ItemPriority>, DecodeError> {
    if v == 0 {
        return Ok(None);
    }
    Ok(Some(v.try_into()?))
}

fn item_source_kind_opt(v: i32) -> Result<Option<ItemSourceKind>, DecodeError> {
    if v == 0 {
        return Ok(None);
    }
    Ok(Some(v.try_into()?))
}

impl From<Item> for wire::Item {
    fn from(v: Item) -> Self {
        wire::Item {
            id: v.id,
            bucket_id: v.bucket_id,
            project_id: v.project_id,
            external_key: v.external_key,
            title: v.title,
            body: v.body,
            question: v.question,
            status: wire::ItemStatus::from(v.status) as i32,
            priority: wire::ItemPriority::from(v.priority) as i32,
            source_kind: wire::ItemSourceKind::from(v.source_kind) as i32,
            source_detail: v.source_detail,
            url: v.url,
            due_at_unix_ms: v.due_at_unix_ms,
            snoozed_until_unix_ms: v.snoozed_until_unix_ms,
            created_by_session_id: v.created_by_session_id,
            created_at_unix_ms: v.created_at_unix_ms,
            updated_at_unix_ms: v.updated_at_unix_ms,
            done_at_unix_ms: v.done_at_unix_ms,
            blocked_by: v.blocked_by,
            session_ids: v.session_ids,
        }
    }
}

impl TryFrom<wire::Item> for Item {
    type Error = DecodeError;
    fn try_from(v: wire::Item) -> Result<Self, DecodeError> {
        Ok(Item {
            id: v.id,
            bucket_id: v.bucket_id,
            project_id: v.project_id,
            external_key: v.external_key,
            title: v.title,
            body: v.body,
            question: v.question,
            status: v.status.try_into()?,
            priority: v.priority.try_into()?,
            source_kind: v.source_kind.try_into()?,
            source_detail: v.source_detail,
            url: v.url,
            due_at_unix_ms: v.due_at_unix_ms,
            snoozed_until_unix_ms: v.snoozed_until_unix_ms,
            created_by_session_id: v.created_by_session_id,
            created_at_unix_ms: v.created_at_unix_ms,
            updated_at_unix_ms: v.updated_at_unix_ms,
            done_at_unix_ms: v.done_at_unix_ms,
            blocked_by: v.blocked_by,
            session_ids: v.session_ids,
        })
    }
}

impl From<BucketBriefing> for wire::BucketBriefing {
    fn from(v: BucketBriefing) -> Self {
        wire::BucketBriefing {
            id: v.id,
            bucket_id: v.bucket_id,
            session_id: v.session_id,
            ts_unix_ms: v.ts_unix_ms,
            markdown: v.markdown,
        }
    }
}

impl From<wire::BucketBriefing> for BucketBriefing {
    fn from(v: wire::BucketBriefing) -> Self {
        BucketBriefing {
            id: v.id,
            bucket_id: v.bucket_id,
            session_id: v.session_id,
            ts_unix_ms: v.ts_unix_ms,
            markdown: v.markdown,
        }
    }
}

impl From<Session> for wire::Session {
    fn from(v: Session) -> Self {
        wire::Session {
            id: v.id,
            project_id: v.project_id,
            agent: wire::AgentKind::from(v.agent) as i32,
            agent_source: wire::AgentSelectionSource::from(v.agent_source) as i32,
            state: wire::SessionState::from(v.state) as i32,
            task_title: v.task_title,
            task_prompt: v.task_prompt,
            agent_session_id: v.agent_session_id.unwrap_or_default(),
            created_at_unix_ms: v.created_at_unix_ms,
            ended_at_unix_ms: v.ended_at_unix_ms,
            exit_code: v.exit_code,
            state_detail: v.state_detail,
            activity: v.activity,
            progress_percent: v.progress_percent,
            resumable: v.resumable,
            permission_mode: wire::PermissionMode::from(v.permission_mode) as i32,
            worker_id: v.worker_id,
            cwd: v.cwd,
            goal: v.goal,
            headline: v.headline,
            summary: v.summary,
            git: v.git.map(wire::SessionGit::from),
            last_activity_at_unix_ms: v.last_activity_at_unix_ms,
            items_api: v.items_api,
            supervisor_api: v.supervisor_api,
            spawned_by_session_id: v.spawned_by_session_id,
            last_agent_activity_at_unix_ms: v.last_agent_activity_at_unix_ms,
            last_user_interaction_at_unix_ms: v.last_user_interaction_at_unix_ms,
            role: wire::SessionRole::from(v.role) as i32,
            needs_input_unseen: v.needs_input_unseen,
            idle_unseen: v.idle_unseen,
            model_profile_id: v.model_profile_id,
            model_profile_source: wire::ModelProfileSource::from(v.model_profile_source) as i32,
            program_status: v
                .program_status
                .into_iter()
                .map(wire::ProgramStatusRecord::from)
                .collect(),
        }
    }
}

macro_rules! review_enum {
    ($domain:ty, $wire:ty, $($d:ident <=> $w:ident),+ $(,)?) => {
        impl From<$domain> for $wire {
            fn from(v: $domain) -> Self {
                match v { $(<$domain>::$d => <$wire>::$w),+ }
            }
        }
        impl $domain {
            fn from_wire(v: i32) -> Self {
                match <$wire>::try_from(v) {
                    $(Ok(<$wire>::$w) => <$domain>::$d,)+
                    _ => Default::default(),
                }
            }
        }
    };
}

review_enum!(ReviewMode, wire::ReviewMode, Range <=> Range, File <=> File);
review_enum!(ReviewState, wire::ReviewState, Open <=> Open, Finished <=> Finished);
review_enum!(
    ReviewThreadState,
    wire::ReviewThreadState,
    Draft <=> Draft,
    Sent <=> Sent,
    Answered <=> Answered,
    Resolved <=> Resolved,
);
review_enum!(ReviewSide, wire::ReviewSide, Right <=> Right, Left <=> Left);
review_enum!(
    ReviewAnchorStatus,
    wire::ReviewAnchorStatus,
    Same <=> Same,
    Moved <=> Moved,
    Changed <=> Changed,
    Unknown <=> Unknown,
);
review_enum!(ReviewAuthor, wire::ReviewAuthor, User <=> User, Session <=> Session);
review_enum!(ReviewChoiceSelect, wire::ReviewChoiceSelect, One <=> One, Many <=> Many);
review_enum!(
    PlanState,
    wire::PlanState,
    Active <=> Active,
    Accepted <=> Accepted,
    Archived <=> Archived,
);

impl From<Plan> for wire::Plan {
    fn from(v: Plan) -> Self {
        wire::Plan {
            id: v.id,
            project_id: v.project_id,
            bucket_id: v.bucket_id,
            owning_session_id: v.owning_session_id,
            creating_session_id: v.creating_session_id,
            name: v.name,
            summary: v.summary,
            state: wire::PlanState::from(v.state) as i32,
            markdown_path: v.markdown_path,
            revision: v.revision,
            active_decision_id: v.active_decision_id,
            linked_item_ids: v.linked_item_ids,
            created_at_unix_ms: v.created_at_unix_ms,
            updated_at_unix_ms: v.updated_at_unix_ms,
        }
    }
}

impl From<wire::Plan> for Plan {
    fn from(v: wire::Plan) -> Self {
        Plan {
            id: v.id,
            project_id: v.project_id,
            bucket_id: v.bucket_id,
            owning_session_id: v.owning_session_id,
            creating_session_id: v.creating_session_id,
            name: v.name,
            summary: v.summary,
            state: PlanState::from_wire(v.state),
            markdown_path: v.markdown_path,
            revision: v.revision,
            active_decision_id: v.active_decision_id,
            linked_item_ids: v.linked_item_ids,
            created_at_unix_ms: v.created_at_unix_ms,
            updated_at_unix_ms: v.updated_at_unix_ms,
        }
    }
}

impl From<ReviewChoiceAnswer> for wire::ReviewChoiceAnswer {
    fn from(v: ReviewChoiceAnswer) -> Self {
        wire::ReviewChoiceAnswer {
            choice_id: v.choice_id,
            select: wire::ReviewChoiceSelect::from(v.select) as i32,
            option_ids: v.option_ids,
            option_labels: v.option_labels,
            other_text: v.other_text,
            notes: v.notes,
        }
    }
}

impl From<wire::ReviewChoiceAnswer> for ReviewChoiceAnswer {
    fn from(v: wire::ReviewChoiceAnswer) -> Self {
        ReviewChoiceAnswer {
            choice_id: v.choice_id,
            select: ReviewChoiceSelect::from_wire(v.select),
            option_ids: v.option_ids,
            option_labels: v.option_labels,
            other_text: v.other_text,
            notes: v.notes,
        }
    }
}
review_enum!(
    ReviewSnapshotKind,
    wire::ReviewSnapshotKind,
    Sent <=> Sent,
    Received <=> Received,
);

impl From<Review> for wire::Review {
    fn from(v: Review) -> Self {
        wire::Review {
            id: v.id,
            session_id: v.session_id,
            project_id: v.project_id,
            worker_id: v.worker_id,
            worktree: v.worktree,
            mode: wire::ReviewMode::from(v.mode) as i32,
            base: v.base,
            head: v.head,
            pathspec: v.pathspec,
            source_file: v.source_file,
            label: v.label,
            range_key: v.range_key,
            state: wire::ReviewState::from(v.state) as i32,
            revision: v.revision,
            created_at_unix_ms: v.created_at_unix_ms,
            updated_at_unix_ms: v.updated_at_unix_ms,
            draft_count: v.draft_count,
            open_count: v.open_count,
            answered_count: v.answered_count,
            resolved_count: v.resolved_count,
            explicit_files: v.explicit_files,
            thread_latest_message: v.thread_latest_message.into_iter().collect(),
        }
    }
}

impl From<wire::Review> for Review {
    fn from(v: wire::Review) -> Self {
        Review {
            id: v.id,
            session_id: v.session_id,
            project_id: v.project_id,
            worker_id: v.worker_id,
            worktree: v.worktree,
            mode: ReviewMode::from_wire(v.mode),
            base: v.base,
            head: v.head,
            pathspec: v.pathspec,
            source_file: v.source_file,
            label: v.label,
            range_key: v.range_key,
            state: ReviewState::from_wire(v.state),
            revision: v.revision,
            created_at_unix_ms: v.created_at_unix_ms,
            updated_at_unix_ms: v.updated_at_unix_ms,
            draft_count: v.draft_count,
            open_count: v.open_count,
            answered_count: v.answered_count,
            resolved_count: v.resolved_count,
            explicit_files: v.explicit_files,
            thread_latest_message: v.thread_latest_message.into_iter().collect(),
        }
    }
}

impl From<ReviewMessage> for wire::ReviewMessage {
    fn from(v: ReviewMessage) -> Self {
        wire::ReviewMessage {
            id: v.id,
            thread_id: v.thread_id,
            author: wire::ReviewAuthor::from(v.author) as i32,
            session_id: v.session_id,
            body: v.body,
            addressed: v.addressed,
            revision: v.revision,
            created_at_unix_ms: v.created_at_unix_ms,
            changes_rev: v.changes_rev,
            changed_files: v.changed_files,
            choice: v.choice.map(Into::into),
        }
    }
}

impl From<wire::ReviewMessage> for ReviewMessage {
    fn from(v: wire::ReviewMessage) -> Self {
        ReviewMessage {
            id: v.id,
            thread_id: v.thread_id,
            author: ReviewAuthor::from_wire(v.author),
            session_id: v.session_id,
            body: v.body,
            addressed: v.addressed,
            revision: v.revision,
            created_at_unix_ms: v.created_at_unix_ms,
            changes_rev: v.changes_rev,
            changed_files: v.changed_files,
            choice: v.choice.map(Into::into),
        }
    }
}

impl From<ReviewThread> for wire::ReviewThread {
    fn from(v: ReviewThread) -> Self {
        wire::ReviewThread {
            id: v.id,
            review_id: v.review_id,
            path: v.path,
            line: v.line,
            side: wire::ReviewSide::from(v.side) as i32,
            excerpt: v.excerpt,
            anchor_snapshot_id: v.anchor_snapshot_id,
            current_line: v.current_line,
            anchor_status: wire::ReviewAnchorStatus::from(v.anchor_status) as i32,
            current_excerpt: v.current_excerpt,
            state: wire::ReviewThreadState::from(v.state) as i32,
            created_rev: v.created_rev,
            created_at_unix_ms: v.created_at_unix_ms,
            messages: v.messages.into_iter().map(Into::into).collect(),
            changed_ahead: v.changed_ahead,
        }
    }
}

impl From<wire::ReviewThread> for ReviewThread {
    fn from(v: wire::ReviewThread) -> Self {
        ReviewThread {
            id: v.id,
            review_id: v.review_id,
            path: v.path,
            line: v.line,
            side: ReviewSide::from_wire(v.side),
            excerpt: v.excerpt,
            anchor_snapshot_id: v.anchor_snapshot_id,
            current_line: v.current_line,
            anchor_status: ReviewAnchorStatus::from_wire(v.anchor_status),
            current_excerpt: v.current_excerpt,
            state: ReviewThreadState::from_wire(v.state),
            created_rev: v.created_rev,
            created_at_unix_ms: v.created_at_unix_ms,
            messages: v.messages.into_iter().map(Into::into).collect(),
            changed_ahead: v.changed_ahead,
        }
    }
}

impl From<ReviewRevision> for wire::ReviewRevision {
    fn from(v: ReviewRevision) -> Self {
        wire::ReviewRevision {
            id: v.id,
            review_id: v.review_id,
            rev: v.rev,
            kind: wire::ReviewSnapshotKind::from(v.kind) as i32,
            snapshot_id: v.snapshot_id,
            created_at_unix_ms: v.created_at_unix_ms,
            files: v.files,
        }
    }
}

impl From<wire::ReviewRevision> for ReviewRevision {
    fn from(v: wire::ReviewRevision) -> Self {
        ReviewRevision {
            id: v.id,
            review_id: v.review_id,
            rev: v.rev,
            kind: ReviewSnapshotKind::from_wire(v.kind),
            snapshot_id: v.snapshot_id,
            created_at_unix_ms: v.created_at_unix_ms,
            files: v.files,
        }
    }
}

impl From<ReviewViewerState> for wire::ReviewViewerState {
    fn from(v: ReviewViewerState) -> Self {
        wire::ReviewViewerState {
            review_id: v.review_id,
            user_id: v.user_id,
            pinned_rev: v.pinned_rev,
            view: v.view,
            layout: v.layout,
            context: v.context,
            viewed_files: v.viewed_files,
            preview_off_files: v.preview_off_files,
            last_thread_id: v.last_thread_id,
            scroll: v.scroll.into_iter().collect(),
            file_list_collapsed: v.file_list_collapsed,
            drafts: v.drafts.into_iter().collect(),
            seen: v.seen.into_iter().collect(),
        }
    }
}

impl From<wire::ReviewViewerState> for ReviewViewerState {
    fn from(v: wire::ReviewViewerState) -> Self {
        ReviewViewerState {
            review_id: v.review_id,
            user_id: v.user_id,
            pinned_rev: v.pinned_rev,
            view: v.view,
            layout: v.layout,
            context: v.context,
            viewed_files: v.viewed_files,
            preview_off_files: v.preview_off_files,
            last_thread_id: v.last_thread_id,
            scroll: v.scroll.into_iter().collect(),
            file_list_collapsed: v.file_list_collapsed,
            drafts: v.drafts.into_iter().collect(),
            seen: v.seen.into_iter().collect(),
        }
    }
}

impl From<SessionGit> for wire::SessionGit {
    fn from(v: SessionGit) -> Self {
        wire::SessionGit {
            branch: v.branch,
            worktree: v.worktree,
            repo_root: v.repo_root,
            commit: v.commit,
            upstream: v.upstream,
            dirty: v.dirty,
        }
    }
}

impl From<wire::SessionGit> for SessionGit {
    fn from(v: wire::SessionGit) -> Self {
        SessionGit {
            branch: v.branch,
            worktree: v.worktree,
            repo_root: v.repo_root,
            commit: v.commit,
            upstream: v.upstream,
            dirty: v.dirty,
        }
    }
}

impl TryFrom<wire::Session> for Session {
    type Error = DecodeError;
    fn try_from(v: wire::Session) -> Result<Self, DecodeError> {
        Ok(Session {
            id: v.id,
            project_id: v.project_id,
            agent: v.agent.try_into()?,
            agent_source: agent_selection_source(v.agent_source)?,
            state: v.state.try_into()?,
            task_title: v.task_title,
            task_prompt: v.task_prompt,
            agent_session_id: (!v.agent_session_id.is_empty()).then_some(v.agent_session_id),
            created_at_unix_ms: v.created_at_unix_ms,
            ended_at_unix_ms: v.ended_at_unix_ms,
            exit_code: v.exit_code,
            state_detail: v.state_detail,
            activity: v.activity,
            progress_percent: v.progress_percent,
            resumable: v.resumable,
            permission_mode: v.permission_mode.try_into()?,
            worker_id: v.worker_id,
            cwd: v.cwd,
            goal: v.goal,
            headline: v.headline,
            summary: v.summary,
            git: v.git.map(SessionGit::from),
            items_api: v.items_api,
            supervisor_api: v.supervisor_api,
            spawned_by_session_id: v.spawned_by_session_id,
            last_activity_at_unix_ms: v.last_activity_at_unix_ms,
            last_agent_activity_at_unix_ms: v.last_agent_activity_at_unix_ms,
            last_user_interaction_at_unix_ms: v.last_user_interaction_at_unix_ms,
            role: v.role.try_into()?,
            needs_input_unseen: v.needs_input_unseen,
            idle_unseen: v.idle_unseen,
            model_profile_id: v.model_profile_id,
            model_profile_source: model_profile_source(v.model_profile_source)?,
            program_status: program_status_records(v.program_status),
        })
    }
}

impl From<PermissionMode> for wire::PermissionMode {
    fn from(v: PermissionMode) -> Self {
        match v {
            PermissionMode::Inherit => wire::PermissionMode::Unspecified,
            PermissionMode::Default => wire::PermissionMode::Default,
            PermissionMode::Auto => wire::PermissionMode::Auto,
            PermissionMode::Bypass => wire::PermissionMode::Bypass,
        }
    }
}

impl TryFrom<i32> for PermissionMode {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::PermissionMode::try_from(v) {
            Ok(wire::PermissionMode::Unspecified) => Ok(PermissionMode::Inherit),
            Ok(wire::PermissionMode::Default) => Ok(PermissionMode::Default),
            Ok(wire::PermissionMode::Auto) => Ok(PermissionMode::Auto),
            Ok(wire::PermissionMode::Bypass) => Ok(PermissionMode::Bypass),
            _ => Err(DecodeError::UnknownEnum("PermissionMode", v)),
        }
    }
}

impl From<PathCheck> for wire::PathCheck {
    fn from(v: PathCheck) -> Self {
        match v {
            PathCheck::Ok => wire::PathCheck::Ok,
            PathCheck::Missing => wire::PathCheck::Missing,
            PathCheck::NotADirectory => wire::PathCheck::NotADirectory,
            PathCheck::Unreadable => wire::PathCheck::Unreadable,
        }
    }
}

impl TryFrom<i32> for PathCheck {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::PathCheck::try_from(v) {
            Ok(wire::PathCheck::Ok) => Ok(PathCheck::Ok),
            Ok(wire::PathCheck::Missing) => Ok(PathCheck::Missing),
            Ok(wire::PathCheck::NotADirectory) => Ok(PathCheck::NotADirectory),
            Ok(wire::PathCheck::Unreadable) => Ok(PathCheck::Unreadable),
            _ => Err(DecodeError::UnknownEnum("PathCheck", v)),
        }
    }
}

impl From<AgentInboxMode> for wire::AgentInboxMode {
    fn from(v: AgentInboxMode) -> Self {
        match v {
            AgentInboxMode::Queue => wire::AgentInboxMode::Queue,
            AgentInboxMode::Steer => wire::AgentInboxMode::Steer,
        }
    }
}

impl TryFrom<i32> for AgentInboxMode {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::AgentInboxMode::try_from(v) {
            // A worker that sends no mode means the default one, which
            // is what every channel without steering honours anyway.
            Ok(wire::AgentInboxMode::Unspecified) | Ok(wire::AgentInboxMode::Queue) => {
                Ok(AgentInboxMode::Queue)
            }
            Ok(wire::AgentInboxMode::Steer) => Ok(AgentInboxMode::Steer),
            _ => Err(DecodeError::UnknownEnum("AgentInboxMode", v)),
        }
    }
}

impl From<AgentInboxOutcome> for wire::AgentInboxOutcome {
    fn from(v: AgentInboxOutcome) -> Self {
        match v {
            AgentInboxOutcome::Delivered => wire::AgentInboxOutcome::Delivered,
            AgentInboxOutcome::NoChannel => wire::AgentInboxOutcome::NoChannel,
            AgentInboxOutcome::Failed => wire::AgentInboxOutcome::Failed,
        }
    }
}

impl TryFrom<i32> for AgentInboxOutcome {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::AgentInboxOutcome::try_from(v) {
            Ok(wire::AgentInboxOutcome::Delivered) => Ok(AgentInboxOutcome::Delivered),
            Ok(wire::AgentInboxOutcome::NoChannel) => Ok(AgentInboxOutcome::NoChannel),
            Ok(wire::AgentInboxOutcome::Failed) => Ok(AgentInboxOutcome::Failed),
            _ => Err(DecodeError::UnknownEnum("AgentInboxOutcome", v)),
        }
    }
}

impl From<HookKind> for wire::HookKind {
    fn from(v: HookKind) -> Self {
        match v {
            HookKind::NeedsInput => wire::HookKind::NeedsInput,
            HookKind::TurnEnded => wire::HookKind::TurnEnded,
            HookKind::TurnFailed => wire::HookKind::TurnFailed,
            HookKind::PromptSubmitted => wire::HookKind::PromptSubmitted,
            HookKind::Started => wire::HookKind::Started,
        }
    }
}

impl TryFrom<i32> for HookKind {
    type Error = DecodeError;
    fn try_from(v: i32) -> Result<Self, DecodeError> {
        match wire::HookKind::try_from(v) {
            Ok(wire::HookKind::NeedsInput) => Ok(HookKind::NeedsInput),
            Ok(wire::HookKind::TurnEnded) => Ok(HookKind::TurnEnded),
            Ok(wire::HookKind::TurnFailed) => Ok(HookKind::TurnFailed),
            Ok(wire::HookKind::PromptSubmitted) => Ok(HookKind::PromptSubmitted),
            Ok(wire::HookKind::Started) => Ok(HookKind::Started),
            _ => Err(DecodeError::UnknownEnum("HookKind", v)),
        }
    }
}

impl From<Scope> for wire::Scope {
    fn from(v: Scope) -> Self {
        let scope = match v {
            Scope::All => wire::scope::Scope::All(true),
            Scope::Bucket(id) => wire::scope::Scope::BucketId(id),
            Scope::Project(id) => wire::scope::Scope::ProjectId(id),
            Scope::Session(id) => wire::scope::Scope::SessionId(id),
        };
        wire::Scope { scope: Some(scope) }
    }
}

impl TryFrom<wire::Scope> for Scope {
    type Error = DecodeError;
    fn try_from(v: wire::Scope) -> Result<Self, DecodeError> {
        match v.scope.ok_or(DecodeError::Missing("Scope.scope"))? {
            wire::scope::Scope::All(_) => Ok(Scope::All),
            wire::scope::Scope::BucketId(id) => Ok(Scope::Bucket(id)),
            wire::scope::Scope::ProjectId(id) => Ok(Scope::Project(id)),
            wire::scope::Scope::SessionId(id) => Ok(Scope::Session(id)),
        }
    }
}

impl From<ClientEnvelope> for wire::ClientMessage {
    fn from(v: ClientEnvelope) -> Self {
        use wire::client_message::Msg;
        let msg = match v.msg {
            ClientMsg::Subscribe { scope } => Msg::Subscribe(wire::Subscribe {
                scope: Some(scope.into()),
            }),
            ClientMsg::SpawnSession {
                project_id,
                agent,
                task_title,
                task_prompt,
                cwd,
                permission_mode,
                worker_id,
                items_api,
                supervisor_api,
                model_profile_id,
                host,
                initial_cols,
                initial_rows,
            } => Msg::SpawnSession(wire::SpawnSession {
                project_id,
                agent: agent
                    .map(|agent| wire::AgentKind::from(agent) as i32)
                    .unwrap_or_default(),
                task_title,
                task_prompt,
                cwd,
                permission_mode: wire::PermissionMode::from(permission_mode) as i32,
                worker_id,
                items_api: Some(items_api),
                supervisor_api: Some(supervisor_api),
                role: Some(wire::SessionRole::from(if supervisor_api {
                    SessionRole::Supervisor
                } else {
                    SessionRole::Worker
                }) as i32),
                model_profile_id,
                host,
                initial_cols: initial_cols.map(|c| c as u32),
                initial_rows: initial_rows.map(|r| r as u32),
            }),
            ClientMsg::AttachPty { session_id } => Msg::AttachPty(wire::AttachPty { session_id }),
            ClientMsg::DetachPty { session_id } => Msg::DetachPty(wire::DetachPty { session_id }),
            ClientMsg::PtyInput { session_id, data } => Msg::PtyInput(wire::PtyInput {
                session_id,
                data: data.to_vec(),
            }),
            ClientMsg::PtyResize {
                session_id,
                cols,
                rows,
            } => Msg::PtyResize(wire::PtyResize {
                session_id,
                cols: cols as u32,
                rows: rows as u32,
            }),
            ClientMsg::InterruptSession { session_id } => {
                Msg::InterruptSession(wire::InterruptSession { session_id })
            }
            ClientMsg::KillSession { session_id } => {
                Msg::KillSession(wire::KillSession { session_id })
            }
            ClientMsg::CreateBucket {
                name,
                allowed_worker_ids,
                default_worker_id,
                is_default,
            } => Msg::CreateBucket(wire::CreateBucket {
                name,
                allowed_worker_ids,
                default_worker_id,
                is_default,
            }),
            ClientMsg::DeleteBucket { id } => Msg::DeleteBucket(wire::DeleteBucket { id }),
            ClientMsg::CreateProject {
                bucket_id,
                name,
                path,
                worker_id,
                allowed_worker_ids,
            } => Msg::CreateProject(wire::CreateProject {
                bucket_id,
                name,
                path,
                worker_id,
                allowed_worker_ids,
            }),
            ClientMsg::UpdateProject {
                project_id,
                path,
                permission_mode,
                worker_id,
            } => Msg::UpdateProject(wire::UpdateProject {
                project_id,
                path,
                permission_mode: permission_mode
                    .map(|mode| wire::PermissionMode::from(mode) as i32),
                worker_update: worker_id.map(|worker_id| match worker_id {
                    Some(worker_id) => wire::update_project::WorkerUpdate::WorkerId(worker_id),
                    None => wire::update_project::WorkerUpdate::ClearWorker(true),
                }),
            }),
            ClientMsg::DeleteProject { id } => Msg::DeleteProject(wire::DeleteProject { id }),
            ClientMsg::HookEvent {
                session_token,
                kind,
                detail,
                agent_session_id,
                transcript_path,
                background_work,
            } => Msg::HookEvent(wire::HookEvent {
                session_token,
                kind: wire::HookKind::from(kind) as i32,
                detail,
                agent_session_id,
                transcript_path,
                background_work,
            }),
            ClientMsg::ResumeSession { session_id } => {
                Msg::ResumeSession(wire::ResumeSession { session_id })
            }
            ClientMsg::SetBucketPermissionMode { bucket_id, mode } => {
                Msg::SetBucketPermissionMode(wire::SetBucketPermissionMode {
                    bucket_id,
                    mode: wire::PermissionMode::from(mode) as i32,
                })
            }
            ClientMsg::SetProjectPermissionMode { project_id, mode } => {
                Msg::SetProjectPermissionMode(wire::SetProjectPermissionMode {
                    project_id,
                    mode: wire::PermissionMode::from(mode) as i32,
                })
            }
            ClientMsg::SetBucketDefaultAgent { bucket_id, agent } => {
                Msg::SetBucketDefaultAgent(wire::SetBucketDefaultAgent {
                    bucket_id,
                    agent: agent
                        .map(|agent| wire::AgentKind::from(agent) as i32)
                        .unwrap_or_default(),
                })
            }
            ClientMsg::SetProjectDefaultAgent { project_id, agent } => {
                Msg::SetProjectDefaultAgent(wire::SetProjectDefaultAgent {
                    project_id,
                    agent: agent
                        .map(|agent| wire::AgentKind::from(agent) as i32)
                        .unwrap_or_default(),
                })
            }
            ClientMsg::CreateModelProfile { name, api_key } => {
                Msg::CreateModelProfile(wire::CreateModelProfile { name, api_key })
            }
            ClientMsg::UpdateModelProfile {
                id,
                name,
                api_key,
                clear_api_key,
            } => Msg::UpdateModelProfile(wire::UpdateModelProfile {
                id,
                name,
                api_key,
                clear_api_key,
            }),
            ClientMsg::DeleteModelProfile { id } => {
                Msg::DeleteModelProfile(wire::DeleteModelProfile { id })
            }
            ClientMsg::SetModelProfileEndpoint {
                profile_id,
                dialect,
                model,
                base_url,
                background_model,
            } => Msg::SetModelProfileEndpoint(wire::SetModelProfileEndpoint {
                profile_id,
                dialect: wire::ModelDialect::from(dialect) as i32,
                model,
                base_url,
                background_model,
            }),
            ClientMsg::DeleteModelProfileEndpoint {
                profile_id,
                dialect,
            } => Msg::DeleteModelProfileEndpoint(wire::DeleteModelProfileEndpoint {
                profile_id,
                dialect: wire::ModelDialect::from(dialect) as i32,
            }),
            ClientMsg::SetBucketModelProfile {
                bucket_id,
                model_profile_id,
            } => Msg::SetBucketModelProfile(wire::SetBucketModelProfile {
                bucket_id,
                model_profile_id,
            }),
            ClientMsg::SetProjectModelProfile {
                project_id,
                model_profile_id,
            } => Msg::SetProjectModelProfile(wire::SetProjectModelProfile {
                project_id,
                model_profile_id,
            }),
            ClientMsg::SetProjectWorkerPath {
                project_id,
                worker,
                path,
            } => Msg::SetProjectWorkerPath(wire::SetProjectWorkerPath {
                project_id,
                worker,
                path,
            }),
            ClientMsg::OpenReview {
                session_id,
                worktree,
                base,
                head,
                pathspec,
                files,
                source_file,
                label,
                reset,
            } => Msg::OpenReview(wire::OpenReview {
                session_id,
                worktree,
                base,
                head,
                pathspec,
                files,
                source_file,
                label,
                reset,
            }),
            ClientMsg::AddReviewComment {
                review_id,
                path,
                line,
                side,
                excerpt,
                body,
                send,
                anchor_snapshot_id,
                choice,
            } => Msg::AddReviewComment(wire::AddReviewComment {
                review_id,
                path,
                line,
                side: wire::ReviewSide::from(side) as i32,
                excerpt,
                body,
                send,
                anchor_snapshot_id,
                choice: choice.map(|c| (*c).into()),
            }),
            ClientMsg::EditReviewComment {
                message_id,
                body,
                choice,
            } => Msg::EditReviewComment(wire::EditReviewComment {
                message_id,
                body,
                choice: choice.map(|c| (*c).into()),
            }),
            ClientMsg::DeleteReviewThread { thread_id } => {
                Msg::DeleteReviewThread(wire::DeleteReviewThread { thread_id })
            }
            ClientMsg::SendReviewThreads {
                review_id,
                thread_ids,
            } => Msg::SendReviewThreads(wire::SendReviewThreads {
                review_id,
                thread_ids,
            }),
            ClientMsg::ResolveReviewThread {
                thread_id,
                resolved,
            } => Msg::ResolveReviewThread(wire::ResolveReviewThread {
                thread_id,
                resolved,
            }),
            ClientMsg::ReplyReviewThread { thread_id, body } => {
                Msg::ReplyReviewThread(wire::ReplyReviewThread { thread_id, body })
            }
            ClientMsg::PostReviewReply {
                thread_id,
                body,
                addressed,
            } => Msg::PostReviewReply(wire::PostReviewReply {
                thread_id,
                body,
                addressed,
            }),
            ClientMsg::AdvanceReview { review_id, rev } => {
                Msg::AdvanceReview(wire::AdvanceReview { review_id, rev })
            }
            ClientMsg::FinishReview { review_id } => {
                Msg::FinishReview(wire::FinishReview { review_id })
            }
            ClientMsg::SetReviewViewerState(u) => {
                let u = *u;
                Msg::SetReviewViewerState(wire::SetReviewViewerState {
                    review_id: u.review_id,
                    pinned_rev: u.pinned_rev,
                    view: u.view,
                    layout: u.layout,
                    context: u.context,
                    set_viewed_files: u.viewed_files.is_some(),
                    viewed_files: u.viewed_files.unwrap_or_default(),
                    set_preview_off_files: u.preview_off_files.is_some(),
                    preview_off_files: u.preview_off_files.unwrap_or_default(),
                    last_thread_id: u.last_thread_id,
                    scroll_key: u.scroll_key,
                    scroll_top: u.scroll_top,
                    file_list_collapsed: u.file_list_collapsed,
                    draft_key: u.draft_key,
                    draft_body: u.draft_body,
                    seen_thread: u.seen_thread,
                    seen_message: u.seen_message,
                })
            }
            ClientMsg::ListReviews {
                session_id,
                include_finished,
            } => Msg::ListReviews(wire::ListReviews {
                session_id,
                include_finished,
            }),
            ClientMsg::ResolveRev {
                session_id,
                worktree,
                rev,
            } => Msg::ResolveRev(wire::ResolveRev {
                session_id,
                worktree,
                rev,
            }),
            ClientMsg::CreateShell { session_id, title } => {
                Msg::CreateShell(wire::CreateShell { session_id, title })
            }
            ClientMsg::RestartTerminal { terminal_id } => {
                Msg::RestartTerminal(wire::RestartTerminal { terminal_id })
            }
            ClientMsg::CloseTerminal { terminal_id } => {
                Msg::CloseTerminal(wire::CloseTerminal { terminal_id })
            }
            ClientMsg::AttachTerminal { terminal_id } => {
                Msg::AttachTerminal(wire::AttachTerminal { terminal_id })
            }
            ClientMsg::DetachTerminal { terminal_id } => {
                Msg::DetachTerminal(wire::DetachTerminal { terminal_id })
            }
            ClientMsg::TerminalInput { terminal_id, data } => {
                Msg::TerminalInput(wire::TerminalInput {
                    terminal_id,
                    data: data.to_vec(),
                })
            }
            ClientMsg::TerminalResize {
                terminal_id,
                cols,
                rows,
            } => Msg::TerminalResize(wire::TerminalResize {
                terminal_id,
                cols: cols as u32,
                rows: rows as u32,
            }),
            ClientMsg::TerminalTranscript {
                terminal_id,
                generation,
            } => Msg::TerminalTranscript(wire::TerminalTranscript {
                terminal_id,
                generation,
            }),
            ClientMsg::ListWorkspaces => Msg::ListWorkspaces(wire::ListWorkspaces {}),
            ClientMsg::SessionForwardInventory { session_token } => {
                Msg::SessionForwardInventory(wire::SessionForwardInventory { session_token })
            }
            ClientMsg::CloseForward { forward_id } => {
                Msg::CloseForward(wire::CloseForward { forward_id })
            }
            ClientMsg::UpsertItem(w) => Msg::UpsertItem(wire::UpsertItem {
                bucket_id: w.bucket_id,
                id: w.id,
                external_key: w.external_key,
                title: w.title,
                body: w.body,
                status: w
                    .status
                    .map(|s| wire::ItemStatus::from(s) as i32)
                    .unwrap_or(0),
                priority: w
                    .priority
                    .map(|p| wire::ItemPriority::from(p) as i32)
                    .unwrap_or(0),
                source_kind: w
                    .source_kind
                    .map(|k| wire::ItemSourceKind::from(k) as i32)
                    .unwrap_or(0),
                source_detail: w.source_detail,
                url: w.url,
                project_id: w.project_id,
                due_at_unix_ms: w.due_at_unix_ms,
                set_blocked_by: w.blocked_by.is_some(),
                blocked_by: w.blocked_by.unwrap_or_default(),
                note: w.note,
                link_session_id: w.link_session_id,
                question: w.question,
                clear_project: w.clear_project,
                clear_due: w.clear_due,
            }),
            ClientMsg::DeleteItem { bucket_id, id } => {
                Msg::DeleteItem(wire::DeleteItem { id, bucket_id })
            }
            ClientMsg::SnoozeItem {
                bucket_id,
                id,
                until_unix_ms,
            } => Msg::SnoozeItem(wire::SnoozeItem {
                id,
                until_unix_ms,
                bucket_id,
            }),
            ClientMsg::ListItems(q) => Msg::ListItems(wire::ListItems {
                bucket_id: q.bucket_id,
                statuses: q
                    .statuses
                    .into_iter()
                    .map(|s| wire::ItemStatus::from(s) as i32)
                    .collect(),
                project_id: q.project_id,
                updated_since_unix_ms: q.updated_since_unix_ms,
                include_closed: q.include_closed,
                include_snoozed: q.include_snoozed,
                search: q.search,
                priorities: q
                    .priorities
                    .into_iter()
                    .map(|p| wire::ItemPriority::from(p) as i32)
                    .collect(),
                source_kinds: q
                    .source_kinds
                    .into_iter()
                    .map(|s| wire::ItemSourceKind::from(s) as i32)
                    .collect(),
                limit: q.limit,
                offset: q.offset,
                summary_filter: q
                    .summary_filter
                    .map(|value| wire::ItemSummaryFilter::from(value) as i32)
                    .unwrap_or_default(),
            }),
            ClientMsg::ItemNotes { bucket_id, item_id } => {
                Msg::ItemNotes(wire::ItemNotes { item_id, bucket_id })
            }
            ClientMsg::SetSetting { key, value } => {
                Msg::SetSetting(wire::SetSetting { key, value })
            }
            ClientMsg::ListSettings => Msg::ListSettings(wire::ListSettings {}),
            ClientMsg::GetSession { session_id } => {
                Msg::GetSession(wire::GetSession { session_id })
            }
            ClientMsg::ListEndedSessions { cursor, limit } => {
                Msg::ListEndedSessions(wire::ListEndedSessions { cursor, limit })
            }
            ClientMsg::SearchSessions {
                query,
                cursor,
                limit,
            } => Msg::SearchSessions(wire::SearchSessions {
                query,
                cursor,
                limit,
            }),
            ClientMsg::MarkSessionSeen { session_id } => {
                Msg::MarkSessionSeen(wire::MarkSessionSeen { session_id })
            }
            ClientMsg::UpdateSessionApis {
                session_id,
                items_api,
                supervisor_api,
                role,
            } => Msg::UpdateSessionApis(wire::UpdateSessionApis {
                session_id,
                items_api,
                supervisor_api,
                role: role.map(|v| wire::SessionRole::from(v) as i32),
            }),
            ClientMsg::ListInstructions {
                bucket_id,
                project_id,
            } => Msg::ListInstructions(wire::ListInstructions {
                bucket_id,
                project_id,
            }),
            ClientMsg::GetEffectiveInstructions {
                bucket_id,
                project_id,
                role,
            } => Msg::GetEffectiveInstructions(wire::GetEffectiveInstructions {
                bucket_id,
                project_id,
                role: wire::SessionRole::from(role) as i32,
            }),
            ClientMsg::SetInstructions {
                bucket_id,
                project_id,
                target,
                markdown,
                expected_revision,
                note,
            } => Msg::SetInstructions(wire::SetInstructions {
                bucket_id,
                project_id,
                target: wire::InstructionTarget::from(target) as i32,
                markdown,
                expected_revision,
                note,
            }),
            ClientMsg::RevertInstructions {
                layer_id,
                revision,
                expected_revision,
                note,
            } => Msg::RevertInstructions(wire::RevertInstructions {
                layer_id,
                revision,
                expected_revision,
                note,
            }),
            ClientMsg::RespondToItem {
                bucket_id,
                item_id,
                text,
                target,
            } => {
                use wire::respond_to_item::Target;
                let target = match target {
                    RespondTarget::Session(id) => Target::SessionId(id),
                    RespondTarget::NewSupervisor { project_id } => {
                        Target::NewSupervisor(wire::NewSupervisorTarget { project_id })
                    }
                    RespondTarget::ReplyOnly => Target::ReplyOnly(true),
                };
                Msg::RespondToItem(wire::RespondToItem {
                    bucket_id,
                    item_id,
                    text,
                    target: Some(target),
                })
            }
        };
        wire::ClientMessage {
            seq: v.seq,
            msg: Some(msg),
        }
    }
}

impl TryFrom<wire::ClientMessage> for ClientEnvelope {
    type Error = DecodeError;
    fn try_from(v: wire::ClientMessage) -> Result<Self, DecodeError> {
        use wire::client_message::Msg;
        let msg = match v.msg.ok_or(DecodeError::Missing("ClientMessage.msg"))? {
            Msg::Subscribe(m) => ClientMsg::Subscribe {
                scope: m
                    .scope
                    .ok_or(DecodeError::Missing("Subscribe.scope"))?
                    .try_into()?,
            },
            Msg::SpawnSession(m) => ClientMsg::SpawnSession {
                project_id: m.project_id,
                agent: optional_agent(m.agent)?,
                task_title: m.task_title,
                task_prompt: m.task_prompt,
                cwd: m.cwd,
                permission_mode: m.permission_mode.try_into()?,
                worker_id: m.worker_id,
                items_api: m.items_api.unwrap_or(true),
                supervisor_api: m.supervisor_api.unwrap_or(false)
                    || matches!(
                        m.role.map(SessionRole::try_from).transpose()?,
                        Some(SessionRole::Supervisor)
                    ),
                model_profile_id: m.model_profile_id,
                host: m.host,
                initial_cols: m.initial_cols.map(|c| c as u16),
                initial_rows: m.initial_rows.map(|r| r as u16),
            },
            Msg::AttachPty(m) => ClientMsg::AttachPty {
                session_id: m.session_id,
            },
            Msg::DetachPty(m) => ClientMsg::DetachPty {
                session_id: m.session_id,
            },
            Msg::PtyInput(m) => ClientMsg::PtyInput {
                session_id: m.session_id,
                data: m.data.into(),
            },
            Msg::PtyResize(m) => ClientMsg::PtyResize {
                session_id: m.session_id,
                cols: u16::try_from(m.cols).map_err(|_| DecodeError::OutOfRange("cols"))?,
                rows: u16::try_from(m.rows).map_err(|_| DecodeError::OutOfRange("rows"))?,
            },
            Msg::InterruptSession(m) => ClientMsg::InterruptSession {
                session_id: m.session_id,
            },
            Msg::KillSession(m) => ClientMsg::KillSession {
                session_id: m.session_id,
            },
            Msg::CreateBucket(m) => ClientMsg::CreateBucket {
                name: m.name,
                allowed_worker_ids: m.allowed_worker_ids,
                default_worker_id: m.default_worker_id,
                is_default: m.is_default,
            },
            Msg::DeleteBucket(m) => ClientMsg::DeleteBucket { id: m.id },
            Msg::CreateProject(m) => ClientMsg::CreateProject {
                bucket_id: m.bucket_id,
                name: m.name,
                path: m.path,
                worker_id: m.worker_id,
                allowed_worker_ids: m.allowed_worker_ids,
            },
            Msg::UpdateProject(m) => ClientMsg::UpdateProject {
                project_id: m.project_id,
                path: m.path,
                permission_mode: m.permission_mode.map(TryInto::try_into).transpose()?,
                worker_id: m.worker_update.map(|update| match update {
                    wire::update_project::WorkerUpdate::WorkerId(worker_id) => Some(worker_id),
                    wire::update_project::WorkerUpdate::ClearWorker(_) => None,
                }),
            },
            Msg::DeleteProject(m) => ClientMsg::DeleteProject { id: m.id },
            Msg::HookEvent(m) => ClientMsg::HookEvent {
                session_token: m.session_token,
                kind: m.kind.try_into()?,
                detail: m.detail,
                agent_session_id: m.agent_session_id,
                transcript_path: m.transcript_path,
                // An older pm sends no such field, which decodes to
                // false: the same answer it would have given.
                background_work: m.background_work,
            },
            Msg::ResumeSession(m) => ClientMsg::ResumeSession {
                session_id: m.session_id,
            },
            Msg::SetBucketPermissionMode(m) => ClientMsg::SetBucketPermissionMode {
                bucket_id: m.bucket_id,
                mode: m.mode.try_into()?,
            },
            Msg::SetProjectPermissionMode(m) => ClientMsg::SetProjectPermissionMode {
                project_id: m.project_id,
                mode: m.mode.try_into()?,
            },
            Msg::SetBucketDefaultAgent(m) => ClientMsg::SetBucketDefaultAgent {
                bucket_id: m.bucket_id,
                agent: optional_agent(m.agent)?,
            },
            Msg::SetProjectDefaultAgent(m) => ClientMsg::SetProjectDefaultAgent {
                project_id: m.project_id,
                agent: optional_agent(m.agent)?,
            },
            Msg::CreateModelProfile(m) => ClientMsg::CreateModelProfile {
                name: m.name,
                api_key: m.api_key,
            },
            Msg::UpdateModelProfile(m) => ClientMsg::UpdateModelProfile {
                id: m.id,
                name: m.name,
                api_key: m.api_key,
                clear_api_key: m.clear_api_key,
            },
            Msg::DeleteModelProfile(m) => ClientMsg::DeleteModelProfile { id: m.id },
            Msg::SetModelProfileEndpoint(m) => ClientMsg::SetModelProfileEndpoint {
                profile_id: m.profile_id,
                dialect: m.dialect.try_into()?,
                model: m.model,
                base_url: m.base_url,
                background_model: m.background_model,
            },
            Msg::DeleteModelProfileEndpoint(m) => ClientMsg::DeleteModelProfileEndpoint {
                profile_id: m.profile_id,
                dialect: m.dialect.try_into()?,
            },
            Msg::SetBucketModelProfile(m) => ClientMsg::SetBucketModelProfile {
                bucket_id: m.bucket_id,
                model_profile_id: m.model_profile_id,
            },
            Msg::SetProjectModelProfile(m) => ClientMsg::SetProjectModelProfile {
                project_id: m.project_id,
                model_profile_id: m.model_profile_id,
            },
            Msg::SetProjectWorkerPath(m) => ClientMsg::SetProjectWorkerPath {
                project_id: m.project_id,
                worker: m.worker,
                path: m.path,
            },
            Msg::OpenReview(m) => ClientMsg::OpenReview {
                session_id: m.session_id,
                worktree: m.worktree,
                base: m.base,
                head: m.head,
                pathspec: m.pathspec,
                files: m.files,
                source_file: m.source_file,
                label: m.label,
                reset: m.reset,
            },
            Msg::AddReviewComment(m) => ClientMsg::AddReviewComment {
                review_id: m.review_id,
                path: m.path,
                line: m.line,
                side: ReviewSide::from_wire(m.side),
                excerpt: m.excerpt,
                body: m.body,
                send: m.send,
                anchor_snapshot_id: m.anchor_snapshot_id,
                choice: m.choice.map(|c| Box::new(c.into())),
            },
            Msg::EditReviewComment(m) => ClientMsg::EditReviewComment {
                message_id: m.message_id,
                body: m.body,
                choice: m.choice.map(|c| Box::new(c.into())),
            },
            Msg::DeleteReviewThread(m) => ClientMsg::DeleteReviewThread {
                thread_id: m.thread_id,
            },
            Msg::SendReviewThreads(m) => ClientMsg::SendReviewThreads {
                review_id: m.review_id,
                thread_ids: m.thread_ids,
            },
            Msg::ResolveReviewThread(m) => ClientMsg::ResolveReviewThread {
                thread_id: m.thread_id,
                resolved: m.resolved,
            },
            Msg::ReplyReviewThread(m) => ClientMsg::ReplyReviewThread {
                thread_id: m.thread_id,
                body: m.body,
            },
            Msg::PostReviewReply(m) => ClientMsg::PostReviewReply {
                thread_id: m.thread_id,
                body: m.body,
                addressed: m.addressed,
            },
            Msg::AdvanceReview(m) => ClientMsg::AdvanceReview {
                review_id: m.review_id,
                rev: m.rev,
            },
            Msg::FinishReview(m) => ClientMsg::FinishReview {
                review_id: m.review_id,
            },
            Msg::SetReviewViewerState(m) => {
                ClientMsg::SetReviewViewerState(Box::new(ReviewViewerStateUpdate {
                    review_id: m.review_id,
                    pinned_rev: m.pinned_rev,
                    view: m.view,
                    layout: m.layout,
                    context: m.context,
                    viewed_files: m.set_viewed_files.then_some(m.viewed_files),
                    preview_off_files: m.set_preview_off_files.then_some(m.preview_off_files),
                    last_thread_id: m.last_thread_id,
                    scroll_key: m.scroll_key,
                    scroll_top: m.scroll_top,
                    file_list_collapsed: m.file_list_collapsed,
                    draft_key: m.draft_key,
                    draft_body: m.draft_body,
                    seen_thread: m.seen_thread,
                    seen_message: m.seen_message,
                }))
            }
            Msg::ListReviews(m) => ClientMsg::ListReviews {
                session_id: m.session_id,
                include_finished: m.include_finished,
            },
            Msg::ResolveRev(m) => ClientMsg::ResolveRev {
                session_id: m.session_id,
                worktree: m.worktree,
                rev: m.rev,
            },
            Msg::CreateShell(m) => ClientMsg::CreateShell {
                session_id: m.session_id,
                title: m.title,
            },
            Msg::RestartTerminal(m) => ClientMsg::RestartTerminal {
                terminal_id: m.terminal_id,
            },
            Msg::CloseTerminal(m) => ClientMsg::CloseTerminal {
                terminal_id: m.terminal_id,
            },
            Msg::AttachTerminal(m) => ClientMsg::AttachTerminal {
                terminal_id: m.terminal_id,
            },
            Msg::DetachTerminal(m) => ClientMsg::DetachTerminal {
                terminal_id: m.terminal_id,
            },
            Msg::TerminalInput(m) => ClientMsg::TerminalInput {
                terminal_id: m.terminal_id,
                data: m.data.into(),
            },
            Msg::TerminalResize(m) => ClientMsg::TerminalResize {
                terminal_id: m.terminal_id,
                cols: u16::try_from(m.cols).map_err(|_| DecodeError::OutOfRange("cols"))?,
                rows: u16::try_from(m.rows).map_err(|_| DecodeError::OutOfRange("rows"))?,
            },
            Msg::TerminalTranscript(m) => ClientMsg::TerminalTranscript {
                terminal_id: m.terminal_id,
                generation: m.generation,
            },
            Msg::ListWorkspaces(_) => ClientMsg::ListWorkspaces,
            Msg::SessionForwardInventory(m) => ClientMsg::SessionForwardInventory {
                session_token: m.session_token,
            },
            Msg::CloseForward(m) => ClientMsg::CloseForward {
                forward_id: m.forward_id,
            },
            Msg::UpsertItem(m) => ClientMsg::UpsertItem(Box::new(ItemWrite {
                bucket_id: m.bucket_id,
                id: m.id,
                external_key: m.external_key,
                title: m.title,
                body: m.body,
                status: item_status_opt(m.status)?,
                priority: item_priority_opt(m.priority)?,
                source_kind: item_source_kind_opt(m.source_kind)?,
                source_detail: m.source_detail,
                url: m.url,
                project_id: m.project_id,
                due_at_unix_ms: m.due_at_unix_ms,
                blocked_by: m.set_blocked_by.then_some(m.blocked_by),
                note: m.note,
                link_session_id: m.link_session_id,
                question: m.question,
                clear_project: m.clear_project,
                clear_due: m.clear_due,
            })),
            Msg::DeleteItem(m) => ClientMsg::DeleteItem {
                bucket_id: m.bucket_id,
                id: m.id,
            },
            Msg::SnoozeItem(m) => ClientMsg::SnoozeItem {
                bucket_id: m.bucket_id,
                id: m.id,
                until_unix_ms: m.until_unix_ms,
            },
            Msg::ListItems(m) => ClientMsg::ListItems(ItemQuery {
                bucket_id: m.bucket_id,
                statuses: m
                    .statuses
                    .into_iter()
                    .map(ItemStatus::try_from)
                    .collect::<Result<_, _>>()?,
                project_id: m.project_id,
                updated_since_unix_ms: m.updated_since_unix_ms,
                include_closed: m.include_closed,
                include_snoozed: m.include_snoozed,
                search: m.search,
                priorities: m
                    .priorities
                    .into_iter()
                    .map(ItemPriority::try_from)
                    .collect::<Result<_, _>>()?,
                source_kinds: m
                    .source_kinds
                    .into_iter()
                    .map(ItemSourceKind::try_from)
                    .collect::<Result<_, _>>()?,
                limit: m.limit,
                offset: m.offset,
                summary_filter: (m.summary_filter != 0)
                    .then(|| ItemSummaryFilter::try_from(m.summary_filter))
                    .transpose()?,
            }),
            Msg::ItemNotes(m) => ClientMsg::ItemNotes {
                bucket_id: m.bucket_id,
                item_id: m.item_id,
            },
            Msg::SetSetting(m) => ClientMsg::SetSetting {
                key: m.key,
                value: m.value,
            },
            Msg::ListSettings(_) => ClientMsg::ListSettings,
            Msg::GetSession(m) => ClientMsg::GetSession {
                session_id: m.session_id,
            },
            Msg::ListEndedSessions(m) => ClientMsg::ListEndedSessions {
                cursor: m.cursor,
                limit: m.limit,
            },
            Msg::SearchSessions(m) => ClientMsg::SearchSessions {
                query: m.query,
                cursor: m.cursor,
                limit: m.limit,
            },
            Msg::MarkSessionSeen(m) => ClientMsg::MarkSessionSeen {
                session_id: m.session_id,
            },
            Msg::UpdateSessionApis(m) => ClientMsg::UpdateSessionApis {
                session_id: m.session_id,
                items_api: m.items_api,
                supervisor_api: m.supervisor_api,
                role: m.role.map(TryInto::try_into).transpose()?,
            },
            Msg::ListInstructions(m) => ClientMsg::ListInstructions {
                bucket_id: m.bucket_id,
                project_id: m.project_id,
            },
            Msg::GetEffectiveInstructions(m) => ClientMsg::GetEffectiveInstructions {
                bucket_id: m.bucket_id,
                project_id: m.project_id,
                role: m.role.try_into()?,
            },
            Msg::SetInstructions(m) => ClientMsg::SetInstructions {
                bucket_id: m.bucket_id,
                project_id: m.project_id,
                target: m.target.try_into()?,
                markdown: m.markdown,
                expected_revision: m.expected_revision,
                note: m.note,
            },
            Msg::RevertInstructions(m) => ClientMsg::RevertInstructions {
                layer_id: m.layer_id,
                revision: m.revision,
                expected_revision: m.expected_revision,
                note: m.note,
            },
            Msg::RespondToItem(m) => {
                use wire::respond_to_item::Target;
                let target = match m
                    .target
                    .ok_or(DecodeError::Missing("RespondToItem.target"))?
                {
                    Target::SessionId(id) => RespondTarget::Session(id),
                    Target::NewSupervisor(target) => RespondTarget::NewSupervisor {
                        project_id: target.project_id,
                    },
                    Target::ReplyOnly(_) => RespondTarget::ReplyOnly,
                };
                ClientMsg::RespondToItem {
                    bucket_id: m.bucket_id,
                    item_id: m.item_id,
                    text: m.text,
                    target,
                }
            }
        };
        Ok(ClientEnvelope { seq: v.seq, msg })
    }
}

impl From<Event> for wire::Event {
    fn from(v: Event) -> Self {
        use wire::event::Event as W;
        let event = match v {
            Event::SessionChanged(s) => W::SessionChanged(s.into()),
            Event::SessionRemoved(id) => W::SessionRemoved(id),
            Event::BucketChanged(b) => W::BucketChanged(b.into()),
            Event::BucketRemoved(id) => W::BucketRemoved(id),
            Event::ProjectChanged(p) => W::ProjectChanged(p.into()),
            Event::ProjectRemoved(id) => W::ProjectRemoved(id),
            Event::WorkerChanged(w) => W::WorkerChanged(w.into()),
            Event::WorkerRemoved(id) => W::WorkerRemoved(id),
            Event::TerminalChanged(t) => W::TerminalChanged(t.into()),
            Event::TerminalRemoved(id) => W::TerminalRemoved(id),
            Event::ContextChanged(c) => W::ContextChanged(c.into()),
            Event::ForwardChanged(f) => W::ForwardChanged(f.into()),
            Event::ForwardRemoved(id) => W::ForwardRemoved(id),
            Event::ItemChanged(i) => W::ItemChanged(i.into()),
            Event::ItemRemoved(reference) => W::ItemRemoved(wire::ItemRef {
                bucket_id: reference.bucket_id,
                item_id: reference.item_id,
            }),
            Event::BriefingChanged(b) => W::BriefingChanged(b.into()),
            Event::UserSettingChanged(setting) => W::UserSettingChanged(wire::UserSettingChanged {
                key: setting.key,
                value_json: setting.value_json,
            }),
            Event::InstructionLayerChanged(layer) => W::InstructionLayerChanged(layer.into()),
            Event::ModelProfileChanged(profile) => W::ModelProfileChanged(profile.into()),
            Event::ModelProfileRemoved(id) => W::ModelProfileRemoved(id),
            Event::ReviewChanged(r) => W::ReviewChanged(r.into()),
            Event::ReviewRemoved(id) => W::ReviewRemoved(id),
            Event::PlanChanged(p) => W::PlanChanged(p.into()),
            Event::PlanRemoved(id) => W::PlanRemoved(id),
            Event::SessionAlert(alert) => W::SessionAlert(wire::SessionAlert {
                session_id: alert.session_id,
                kind: match alert.kind {
                    SessionAlertKind::NeedsInput => wire::SessionAlertKind::NeedsInput as i32,
                    SessionAlertKind::Failed => wire::SessionAlertKind::Failed as i32,
                    SessionAlertKind::Completed => wire::SessionAlertKind::Completed as i32,
                },
            }),
            Event::SecurityNotice(notice) => W::SecurityNotice(wire::SecurityNotice {
                kind: match notice.kind {
                    SecurityNoticeKind::DeviceEnrolled => {
                        wire::SecurityNoticeKind::DeviceEnrolled as i32
                    }
                    SecurityNoticeKind::HostEnrolled => {
                        wire::SecurityNoticeKind::HostEnrolled as i32
                    }
                    SecurityNoticeKind::HostKeyReplaced => {
                        wire::SecurityNoticeKind::HostKeyReplaced as i32
                    }
                    SecurityNoticeKind::InstructionsRewritten => {
                        wire::SecurityNoticeKind::InstructionsRewritten as i32
                    }
                },
                subject: notice.subject,
                detail: notice.detail,
            }),
        };
        wire::Event { event: Some(event) }
    }
}

impl TryFrom<wire::Event> for Event {
    type Error = DecodeError;
    fn try_from(v: wire::Event) -> Result<Self, DecodeError> {
        use wire::event::Event as W;
        Ok(match v.event.ok_or(DecodeError::Missing("Event.event"))? {
            W::SessionChanged(s) => Event::SessionChanged(s.try_into()?),
            W::SessionRemoved(id) => Event::SessionRemoved(id),
            W::BucketChanged(b) => Event::BucketChanged(b.try_into()?),
            W::BucketRemoved(id) => Event::BucketRemoved(id),
            W::ProjectChanged(p) => Event::ProjectChanged(p.try_into()?),
            W::ProjectRemoved(id) => Event::ProjectRemoved(id),
            W::WorkerChanged(w) => Event::WorkerChanged(w.try_into()?),
            W::WorkerRemoved(id) => Event::WorkerRemoved(id),
            W::TerminalChanged(t) => Event::TerminalChanged(t.try_into()?),
            W::TerminalRemoved(id) => Event::TerminalRemoved(id),
            W::ContextChanged(c) => Event::ContextChanged(c.into()),
            W::ForwardChanged(f) => Event::ForwardChanged(f.try_into()?),
            W::ForwardRemoved(id) => Event::ForwardRemoved(id),
            W::ItemChanged(i) => Event::ItemChanged(i.try_into()?),
            W::ItemRemoved(reference) => Event::ItemRemoved(ItemRef {
                bucket_id: reference.bucket_id,
                item_id: reference.item_id,
            }),
            W::BriefingChanged(b) => Event::BriefingChanged(b.into()),
            W::UserSettingChanged(setting) => Event::UserSettingChanged(UserSettingChanged {
                key: setting.key,
                value_json: setting.value_json,
            }),
            W::InstructionLayerChanged(layer) => Event::InstructionLayerChanged(layer.try_into()?),
            W::ModelProfileChanged(profile) => Event::ModelProfileChanged(profile.try_into()?),
            W::ModelProfileRemoved(id) => Event::ModelProfileRemoved(id),
            W::ReviewChanged(r) => Event::ReviewChanged(r.into()),
            W::ReviewRemoved(id) => Event::ReviewRemoved(id),
            W::PlanChanged(p) => Event::PlanChanged(p.into()),
            W::PlanRemoved(id) => Event::PlanRemoved(id),
            W::SessionAlert(alert) => Event::SessionAlert(SessionAlert {
                session_id: alert.session_id,
                kind: alert.kind.try_into()?,
            }),
            W::SecurityNotice(notice) => Event::SecurityNotice(SecurityNotice {
                kind: notice.kind.try_into()?,
                subject: notice.subject,
                detail: notice.detail,
            }),
        })
    }
}

impl From<ServerMsg> for wire::ServerMessage {
    fn from(v: ServerMsg) -> Self {
        use wire::server_message::Msg;
        let msg = match v {
            ServerMsg::Snapshot(s) => Msg::Snapshot(wire::Snapshot {
                buckets: s.buckets.into_iter().map(Into::into).collect(),
                projects: s.projects.into_iter().map(Into::into).collect(),
                sessions: s.sessions.into_iter().map(Into::into).collect(),
                workers: s.workers.into_iter().map(Into::into).collect(),
                terminals: s.terminals.into_iter().map(Into::into).collect(),
                contexts: s.contexts.into_iter().map(Into::into).collect(),
                forwards: s.forwards.into_iter().map(Into::into).collect(),
                items: s.items.into_iter().map(Into::into).collect(),
                briefings: s.briefings.into_iter().map(Into::into).collect(),
                user_settings: s
                    .user_settings
                    .into_iter()
                    .map(|setting| wire::UserSetting {
                        key: setting.key,
                        value_json: setting.value_json,
                    })
                    .collect(),
                instruction_layers: s.instruction_layers.into_iter().map(Into::into).collect(),
                model_profiles: s.model_profiles.into_iter().map(Into::into).collect(),
                agent_dialects: s.agent_dialects.into_iter().map(Into::into).collect(),
                reviews: s.reviews.into_iter().map(Into::into).collect(),
                plans: s.plans.into_iter().map(Into::into).collect(),
                review_viewer_states: s.review_viewer_states.into_iter().map(Into::into).collect(),
            }),
            ServerMsg::Event(e) => Msg::Event(e.into()),
            ServerMsg::PtyOutput {
                session_id,
                data,
                replay,
                terminal_id,
                generation,
            } => Msg::PtyOutput(wire::PtyOutput {
                session_id,
                data: data.to_vec(),
                replay,
                terminal_id,
                generation,
            }),
            ServerMsg::CommandResult { seq, result, data } => {
                let (ok, error, created_id) = match result {
                    Ok(id) => (true, String::new(), id),
                    Err(e) => (false, e, None),
                };
                Msg::CommandResult(wire::CommandResult {
                    seq,
                    ok,
                    error,
                    created_id,
                    data: data.to_vec(),
                })
            }
        };
        wire::ServerMessage { msg: Some(msg) }
    }
}

impl TryFrom<wire::ServerMessage> for ServerMsg {
    type Error = DecodeError;
    fn try_from(v: wire::ServerMessage) -> Result<Self, DecodeError> {
        use wire::server_message::Msg;
        Ok(
            match v.msg.ok_or(DecodeError::Missing("ServerMessage.msg"))? {
                Msg::Snapshot(s) => ServerMsg::Snapshot(Snapshot {
                    buckets: s
                        .buckets
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    projects: s
                        .projects
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    sessions: s
                        .sessions
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    workers: s
                        .workers
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    terminals: s
                        .terminals
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    contexts: s.contexts.into_iter().map(Into::into).collect(),
                    forwards: s
                        .forwards
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    items: s
                        .items
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    briefings: s.briefings.into_iter().map(Into::into).collect(),
                    user_settings: s
                        .user_settings
                        .into_iter()
                        .map(|setting| UserSetting {
                            key: setting.key,
                            value_json: setting.value_json,
                        })
                        .collect(),
                    instruction_layers: s
                        .instruction_layers
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    model_profiles: s
                        .model_profiles
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    agent_dialects: s
                        .agent_dialects
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    reviews: s.reviews.into_iter().map(Into::into).collect(),
                    plans: s.plans.into_iter().map(Into::into).collect(),
                    review_viewer_states: s
                        .review_viewer_states
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                }),
                Msg::Event(e) => ServerMsg::Event(e.try_into()?),
                Msg::PtyOutput(p) => ServerMsg::PtyOutput {
                    session_id: p.session_id,
                    data: p.data.into(),
                    replay: p.replay,
                    terminal_id: p.terminal_id,
                    generation: p.generation,
                },
                Msg::CommandResult(r) => ServerMsg::CommandResult {
                    seq: r.seq,
                    result: if r.ok { Ok(r.created_id) } else { Err(r.error) },
                    data: r.data.into(),
                },
            },
        )
    }
}

impl From<WorkerMsg> for wire::WorkerMessage {
    fn from(v: WorkerMsg) -> Self {
        use wire::worker_message::Msg;
        let msg = match v {
            WorkerMsg::Register {
                protocol_version,
                enrollment_token,
                credential,
                hostname,
                platform,
                pm_version,
                default_project_root,
                live_sessions,
                live_terminals,
                pending_transcripts,
                live_dir_shares,
                runtime,
                container,
            } => Msg::Register(wire::WorkerRegister {
                protocol_version,
                enrollment_token,
                credential,
                hostname,
                platform,
                pm_version,
                runtime,
                container,
                default_project_root,
                live_sessions,
                live_terminals: live_terminals
                    .into_iter()
                    .map(|terminal| wire::WorkerTerminal {
                        terminal_id: terminal.terminal_id,
                        generation: terminal.generation,
                        kind: wire::TerminalKind::from(terminal.kind) as i32,
                        state: wire::TerminalRunState::from(terminal.state) as i32,
                        agent_resumable: terminal.agent_resumable,
                        transcript_available: terminal.transcript_available,
                    })
                    .collect(),
                pending_transcripts: pending_transcripts
                    .into_iter()
                    .map(|transcript| wire::WorkerTranscript {
                        terminal_id: transcript.terminal_id,
                        generation: transcript.generation,
                        size: transcript.size,
                    })
                    .collect(),
                live_dir_shares: live_dir_shares
                    .into_iter()
                    .map(|share| wire::WorkerDirShare {
                        share_id: share.share_id,
                        port: share.port as u32,
                    })
                    .collect(),
            }),
            WorkerMsg::Heartbeat => Msg::Heartbeat(wire::WorkerHeartbeat {}),
            WorkerMsg::SessionState {
                session_id,
                state,
                detail,
            } => Msg::SessionState(wire::WorkerSessionState {
                session_id,
                state: wire::SessionState::from(state) as i32,
                detail,
            }),
            WorkerMsg::SessionExit {
                session_id,
                exit_code,
            } => Msg::SessionExit(wire::WorkerSessionExit {
                session_id,
                exit_code,
            }),
            WorkerMsg::TerminalExit {
                terminal_id,
                generation,
                exit_code,
                state,
                transcript_available,
                transcript_size,
                detail,
            } => Msg::TerminalExit(wire::WorkerTerminalExit {
                terminal_id,
                generation,
                exit_code,
                state: wire::TerminalRunState::from(state) as i32,
                transcript_available,
                transcript_size,
                detail,
            }),
            WorkerMsg::NeedsInput { session_id } => {
                Msg::NeedsInput(wire::WorkerNeedsInput { session_id })
            }
            WorkerMsg::ProgramStatus {
                terminal_id,
                generation,
                reset,
                records,
                removed,
            } => Msg::ProgramStatus(wire::WorkerProgramStatus {
                terminal_id,
                generation,
                reset,
                records: records
                    .into_iter()
                    .map(wire::ProgramStatusRecord::from)
                    .collect(),
                removed,
            }),
            WorkerMsg::TerminalActivity {
                terminal_id,
                generation,
            } => Msg::TerminalActivity(wire::WorkerTerminalActivity {
                terminal_id,
                generation,
            }),
            WorkerMsg::HookReport {
                session_token,
                kind,
                detail,
                agent_session_id,
                transcript_path,
                req_id,
                background_work,
            } => Msg::HookReport(wire::WorkerHookReport {
                session_token,
                kind: wire::HookKind::from(kind) as i32,
                detail,
                agent_session_id,
                transcript_path,
                req_id,
                background_work,
            }),
            WorkerMsg::HarnessStatus { req_id, status } => {
                Msg::HarnessStatus(wire::WorkerHarnessStatus {
                    req_id,
                    status: Some(wire::HarnessStatus {
                        state: status.state,
                        command: status.command,
                        output: status.output,
                        error: status.error,
                    }),
                })
            }
            WorkerMsg::PathChecked {
                req_id,
                status,
                detail,
            } => Msg::PathChecked(wire::WorkerPathChecked {
                req_id,
                status: wire::PathCheck::from(status) as i32,
                detail,
            }),
            WorkerMsg::AgentInboxResult {
                req_id,
                outcome,
                transport,
                mode,
                detail,
            } => Msg::AgentInboxResult(wire::WorkerAgentInboxResult {
                req_id,
                outcome: wire::AgentInboxOutcome::from(outcome) as i32,
                transport,
                mode: wire::AgentInboxMode::from(mode) as i32,
                detail,
            }),
            WorkerMsg::FsListing {
                req_id,
                ok,
                error,
                dir,
                parent,
                entries,
            } => Msg::FsListing(wire::WorkerFsListing {
                req_id,
                ok,
                error,
                dir,
                parent,
                entries: entries
                    .into_iter()
                    .map(|e| wire::FsEntry {
                        name: e.name,
                        path: e.path,
                    })
                    .collect(),
            }),
            WorkerMsg::FileRead {
                req_id,
                ok,
                error,
                content,
                filename,
            } => Msg::FileRead(wire::WorkerFileRead {
                req_id,
                ok,
                error,
                content,
                filename,
            }),
            WorkerMsg::McpRequest {
                req_id,
                bearer,
                body,
            } => Msg::McpRequest(wire::WorkerMcpRequest {
                req_id,
                bearer,
                body,
            }),
            WorkerMsg::ForwardOpened { req_id, ok, error } => {
                Msg::ForwardOpened(wire::WorkerForwardOpened { req_id, ok, error })
            }
            WorkerMsg::DirShareBound {
                share_id,
                ok,
                error,
                port,
            } => Msg::DirShareBound(wire::WorkerDirShareBound {
                share_id,
                ok,
                error,
                port: port as u32,
            }),
            WorkerMsg::RepoResponse {
                req_id,
                ok,
                error,
                answer,
            } => Msg::RepoResponse(wire::WorkerRepoResponse {
                req_id,
                ok,
                error,
                answer,
            }),
        };
        wire::WorkerMessage { msg: Some(msg) }
    }
}

impl TryFrom<wire::WorkerMessage> for WorkerMsg {
    type Error = DecodeError;
    fn try_from(v: wire::WorkerMessage) -> Result<Self, DecodeError> {
        use wire::worker_message::Msg;
        Ok(
            match v.msg.ok_or(DecodeError::Missing("WorkerMessage.msg"))? {
                Msg::RepoResponse(m) => WorkerMsg::RepoResponse {
                    req_id: m.req_id,
                    ok: m.ok,
                    error: m.error,
                    answer: m.answer.to_vec(),
                },
                Msg::Register(m) => WorkerMsg::Register {
                    protocol_version: m.protocol_version,
                    enrollment_token: m.enrollment_token,
                    credential: m.credential,
                    hostname: m.hostname,
                    platform: m.platform,
                    pm_version: m.pm_version,
                    runtime: m.runtime,
                    container: m.container,
                    default_project_root: m.default_project_root,
                    live_sessions: m.live_sessions,
                    live_terminals: m
                        .live_terminals
                        .into_iter()
                        .map(|terminal| {
                            Ok(crate::domain::WorkerTerminal {
                                terminal_id: terminal.terminal_id,
                                generation: terminal.generation,
                                kind: terminal.kind.try_into()?,
                                state: terminal.state.try_into()?,
                                agent_resumable: terminal.agent_resumable,
                                transcript_available: terminal.transcript_available,
                            })
                        })
                        .collect::<Result<Vec<_>, DecodeError>>()?,
                    pending_transcripts: m
                        .pending_transcripts
                        .into_iter()
                        .map(|transcript| crate::domain::WorkerTranscript {
                            terminal_id: transcript.terminal_id,
                            generation: transcript.generation,
                            size: transcript.size,
                        })
                        .collect(),
                    live_dir_shares: m
                        .live_dir_shares
                        .into_iter()
                        .map(|share| {
                            Ok(crate::domain::WorkerDirShare {
                                share_id: share.share_id,
                                port: u16::try_from(share.port)
                                    .map_err(|_| DecodeError::OutOfRange("port"))?,
                            })
                        })
                        .collect::<Result<Vec<_>, DecodeError>>()?,
                },
                Msg::Heartbeat(_) => WorkerMsg::Heartbeat,
                Msg::SessionState(m) => WorkerMsg::SessionState {
                    session_id: m.session_id,
                    state: m.state.try_into()?,
                    detail: m.detail,
                },
                Msg::SessionExit(m) => WorkerMsg::SessionExit {
                    session_id: m.session_id,
                    exit_code: m.exit_code,
                },
                Msg::TerminalExit(m) => WorkerMsg::TerminalExit {
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                    exit_code: m.exit_code,
                    state: m.state.try_into()?,
                    transcript_available: m.transcript_available,
                    transcript_size: m.transcript_size,
                    detail: m.detail,
                },
                Msg::NeedsInput(m) => WorkerMsg::NeedsInput {
                    session_id: m.session_id,
                },
                Msg::TerminalActivity(m) => WorkerMsg::TerminalActivity {
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                },
                Msg::ProgramStatus(m) => WorkerMsg::ProgramStatus {
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                    reset: m.reset,
                    records: program_status_records(m.records),
                    removed: m.removed,
                },
                Msg::HookReport(m) => WorkerMsg::HookReport {
                    session_token: m.session_token,
                    kind: m.kind.try_into()?,
                    detail: m.detail,
                    agent_session_id: m.agent_session_id,
                    transcript_path: m.transcript_path,
                    req_id: m.req_id,
                    // An older worker sends no such field,
                    // which decodes to false: the answer it has
                    // always given.
                    background_work: m.background_work,
                },
                Msg::HarnessStatus(m) => {
                    let s = m.status.unwrap_or_default();
                    WorkerMsg::HarnessStatus {
                        req_id: m.req_id,
                        status: HarnessStatus {
                            state: s.state,
                            command: s.command,
                            output: s.output,
                            error: s.error,
                        },
                    }
                }
                Msg::PathChecked(m) => WorkerMsg::PathChecked {
                    req_id: m.req_id,
                    status: m.status.try_into()?,
                    detail: m.detail,
                },
                Msg::AgentInboxResult(m) => WorkerMsg::AgentInboxResult {
                    req_id: m.req_id,
                    outcome: m.outcome.try_into()?,
                    transport: m.transport,
                    mode: m.mode.try_into()?,
                    detail: m.detail,
                },
                Msg::FsListing(m) => WorkerMsg::FsListing {
                    req_id: m.req_id,
                    ok: m.ok,
                    error: m.error,
                    dir: m.dir,
                    parent: m.parent,
                    entries: m
                        .entries
                        .into_iter()
                        .map(|e| FsEntry {
                            name: e.name,
                            path: e.path,
                        })
                        .collect(),
                },
                Msg::FileRead(m) => WorkerMsg::FileRead {
                    req_id: m.req_id,
                    ok: m.ok,
                    error: m.error,
                    content: m.content,
                    filename: m.filename,
                },
                Msg::McpRequest(m) => WorkerMsg::McpRequest {
                    req_id: m.req_id,
                    bearer: m.bearer,
                    body: m.body,
                },
                Msg::ForwardOpened(m) => WorkerMsg::ForwardOpened {
                    req_id: m.req_id,
                    ok: m.ok,
                    error: m.error,
                },
                Msg::DirShareBound(m) => WorkerMsg::DirShareBound {
                    share_id: m.share_id,
                    ok: m.ok,
                    error: m.error,
                    port: u16::try_from(m.port).map_err(|_| DecodeError::OutOfRange("port"))?,
                },
            },
        )
    }
}

impl From<ControllerMsg> for wire::ControllerMessage {
    fn from(v: ControllerMsg) -> Self {
        use wire::controller_message::Msg;
        let msg = match v {
            ControllerMsg::Registered {
                worker_id,
                credential,
                error,
                mcp_base_url,
                http_port,
                pm_version,
            } => Msg::Registered(wire::ControllerRegistered {
                worker_id,
                credential,
                error,
                mcp_base_url,
                http_port,
                pm_version,
            }),
            ControllerMsg::UpdateNow => Msg::UpdateNow(wire::ControllerUpdateNow {}),
            ControllerMsg::RepoRequest { req_id, op } => {
                Msg::RepoRequest(wire::ControllerRepoRequest { req_id, op })
            }
            ControllerMsg::HookResult { req_id, nudge } => {
                Msg::HookResult(wire::ControllerHookResult { req_id, nudge })
            }
            ControllerMsg::Spawn {
                session_id,
                agent,
                task_prompt,
                permission_mode,
                cwd,
                session_token,
                resume_agent_session_id,
                terminal_id,
                generation,
                truecolor,
                fullscreen,
                compiled_instructions,
                model_endpoint,
                initial_cols,
                initial_rows,
                program_status,
            } => Msg::Spawn(wire::ControllerSpawn {
                session_id,
                agent: wire::AgentKind::from(agent) as i32,
                task_prompt,
                permission_mode: wire::PermissionMode::from(permission_mode) as i32,
                cwd,
                session_token,
                resume_agent_session_id,
                terminal_id,
                generation,
                truecolor: Some(truecolor),
                fullscreen: Some(fullscreen),
                compiled_instructions,
                model_endpoint: model_endpoint.map(|endpoint| Box::new((*endpoint).into())),
                initial_cols: initial_cols.map(|c| c as u32),
                initial_rows: initial_rows.map(|r| r as u32),
                program_status,
            }),
            ControllerMsg::SpawnShell {
                terminal_id,
                generation,
                cwd,
                truecolor,
                initial_cols,
                initial_rows,
            } => Msg::SpawnShell(wire::ControllerSpawnShell {
                terminal_id,
                generation,
                cwd,
                truecolor: Some(truecolor),
                initial_cols: initial_cols.map(|c| c as u32),
                initial_rows: initial_rows.map(|r| r as u32),
            }),
            ControllerMsg::Interrupt { session_id } => Msg::Interrupt(wire::ControllerInterrupt {
                session_id,
                terminal_id: 0,
                generation: 0,
            }),
            ControllerMsg::TerminalInterrupt {
                terminal_id,
                generation,
            } => Msg::Interrupt(wire::ControllerInterrupt {
                session_id: 0,
                terminal_id,
                generation,
            }),
            ControllerMsg::Kill { session_id } => Msg::Kill(wire::ControllerKill {
                session_id,
                terminal_id: 0,
                generation: 0,
            }),
            ControllerMsg::TerminalKill {
                terminal_id,
                generation,
            } => Msg::Kill(wire::ControllerKill {
                session_id: 0,
                terminal_id,
                generation,
            }),
            ControllerMsg::FsList { req_id, path } => {
                Msg::FsList(wire::ControllerFsList { req_id, path })
            }
            ControllerMsg::HarnessRequest {
                req_id,
                agent,
                install,
            } => Msg::HarnessRequest(wire::ControllerHarnessRequest {
                req_id,
                agent: wire::AgentKind::from(agent) as i32,
                install,
            }),
            ControllerMsg::PathCheck { req_id, path } => {
                Msg::PathCheck(wire::ControllerPathCheck { req_id, path })
            }
            ControllerMsg::AgentInbox {
                req_id,
                session_id,
                agent_terminal_id,
                agent,
                agent_session_id,
                agent_port,
                text,
                mode,
            } => Msg::AgentInbox(wire::ControllerAgentInbox {
                req_id,
                session_id,
                agent_terminal_id,
                agent: wire::AgentKind::from(agent) as i32,
                agent_session_id,
                agent_port: agent_port.unwrap_or(0) as u32,
                text,
                mode: wire::AgentInboxMode::from(mode) as i32,
            }),
            ControllerMsg::FileRead {
                req_id,
                root,
                path,
                max_bytes,
            } => Msg::FileRead(wire::ControllerFileRead {
                req_id,
                root,
                path,
                max_bytes,
            }),
            ControllerMsg::McpResponse {
                req_id,
                status,
                body,
            } => Msg::McpResponse(wire::ControllerMcpResponse {
                req_id,
                status,
                body,
            }),
            ControllerMsg::TerminalAttach {
                terminal_id,
                generation,
                token,
                replay_bytes,
                size: (cols, rows),
            } => Msg::Attach(wire::ControllerAttach {
                terminal_id,
                generation,
                token,
                replay_bytes,
                cols: cols.into(),
                rows: rows.into(),
            }),
            ControllerMsg::TerminalDetach {
                terminal_id,
                generation,
            } => Msg::Detach(wire::ControllerDetach {
                terminal_id,
                generation,
            }),
            ControllerMsg::Transcript {
                terminal_id,
                generation,
                token,
            } => Msg::Transcript(wire::ControllerTranscript {
                terminal_id,
                generation,
                token,
            }),
            ControllerMsg::TranscriptAck {
                terminal_id,
                generation,
            } => Msg::TranscriptAck(wire::ControllerTranscriptAck {
                terminal_id,
                generation,
            }),
            ControllerMsg::ForwardOpen {
                req_id,
                port,
                token,
            } => Msg::ForwardOpen(wire::ControllerForwardOpen {
                req_id,
                port: port as u32,
                token,
            }),
            ControllerMsg::DirShareServe {
                share_id,
                root,
                path,
            } => Msg::DirShareServe(wire::ControllerDirShareServe {
                share_id,
                root,
                path,
            }),
            ControllerMsg::DirShareStop { share_id } => {
                Msg::DirShareStop(wire::ControllerDirShareStop { share_id })
            }
        };
        wire::ControllerMessage { msg: Some(msg) }
    }
}

impl TryFrom<wire::ControllerMessage> for ControllerMsg {
    type Error = DecodeError;
    fn try_from(v: wire::ControllerMessage) -> Result<Self, DecodeError> {
        use wire::controller_message::Msg;
        Ok(
            match v.msg.ok_or(DecodeError::Missing("ControllerMessage.msg"))? {
                Msg::RepoRequest(m) => ControllerMsg::RepoRequest {
                    req_id: m.req_id,
                    op: m.op.to_vec(),
                },
                Msg::Registered(m) => ControllerMsg::Registered {
                    worker_id: m.worker_id,
                    credential: m.credential,
                    error: m.error,
                    mcp_base_url: m.mcp_base_url,
                    http_port: m.http_port,
                    pm_version: m.pm_version,
                },
                Msg::UpdateNow(_) => ControllerMsg::UpdateNow,
                Msg::HookResult(m) => ControllerMsg::HookResult {
                    req_id: m.req_id,
                    nudge: m.nudge,
                },
                Msg::Spawn(m) => ControllerMsg::Spawn {
                    session_id: m.session_id,
                    agent: m.agent.try_into()?,
                    task_prompt: m.task_prompt,
                    permission_mode: m.permission_mode.try_into()?,
                    cwd: m.cwd,
                    session_token: m.session_token,
                    resume_agent_session_id: m.resume_agent_session_id,
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                    truecolor: m.truecolor.unwrap_or(true),
                    fullscreen: m.fullscreen.unwrap_or(true),
                    compiled_instructions: m.compiled_instructions,
                    model_endpoint: m
                        .model_endpoint
                        .map(|endpoint| ResolvedModelEndpoint::try_from(*endpoint))
                        .transpose()?
                        .map(Box::new),
                    initial_cols: m.initial_cols.map(|c| c as u16),
                    initial_rows: m.initial_rows.map(|r| r as u16),
                    program_status: m.program_status,
                },
                Msg::SpawnShell(m) => ControllerMsg::SpawnShell {
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                    cwd: m.cwd,
                    truecolor: m.truecolor.unwrap_or(true),
                    initial_cols: m.initial_cols.map(|c| c as u16),
                    initial_rows: m.initial_rows.map(|r| r as u16),
                },
                Msg::Interrupt(m) => {
                    if m.terminal_id != 0 {
                        ControllerMsg::TerminalInterrupt {
                            terminal_id: m.terminal_id,
                            generation: m.generation,
                        }
                    } else {
                        ControllerMsg::Interrupt {
                            session_id: m.session_id,
                        }
                    }
                }
                Msg::Kill(m) => {
                    if m.terminal_id != 0 {
                        ControllerMsg::TerminalKill {
                            terminal_id: m.terminal_id,
                            generation: m.generation,
                        }
                    } else {
                        ControllerMsg::Kill {
                            session_id: m.session_id,
                        }
                    }
                }
                Msg::FsList(m) => ControllerMsg::FsList {
                    req_id: m.req_id,
                    path: m.path,
                },
                Msg::HarnessRequest(m) => ControllerMsg::HarnessRequest {
                    req_id: m.req_id,
                    agent: m.agent.try_into()?,
                    install: m.install,
                },
                Msg::PathCheck(m) => ControllerMsg::PathCheck {
                    req_id: m.req_id,
                    path: m.path,
                },
                Msg::AgentInbox(m) => ControllerMsg::AgentInbox {
                    req_id: m.req_id,
                    session_id: m.session_id,
                    agent_terminal_id: m.agent_terminal_id,
                    agent: m.agent.try_into()?,
                    agent_session_id: m.agent_session_id,
                    agent_port: u16::try_from(m.agent_port).ok().filter(|p| *p != 0),
                    text: m.text,
                    mode: m.mode.try_into()?,
                },
                Msg::FileRead(m) => ControllerMsg::FileRead {
                    req_id: m.req_id,
                    root: m.root,
                    path: m.path,
                    max_bytes: m.max_bytes,
                },
                Msg::McpResponse(m) => ControllerMsg::McpResponse {
                    req_id: m.req_id,
                    status: m.status,
                    body: m.body,
                },
                Msg::Attach(m) => ControllerMsg::TerminalAttach {
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                    token: m.token,
                    replay_bytes: m.replay_bytes,
                    size: (
                        u16::try_from(m.cols).unwrap_or(0),
                        u16::try_from(m.rows).unwrap_or(0),
                    ),
                },
                Msg::Detach(m) => ControllerMsg::TerminalDetach {
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                },
                Msg::Transcript(m) => ControllerMsg::Transcript {
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                    token: m.token,
                },
                Msg::TranscriptAck(m) => ControllerMsg::TranscriptAck {
                    terminal_id: m.terminal_id,
                    generation: m.generation,
                },
                Msg::ForwardOpen(m) => ControllerMsg::ForwardOpen {
                    req_id: m.req_id,
                    port: u16::try_from(m.port).map_err(|_| DecodeError::OutOfRange("port"))?,
                    token: m.token,
                },
                Msg::DirShareServe(m) => ControllerMsg::DirShareServe {
                    share_id: m.share_id,
                    root: m.root,
                    path: m.path,
                },
                Msg::DirShareStop(m) => ControllerMsg::DirShareStop {
                    share_id: m.share_id,
                },
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every modelled kind must survive the wire round trip and its own
    /// string form. Walking the constant is what makes a newly added
    /// kind fail here instead of silently decoding as an error.
    #[test]
    fn every_agent_kind_round_trips_through_the_wire_and_its_name() {
        for agent in AgentKind::ALL.iter().copied() {
            let encoded = wire::AgentKind::from(agent) as i32;
            assert_ne!(encoded, wire::AgentKind::Unspecified as i32);
            assert_eq!(AgentKind::try_from(encoded).unwrap(), agent);
            assert_eq!(AgentKind::parse(agent.as_str()), Some(agent));
        }
    }

    #[test]
    fn every_model_dialect_round_trips_through_the_wire_and_its_name() {
        for dialect in ModelDialect::ALL.iter().copied() {
            let encoded = wire::ModelDialect::from(dialect) as i32;
            assert_ne!(encoded, wire::ModelDialect::Unspecified as i32);
            assert_eq!(ModelDialect::try_from(encoded).unwrap(), dialect);
            assert_eq!(ModelDialect::parse(dialect.as_str()), Some(dialect));
        }
    }

    fn sample_forward() -> SessionForward {
        SessionForward {
            id: 11,
            session_id: 7,
            worker_port: 5173,
            listener_port: 41783,
            slug: "vite".into(),
            label: "vite".into(),
            scheme: "http".into(),
            created_at_unix_ms: 1_700_000_000_000,
            url: "http://pm-box:41783".into(),
            target_reachable: Some(true),
            source_path: String::new(),
        }
    }

    #[test]
    fn display_name_prefers_goal_then_title_then_headline() {
        let mut session = sample_session();
        assert_eq!(session.display_name(), "Moving auth to JWTs");
        session.goal.clear();
        assert_eq!(session.display_name(), "fix auth");
        session.task_title.clear();
        assert_eq!(session.display_name(), "migrating auth");
        session.headline.clear();
        assert_eq!(session.display_name(), "session 7");
    }

    fn sample_session() -> Session {
        Session {
            id: 7,
            project_id: 3,
            agent: AgentKind::ClaudeCode,
            agent_source: AgentSelectionSource::Explicit,
            state: SessionState::NeedsInput,
            task_title: "fix auth".into(),
            task_prompt: "fix the auth bug".into(),
            agent_session_id: Some("abc-123".into()),
            created_at_unix_ms: 1_700_000_000_000,
            ended_at_unix_ms: None,
            exit_code: None,
            state_detail: "waiting on permission".into(),
            activity: "running tests".into(),
            progress_percent: Some(40),
            resumable: true,
            permission_mode: PermissionMode::Bypass,
            worker_id: 0,
            cwd: "/tmp/api".into(),
            goal: "Moving auth to JWTs".into(),
            headline: "migrating auth".into(),
            git: None,
            summary: "swapping session cookies for JWTs".into(),
            items_api: true,
            supervisor_api: true,
            spawned_by_session_id: Some(5),
            role: SessionRole::Supervisor,
            last_activity_at_unix_ms: 1_700_000_100_000,
            last_agent_activity_at_unix_ms: 1_700_000_090_000,
            last_user_interaction_at_unix_ms: 1_700_000_100_000,
            needs_input_unseen: true,
            idle_unseen: true,
            model_profile_id: Some(11),
            model_profile_source: Some(ModelProfileSource::Bucket),
            program_status: Vec::new(),
        }
    }

    fn sample_model_profile() -> ModelProfile {
        ModelProfile {
            id: 11,
            name: "Gateway".into(),
            key_set: true,
            endpoints: vec![
                ModelProfileEndpoint {
                    profile_id: 11,
                    dialect: ModelDialect::AnthropicMessages,
                    model: "gw/big".into(),
                    base_url: "https://gw.example/v1".into(),
                    background_model: "gw/small".into(),
                },
                ModelProfileEndpoint {
                    profile_id: 11,
                    dialect: ModelDialect::OpenaiResponses,
                    model: "gw/o".into(),
                    base_url: String::new(),
                    background_model: String::new(),
                },
            ],
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_050_000,
        }
    }

    fn sample_item() -> Item {
        Item {
            id: 21,
            bucket_id: 1,
            project_id: Some(3),
            external_key: Some("github:pr:acme/api#412".into()),
            title: "Review PR #412".into(),
            body: "requested two days ago".into(),
            question: "Should this ship today?".into(),
            status: ItemStatus::Inbox,
            priority: ItemPriority::High,
            source_kind: ItemSourceKind::Github,
            source_detail: "acme/api".into(),
            url: "https://example.invalid/pr/412".into(),
            due_at_unix_ms: Some(1_700_000_500_000),
            snoozed_until_unix_ms: None,
            created_by_session_id: Some(7),
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_100_000,
            done_at_unix_ms: None,
            blocked_by: vec![19],
            session_ids: vec![7],
        }
    }

    #[test]
    fn client_messages_round_trip() {
        let msgs = vec![
            ClientMsg::Subscribe { scope: Scope::All },
            ClientMsg::Subscribe {
                scope: Scope::Session(9),
            },
            ClientMsg::SpawnSession {
                project_id: 3,
                agent: Some(AgentKind::Codex),
                task_title: "t".into(),
                task_prompt: "p".into(),
                cwd: "/tmp/override".into(),
                permission_mode: PermissionMode::Auto,
                worker_id: Some(2),
                items_api: false,
                supervisor_api: true,
                model_profile_id: Some(11),
                host: "mac-vm".into(),
                initial_cols: Some(100),
                initial_rows: Some(30),
            },
            ClientMsg::AttachPty { session_id: 1 },
            ClientMsg::DetachPty { session_id: 1 },
            ClientMsg::PtyInput {
                session_id: 1,
                data: bytes::Bytes::from_static(b"\x1b[A\r"),
            },
            ClientMsg::PtyResize {
                session_id: 1,
                cols: 120,
                rows: 40,
            },
            ClientMsg::InterruptSession { session_id: 1 },
            ClientMsg::KillSession { session_id: 1 },
            ClientMsg::CreateBucket {
                name: "work".into(),
                allowed_worker_ids: vec![3],
                default_worker_id: 3,
                is_default: true,
            },
            ClientMsg::DeleteBucket { id: 2 },
            ClientMsg::CreateProject {
                bucket_id: 2,
                name: "api".into(),
                path: "/tmp/api".into(),
                worker_id: Some(3),
                allowed_worker_ids: vec![3],
            },
            ClientMsg::UpdateProject {
                project_id: 4,
                path: Some("/tmp/api-v2".into()),
                permission_mode: Some(PermissionMode::Auto),
                worker_id: Some(Some(7)),
            },
            ClientMsg::UpdateProject {
                project_id: 4,
                path: None,
                permission_mode: Some(PermissionMode::Inherit),
                worker_id: Some(None),
            },
            ClientMsg::DeleteProject { id: 4 },
            ClientMsg::SetProjectWorkerPath {
                project_id: 4,
                worker: "laptop".into(),
                path: Some("/laptop/api".into()),
            },
            ClientMsg::SetProjectWorkerPath {
                project_id: 4,
                worker: "5".into(),
                path: None,
            },
            ClientMsg::HookEvent {
                session_token: "tok".into(),
                kind: HookKind::NeedsInput,
                detail: "permission prompt".into(),
                agent_session_id: String::new(),
                transcript_path: String::new(),
                background_work: false,
            },
            ClientMsg::HookEvent {
                session_token: "tok".into(),
                kind: HookKind::TurnEnded,
                detail: String::new(),
                agent_session_id: "abc-123".into(),
                transcript_path: "/p/abc-123.jsonl".into(),
                background_work: true,
            },
            ClientMsg::HookEvent {
                session_token: "tok".into(),
                kind: HookKind::TurnFailed,
                detail: "rate_limit".into(),
                agent_session_id: "abc-123".into(),
                transcript_path: "/p/abc-123.jsonl".into(),
                background_work: false,
            },
            ClientMsg::HookEvent {
                session_token: "tok".into(),
                kind: HookKind::PromptSubmitted,
                detail: String::new(),
                agent_session_id: String::new(),
                transcript_path: String::new(),
                background_work: false,
            },
            ClientMsg::HookEvent {
                session_token: "tok".into(),
                kind: HookKind::Started,
                detail: String::new(),
                agent_session_id: "abc".into(),
                transcript_path: "/p/abc.jsonl".into(),
                background_work: false,
            },
            ClientMsg::ResumeSession { session_id: 9 },
            ClientMsg::SetBucketPermissionMode {
                bucket_id: 2,
                mode: PermissionMode::Default,
            },
            ClientMsg::SetProjectPermissionMode {
                project_id: 4,
                mode: PermissionMode::Inherit,
            },
            ClientMsg::SetBucketDefaultAgent {
                bucket_id: 2,
                agent: Some(AgentKind::Codex),
            },
            ClientMsg::SetProjectDefaultAgent {
                project_id: 4,
                agent: None,
            },
            ClientMsg::ListWorkspaces,
            ClientMsg::SessionForwardInventory {
                session_token: "tok-307".into(),
            },
            ClientMsg::CloseForward { forward_id: 11 },
            ClientMsg::UpsertItem(Box::new(ItemWrite {
                bucket_id: 1,
                id: None,
                external_key: Some("jira:ABC-42".into()),
                title: Some("Ship the fix".into()),
                body: None,
                question: Some("Which release should include this?".into()),
                status: Some(ItemStatus::Planned),
                priority: None,
                source_kind: Some(ItemSourceKind::Jira),
                source_detail: Some("ABC board".into()),
                url: None,
                project_id: Some(3),
                clear_project: false,
                due_at_unix_ms: None,
                clear_due: true,
                blocked_by: Some(vec![19, 21]),
                note: Some("moved to this sprint".into()),
                link_session_id: None,
            })),
            ClientMsg::UpsertItem(Box::new(ItemWrite {
                bucket_id: 1,
                id: Some(21),
                blocked_by: None,
                link_session_id: Some(7),
                ..ItemWrite::default()
            })),
            ClientMsg::DeleteItem {
                bucket_id: 1,
                id: 21,
            },
            ClientMsg::SnoozeItem {
                bucket_id: 1,
                id: 21,
                until_unix_ms: Some(1_700_000_900_000),
            },
            ClientMsg::SnoozeItem {
                bucket_id: 1,
                id: 21,
                until_unix_ms: None,
            },
            ClientMsg::ListItems(ItemQuery {
                bucket_id: 1,
                statuses: vec![ItemStatus::Done, ItemStatus::Dropped],
                search: Some("ship".into()),
                project_id: None,
                priorities: vec![ItemPriority::High],
                source_kinds: vec![ItemSourceKind::Github],
                updated_since_unix_ms: Some(1_700_000_000_000),
                include_closed: true,
                include_snoozed: true,
                limit: Some(25),
                offset: 50,
                summary_filter: Some(ItemSummaryFilter::LiveLinked),
            }),
            ClientMsg::ItemNotes {
                bucket_id: 1,
                item_id: 21,
            },
            ClientMsg::SetSetting {
                key: "spawn.truecolor".into(),
                value: Some("false".into()),
            },
            ClientMsg::SetSetting {
                key: "spawn.truecolor".into(),
                value: None,
            },
            ClientMsg::ListSettings,
            ClientMsg::GetSession { session_id: 99 },
            ClientMsg::ListEndedSessions {
                cursor: "1700:99".into(),
                limit: 50,
            },
            ClientMsg::SearchSessions {
                query: "needle / path".into(),
                cursor: "2:1700:99".into(),
                limit: 50,
            },
            ClientMsg::MarkSessionSeen { session_id: 99 },
            ClientMsg::UpdateSessionApis {
                session_id: 9,
                items_api: None,
                supervisor_api: Some(true),
                role: Some(SessionRole::Supervisor),
            },
            ClientMsg::RespondToItem {
                bucket_id: 1,
                item_id: 21,
                text: "Use the current release.".into(),
                target: RespondTarget::Session(9),
            },
            ClientMsg::RespondToItem {
                bucket_id: 1,
                item_id: 21,
                text: "Start a new supervisor.".into(),
                target: RespondTarget::NewSupervisor { project_id: 3 },
            },
            ClientMsg::RespondToItem {
                bucket_id: 1,
                item_id: 21,
                text: "Recorded for later.".into(),
                target: RespondTarget::ReplyOnly,
            },
            // A comment written on a rendered page names the snapshot
            // it was rendered from; one from an agent or an older
            // client names none, and both have to survive the wire.
            ClientMsg::AddReviewComment {
                review_id: 3,
                path: "src/lib.rs".into(),
                line: 4,
                side: ReviewSide::Right,
                excerpt: "delta".into(),
                body: "rename delta".into(),
                send: true,
                anchor_snapshot_id: Some(12),
                choice: None,
            },
            ClientMsg::AddReviewComment {
                review_id: 3,
                path: "src/lib.rs".into(),
                line: 4,
                side: ReviewSide::Right,
                excerpt: "delta".into(),
                body: "rename delta".into(),
                send: false,
                anchor_snapshot_id: None,
                choice: None,
            },
            // An answer to a marked option list travels as fields, so
            // every one of them has to survive the wire.
            ClientMsg::AddReviewComment {
                review_id: 3,
                path: "plan.md".into(),
                line: 12,
                side: ReviewSide::Right,
                excerpt: "<!-- pm-choice id=auth select=one -->".into(),
                body: "auth: Server-side sessions".into(),
                send: true,
                anchor_snapshot_id: Some(12),
                choice: Some(Box::new(ReviewChoiceAnswer {
                    choice_id: "auth".into(),
                    select: ReviewChoiceSelect::One,
                    option_ids: vec!["server-side-sessions".into()],
                    option_labels: vec!["Server-side sessions".into()],
                    other_text: String::new(),
                    notes: "sticky routing is fine".into(),
                })),
            },
            ClientMsg::EditReviewComment {
                message_id: 7,
                body: "auth: Other".into(),
                choice: Some(Box::new(ReviewChoiceAnswer {
                    choice_id: "auth".into(),
                    select: ReviewChoiceSelect::Many,
                    option_ids: vec!["other".into()],
                    option_labels: vec!["Other".into()],
                    other_text: "mTLS between services".into(),
                    notes: String::new(),
                })),
            },
        ];
        for (i, msg) in msgs.into_iter().enumerate() {
            let env = ClientEnvelope { seq: i as u64, msg };
            let decoded = ClientEnvelope::decode(&env.encode_to_vec()).unwrap();
            assert_eq!(env, decoded);
        }
    }

    #[test]
    fn server_messages_round_trip() {
        let msgs = vec![
            ServerMsg::Snapshot(Snapshot {
                reviews: Vec::new(),
                review_viewer_states: Vec::new(),
                plans: Vec::new(),
                buckets: vec![Bucket {
                    id: 1,
                    name: "work".into(),
                    position: 0,
                    permission_mode: PermissionMode::Bypass,
                    default_agent: Some(AgentKind::ClaudeCode),
                    model_profile_id: Some(11),
                    default_worker_id: 0,
                    allowed_worker_ids: vec![0, 5],
                    is_default: true,
                }],
                projects: vec![Project {
                    id: 3,
                    bucket_id: 1,
                    name: "api".into(),
                    path: "/tmp/api".into(),
                    permission_mode: PermissionMode::Inherit,
                    default_agent: Some(AgentKind::Codex),
                    model_profile_id: None,
                    worker_id: Some(5),
                    allowed_worker_ids: vec![5],
                    worker_paths: vec![ProjectPath {
                        worker_id: 5,
                        path: "/remote/api".into(),
                    }],
                }],
                sessions: vec![sample_session()],
                workers: vec![Worker {
                    id: 5,
                    name: "laptop".into(),
                    hostname: "host.local".into(),
                    platform: "linux".into(),
                    online: true,
                    default_project_root: "/home/dev".into(),
                    pm_version: "0.1.0+abc1234".into(),
                    runtime: String::new(),
                    container: String::new(),
                    last_seen_at_unix_ms: Some(1_700_000_000_000),
                    connect_mode: ConnectMode::Accept,
                    endpoint: "laptop.internal:7678".into(),
                }],
                terminals: Vec::new(),
                forwards: vec![sample_forward()],
                contexts: vec![SessionContext {
                    session_id: 7,
                    glance: vec![ContextField {
                        key: "tests".into(),
                        label: "tests".into(),
                        value: "142 passing".into(),
                        kind: ContextKind::Badge,
                        severity: ContextSeverity::Good,
                    }],
                    detail: vec![ContextField {
                        key: "branch".into(),
                        label: "branch".into(),
                        value: "feat/jwt".into(),
                        kind: ContextKind::Code,
                        severity: ContextSeverity::Neutral,
                    }],
                }],
                items: vec![sample_item()],
                briefings: vec![BucketBriefing {
                    id: 2,
                    bucket_id: 1,
                    session_id: Some(7),
                    ts_unix_ms: 1_700_000_200_000,
                    markdown: "## Needs you\n- [PR 412](pm:item/3/21)".into(),
                }],
                user_settings: vec![UserSetting {
                    key: "terminal.theme".into(),
                    value_json: r#"{"kind":"puppet-master-terminal-theme"}"#.into(),
                }],
                instruction_layers: vec![],
                model_profiles: vec![sample_model_profile()],
                agent_dialects: vec![AgentDialects {
                    agent: AgentKind::Codex,
                    dialects: vec![ModelDialect::OpenaiResponses],
                    supports_background_model: false,
                }],
            }),
            ServerMsg::Event(Event::ModelProfileChanged(sample_model_profile())),
            ServerMsg::Event(Event::ModelProfileRemoved(11)),
            ServerMsg::Event(Event::ItemChanged(sample_item())),
            ServerMsg::Event(Event::ItemRemoved(ItemRef {
                bucket_id: 1,
                item_id: 21,
            })),
            ServerMsg::Event(Event::BriefingChanged(BucketBriefing {
                id: 3,
                bucket_id: 1,
                session_id: None,
                ts_unix_ms: 1_700_000_300_000,
                markdown: "quiet day".into(),
            })),
            ServerMsg::Event(Event::UserSettingChanged(UserSettingChanged {
                key: "terminal.theme".into(),
                value_json: None,
            })),
            ServerMsg::Event(Event::SessionChanged(sample_session())),
            ServerMsg::Event(Event::SessionChanged(Session {
                state: SessionState::AwaitingWorker,
                state_detail: "awaiting worker to reconnect".into(),
                ..sample_session()
            })),
            ServerMsg::Event(Event::ContextChanged(SessionContext {
                session_id: 7,
                glance: Vec::new(),
                detail: Vec::new(),
            })),
            ServerMsg::Event(Event::SessionRemoved(7)),
            ServerMsg::Event(Event::ForwardChanged(sample_forward())),
            ServerMsg::Event(Event::ForwardRemoved(11)),
            ServerMsg::Event(Event::WorkerChanged(Worker {
                id: 5,
                name: "laptop".into(),
                hostname: "host.local".into(),
                platform: "linux".into(),
                online: false,
                default_project_root: String::new(),
                last_seen_at_unix_ms: None,
                pm_version: String::new(),
                runtime: String::new(),
                container: String::new(),
                connect_mode: ConnectMode::Dial,
                endpoint: String::new(),
            })),
            ServerMsg::Event(Event::WorkerRemoved(5)),
            ServerMsg::PtyOutput {
                session_id: 7,
                terminal_id: 9,
                generation: 2,
                data: bytes::Bytes::from_static(b"\x1b[2Jhello"),
                replay: true,
            },
            ServerMsg::CommandResult {
                seq: 5,
                result: Ok(Some(12)),
                data: bytes::Bytes::new(),
            },
            ServerMsg::CommandResult {
                seq: 6,
                result: Err("no such project".into()),
                data: bytes::Bytes::new(),
            },
            ServerMsg::CommandResult {
                seq: 7,
                result: Ok(None),
                data: bytes::Bytes::from_static(b"[{\"id\":1}]"),
            },
        ];
        for msg in msgs {
            let decoded = ServerMsg::decode(&msg.encode_to_vec()).unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn worker_plane_messages_round_trip() {
        let up = vec![
            WorkerMsg::HarnessStatus {
                req_id: 42,
                status: HarnessStatus {
                    state: "failed".into(),
                    command: "installer".into(),
                    output: "permission denied".into(),
                    error: "exit status 1".into(),
                },
            },
            WorkerMsg::Register {
                protocol_version: crate::WORKER_PROTOCOL_VERSION,
                enrollment_token: "enr".into(),
                credential: String::new(),
                hostname: "host".into(),
                platform: "linux".into(),
                pm_version: "0.1.0+abc1234".into(),
                runtime: String::new(),
                container: String::new(),
                default_project_root: "/home/dev".into(),
                live_sessions: vec![3, 9],
                live_terminals: vec![crate::domain::WorkerTerminal {
                    terminal_id: 12,
                    generation: 4,
                    kind: TerminalKind::Shell,
                    state: TerminalRunState::Running,
                    agent_resumable: false,
                    transcript_available: true,
                }],
                pending_transcripts: vec![crate::domain::WorkerTranscript {
                    terminal_id: 7,
                    generation: 2,
                    size: 128,
                }],
                live_dir_shares: vec![crate::domain::WorkerDirShare {
                    share_id: 5,
                    port: 41_001,
                }],
            },
            WorkerMsg::Heartbeat,
            WorkerMsg::SessionState {
                session_id: 3,
                state: SessionState::NeedsInput,
                detail: "pick one".into(),
            },
            WorkerMsg::SessionExit {
                session_id: 3,
                exit_code: Some(0),
            },
            WorkerMsg::TerminalExit {
                terminal_id: 8,
                generation: 3,
                exit_code: Some(7),
                state: TerminalRunState::Exited,
                transcript_available: true,
                transcript_size: 8,
                detail: String::new(),
            },
            WorkerMsg::TerminalExit {
                terminal_id: 9,
                generation: 1,
                exit_code: None,
                state: TerminalRunState::Failed,
                transcript_available: false,
                transcript_size: 0,
                detail: "agent cannot reach its puppet-master tools".into(),
            },
            WorkerMsg::NeedsInput { session_id: 4 },
            WorkerMsg::DirShareBound {
                share_id: 5,
                ok: true,
                error: String::new(),
                port: 41_001,
            },
            WorkerMsg::DirShareBound {
                share_id: 6,
                ok: false,
                error: "no such directory".into(),
                port: 0,
            },
            WorkerMsg::TerminalActivity {
                terminal_id: 8,
                generation: 3,
            },
            WorkerMsg::ProgramStatus {
                terminal_id: 8,
                generation: 3,
                reset: true,
                records: vec![
                    ProgramStatusRecord {
                        id: String::new(),
                        state: ProgramStatusState::Blocked,
                        kind: Some(ProgramStatusKind::Permission),
                        progress: Some(40),
                        app: "claude-code".into(),
                        title: String::new(),
                        msg: "Allow edit?".into(),
                        updated_at_unix_ms: 1_700_000_000_000,
                    },
                    ProgramStatusRecord {
                        id: "task/1".into(),
                        state: ProgramStatusState::Done,
                        kind: None,
                        progress: None,
                        app: String::new(),
                        title: "Explore".into(),
                        msg: String::new(),
                        updated_at_unix_ms: 1_700_000_000_001,
                    },
                ],
                removed: vec!["task/2".into()],
            },
            WorkerMsg::HookReport {
                session_token: "tok".into(),
                kind: HookKind::TurnFailed,
                detail: "overloaded".into(),
                agent_session_id: "abc".into(),
                transcript_path: "/p/abc.jsonl".into(),
                req_id: 11,
                background_work: false,
            },
            WorkerMsg::HookReport {
                session_token: "tok".into(),
                kind: HookKind::TurnEnded,
                detail: String::new(),
                agent_session_id: "abc".into(),
                transcript_path: "/p/abc.jsonl".into(),
                req_id: 12,
                background_work: true,
            },
            WorkerMsg::ForwardOpened {
                req_id: 6,
                ok: false,
                error: "connection refused".into(),
            },
            WorkerMsg::PathChecked {
                req_id: 4,
                status: PathCheck::Unreadable,
                detail: "Permission denied (os error 13)".into(),
            },
            WorkerMsg::AgentInboxResult {
                req_id: 5,
                outcome: AgentInboxOutcome::Delivered,
                transport: "claude-socket".into(),
                mode: AgentInboxMode::Queue,
                detail: String::new(),
            },
            WorkerMsg::AgentInboxResult {
                req_id: 6,
                outcome: AgentInboxOutcome::NoChannel,
                transport: String::new(),
                mode: AgentInboxMode::Steer,
                detail: "the session has no agent process yet".into(),
            },
        ];
        for msg in up {
            assert_eq!(msg, WorkerMsg::decode(&msg.encode_to_vec()).unwrap());
        }

        let down = vec![
            ControllerMsg::AgentInbox {
                req_id: 9,
                session_id: 77,
                agent_terminal_id: 104,
                agent: AgentKind::ClaudeCode,
                agent_session_id: "a-b-c".into(),
                agent_port: Some(4567),
                text: "two of your sessions are parked".into(),
                mode: AgentInboxMode::Steer,
            },
            ControllerMsg::HarnessRequest {
                req_id: 42,
                agent: AgentKind::ClaudeCode,
                install: true,
            },
            ControllerMsg::Registered {
                worker_id: 5,
                credential: "cred".into(),
                error: String::new(),
                mcp_base_url: "https://controller.example".into(),
                http_port: 7676,
                pm_version: "0.2.0+abc1234".into(),
            },
            ControllerMsg::UpdateNow,
            ControllerMsg::HookResult {
                req_id: 11,
                nudge: "set a headline".into(),
            },
            ControllerMsg::Spawn {
                session_id: 3,
                terminal_id: 9,
                generation: 2,
                agent: AgentKind::ClaudeCode,
                task_prompt: "fix it".into(),
                permission_mode: PermissionMode::Bypass,
                cwd: "/srv/api".into(),
                session_token: "tok".into(),
                resume_agent_session_id: String::new(),
                truecolor: false,
                fullscreen: false,
                compiled_instructions: "private".into(),
                model_endpoint: Some(Box::new(ResolvedModelEndpoint {
                    dialect: ModelDialect::AnthropicMessages,
                    model: "gw/big".into(),
                    base_url: "https://gw.example/v1".into(),
                    background_model: "gw/small".into(),
                    api_key: "sk-secret".into(),
                    provider_name: "Gateway".into(),
                })),
                initial_cols: Some(120),
                initial_rows: Some(32),
                program_status: true,
            },
            ControllerMsg::TerminalAttach {
                terminal_id: 9,
                generation: 2,
                token: "terminal-token".into(),
                replay_bytes: 256 * 1024,
                size: (132, 41),
            },
            ControllerMsg::TerminalDetach {
                terminal_id: 9,
                generation: 2,
            },
            ControllerMsg::DirShareServe {
                share_id: 5,
                root: "/srv/api".into(),
                path: "out/report".into(),
            },
            ControllerMsg::DirShareStop { share_id: 5 },
            ControllerMsg::Interrupt { session_id: 3 },
            ControllerMsg::Kill { session_id: 3 },
            ControllerMsg::ForwardOpen {
                req_id: 6,
                port: 5173,
                token: "stream-token".into(),
            },
            ControllerMsg::PathCheck {
                req_id: 4,
                path: "/srv/acme".into(),
            },
        ];
        for msg in down {
            assert_eq!(msg, ControllerMsg::decode(&msg.encode_to_vec()).unwrap());
        }
    }

    #[test]
    fn every_path_check_verdict_round_trips_through_the_wire_and_its_name() {
        for status in [
            PathCheck::Ok,
            PathCheck::Missing,
            PathCheck::NotADirectory,
            PathCheck::Unreadable,
        ] {
            let encoded = wire::PathCheck::from(status) as i32;
            assert_eq!(PathCheck::try_from(encoded).unwrap(), status);
            assert!(!status.as_str().is_empty());
        }
        assert!(matches!(
            PathCheck::try_from(wire::PathCheck::Unspecified as i32),
            Err(DecodeError::UnknownEnum("PathCheck", _))
        ));
    }

    #[test]
    fn out_of_range_forward_port_is_an_error() {
        let mut raw: wire::SessionForward = sample_forward().into();
        raw.worker_port = 70_000;
        let err = SessionForward::try_from(raw).unwrap_err();
        assert!(matches!(err, DecodeError::OutOfRange("worker_port")));
    }

    #[test]
    fn missing_oneof_is_an_error_not_a_guess() {
        let raw = wire::ClientMessage { seq: 1, msg: None };
        let buf = prost::Message::encode_to_vec(&raw);
        assert!(ClientEnvelope::decode(&buf).is_err());
    }

    #[test]
    fn unknown_enum_value_is_an_error() {
        let mut raw: wire::Session = sample_session().into();
        raw.state = 999;
        let err = Session::try_from(raw).unwrap_err();
        assert!(matches!(err, DecodeError::UnknownEnum("SessionState", 999)));
    }

    #[test]
    fn item_status_must_be_specified_on_items_but_not_writes() {
        let mut raw: wire::Item = sample_item().into();
        raw.status = 0;
        assert!(matches!(
            Item::try_from(raw).unwrap_err(),
            DecodeError::UnknownEnum("ItemStatus", 0)
        ));

        // On a write, UNSPECIFIED means "leave the stored value".
        let raw = wire::ClientMessage {
            seq: 1,
            msg: Some(wire::client_message::Msg::UpsertItem(wire::UpsertItem {
                bucket_id: 1,
                ..Default::default()
            })),
        };
        let env = ClientEnvelope::decode(&prost::Message::encode_to_vec(&raw)).unwrap();
        let ClientMsg::UpsertItem(w) = env.msg else {
            panic!("expected UpsertItem");
        };
        assert_eq!(w.status, None);
        assert_eq!(w.priority, None);
        assert_eq!(w.blocked_by, None);
    }

    #[test]
    fn spawn_without_items_api_field_defaults_to_enabled() {
        let raw = wire::ClientMessage {
            seq: 1,
            msg: Some(wire::client_message::Msg::SpawnSession(
                wire::SpawnSession {
                    project_id: 3,
                    agent: wire::AgentKind::ClaudeCode as i32,
                    ..Default::default()
                },
            )),
        };
        let env = ClientEnvelope::decode(&prost::Message::encode_to_vec(&raw)).unwrap();
        assert!(matches!(
            env.msg,
            ClientMsg::SpawnSession {
                items_api: true,
                supervisor_api: false,
                ..
            }
        ));
    }

    #[test]
    fn spawn_without_agent_decodes_as_inherited_selection() {
        let raw = wire::ClientMessage {
            seq: 1,
            msg: Some(wire::client_message::Msg::SpawnSession(
                wire::SpawnSession {
                    project_id: 3,
                    agent: wire::AgentKind::Unspecified as i32,
                    ..Default::default()
                },
            )),
        };
        let env = ClientEnvelope::decode(&prost::Message::encode_to_vec(&raw)).unwrap();
        assert!(matches!(
            env.msg,
            ClientMsg::SpawnSession { agent: None, .. }
        ));
    }

    /// A controller that predates the renderer setting sends no
    /// `fullscreen`, and it meant the alternate screen it always got.
    #[test]
    fn a_spawn_without_the_renderer_field_keeps_the_alternate_screen() {
        let decoded = ControllerMsg::try_from(wire::ControllerMessage {
            msg: Some(wire::controller_message::Msg::Spawn(
                wire::ControllerSpawn {
                    session_id: 3,
                    agent: wire::AgentKind::ClaudeCode as i32,
                    cwd: "/srv/api".into(),
                    terminal_id: 9,
                    generation: 2,
                    fullscreen: None,
                    ..Default::default()
                },
            )),
        })
        .unwrap();
        assert!(matches!(
            decoded,
            ControllerMsg::Spawn {
                fullscreen: true,
                ..
            }
        ));
    }

    /// A controller that predates Program Status sends no flag, and its
    /// agents must keep a terminal that neither answers nor strips OSC 7501.
    #[test]
    fn a_spawn_without_the_program_status_field_leaves_it_off() {
        let decoded = ControllerMsg::try_from(wire::ControllerMessage {
            msg: Some(wire::controller_message::Msg::Spawn(
                wire::ControllerSpawn {
                    session_id: 3,
                    agent: wire::AgentKind::ClaudeCode as i32,
                    terminal_id: 9,
                    generation: 2,
                    ..Default::default()
                },
            )),
        })
        .unwrap();
        assert!(matches!(
            decoded,
            ControllerMsg::Spawn {
                program_status: false,
                ..
            }
        ));
    }

    #[test]
    fn a_program_status_record_with_an_unknown_state_is_dropped_not_fatal() {
        const FUTURE_STATE: i32 = 99;
        let records = program_status_records(vec![
            wire::ProgramStatusRecord {
                id: "a".into(),
                state: FUTURE_STATE,
                ..Default::default()
            },
            wire::ProgramStatusRecord {
                id: "b".into(),
                state: wire::ProgramStatusState::Working as i32,
                kind: FUTURE_STATE,
                ..Default::default()
            },
        ]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "b");
        assert_eq!(records[0].kind, None);
    }

    #[test]
    fn legacy_supervisor_flag_maps_to_first_class_role_compatibly() {
        let decoded = ClientEnvelope::try_from(wire::ClientMessage {
            seq: 1,
            msg: Some(wire::client_message::Msg::SpawnSession(
                wire::SpawnSession {
                    project_id: 1,
                    agent: wire::AgentKind::Codex as i32,
                    task_title: String::new(),
                    task_prompt: String::new(),
                    cwd: String::new(),
                    permission_mode: 0,
                    worker_id: None,
                    items_api: None,
                    supervisor_api: Some(true),
                    role: None,
                    model_profile_id: None,
                    host: String::new(),
                    initial_cols: None,
                    initial_rows: None,
                },
            )),
        })
        .unwrap();
        assert!(matches!(
            decoded.msg,
            ClientMsg::SpawnSession {
                items_api: true,
                supervisor_api: true,
                ..
            }
        ));
    }
}

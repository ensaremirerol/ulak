// Hand-written mirror of crates/ulak-proto (dto.rs, wire.rs, version.rs). The JSON contract is
// pinned by test/fixtures/wire.json, which a Rust test generates from the real types.

export type NodeId = string;
export type AgentId = string;
export type ThreadId = string;
export type MessageId = string;
export type QuestionId = string;
export type BlockerId = string;
export type ProfileKey = string;
export type CredentialId = string;
export type QueueId = string;
/** `owner/name` on github.com. */
export type RepoRef = string;

export interface Telemetry {
  load1: number;
  cpu_pct: number;
  gpu_pct: number | null;
  mem_used_pct: number;
  at_ms: number;
}

/** serde's default (externally tagged) enum encoding. */
export type BusyReason = { Gaming: { process: string } } | 'Load';
export type NodeState = 'Idle' | 'Offline' | { Busy: BusyReason };

export interface NodeInfo {
  id: NodeId;
  hostname: string;
  state: NodeState;
  telemetry: Telemetry;
  agents: AgentId[];
}

export type AgentStatus =
  | 'Starting'
  | 'Running'
  | 'Failed'
  | 'Installing'
  | 'NeedsLogin'
  | { Exited: number | null };

export interface AgentProfile {
  binary: string;
  args: string[];
  config_dir: string | null;
}

/** Where an agent's files come from. */
export type Source =
  | { kind: 'folder'; path: string }
  | { kind: 'repo'; repo: RepoRef; branch: string | null; credential: CredentialId; allow_push: boolean };

export interface SpawnSpec {
  agent_id: AgentId;
  profile: AgentProfile;
  /** Folder spawns: the path. Repo spawns: '' (the daemon chooses). */
  workdir: string;
  prompt: string;
  env: Record<string, string>;
  /** Absent means a folder spawn at `workdir`. */
  source?: Source;
}

export type SpawnTarget = 'Auto' | { Node: NodeId };

export type MessageStatus = 'Pending' | 'Delivered' | 'Read';

export interface Message {
  id: MessageId;
  from: AgentId;
  to: AgentId;
  thread: ThreadId;
  payload: string;
  status: MessageStatus;
  at_ms: number;
}

export interface Question {
  id: QuestionId;
  from: AgentId;
  text: string;
  at_ms: number;
}

export interface Answer {
  question: QuestionId;
  text: string;
}

export type BlockerKind = 'Login' | 'Install';

export interface Blocker {
  id: BlockerId;
  node: NodeId;
  profile: ProfileKey;
  kind: BlockerKind;
  agent: AgentId | null;
  detail: string;
  at_ms: number;
}

export type DeviceKind = 'node' | 'ui' | 'browser';

export interface DeviceInfo {
  id: NodeId;
  kind: DeviceKind;
  hostname: string;
  created_ms: number;
  revoked: boolean;
}

export interface VersionInfo {
  version: string;
  git: string;
  proto: number;
  os: string;
  arch: string;
}

export type UpdateState =
  | { state: 'up_to_date'; checked_ms: number }
  | { state: 'available'; version: string }
  | { state: 'checking' | 'downloading' | 'verifying' | 'installing' | 'restarting' }
  | { state: 'failed'; message: string };

/** A stored GitHub credential. The token is never sent back to the UI. */
export interface CredentialInfo {
  id: CredentialId;
  name: string;
  kind: 'github';
  login: string;
  created_ms: number;
}

export interface RepoInfo {
  full_name: RepoRef;
  private: boolean;
  default_branch: string;
  /** 0 when unknown. */
  pushed_at_ms: number;
  description?: string;
}

export type QueueState = 'Queued' | { Failed: string };

export interface QueuedSpawn {
  id: QueueId;
  agent: AgentId;
  source: Source;
  target: SpawnTarget;
  profile: ProfileKey;
  prompt_preview: string;
  created_ms: number;
  state: QueueState;
}

/** What the hub tells a one-time-code holder (device enrolment). */
export interface OtpOffer {
  kind: DeviceKind;
  otp: string;
  hub_fingerprint: string;
  expires_ms: number;
}

// ---- frames. `data` is raw bytes here; on the wire it is standard base64. ----

export type UiToHub =
  | { type: 'spawn'; target: SpawnTarget; spec: SpawnSpec }
  | ({ type: 'answer' } & Answer)
  | { type: 'attach'; agent: AgentId }
  | { type: 'detach'; agent: AgentId }
  | { type: 'pane_input'; agent: AgentId; data: Uint8Array }
  | { type: 'kill'; agent: AgentId }
  | { type: 'add_device'; kind: DeviceKind }
  | { type: 'revoke_device'; id: NodeId }
  | { type: 'list_devices' }
  | { type: 'resolve_blocker'; id: BlockerId }
  | { type: 'check_update'; node: NodeId | null }
  | { type: 'update_node'; node: NodeId | null }
  | { type: 'mailbox_log'; thread: ThreadId | null; limit: number }
  | { type: 'add_credential'; name: string; token: string }
  | { type: 'delete_credential'; id: CredentialId }
  | { type: 'list_credentials' }
  | { type: 'list_repos'; credential: CredentialId; query: string | null; page: number }
  | { type: 'list_branches'; credential: CredentialId; repo: RepoRef }
  | { type: 'cancel_queued'; id: QueueId }
  | { type: 'list_queue' };

export type HubToUi =
  | { type: 'snapshot'; nodes: NodeInfo[]; questions: Question[] }
  | { type: 'blockers'; blockers: Blocker[] }
  | ({ type: 'blocker_opened' } & Blocker)
  | { type: 'blocker_closed'; id: BlockerId }
  | ({ type: 'node_update' } & NodeInfo)
  | ({ type: 'question_opened' } & Question)
  | { type: 'question_closed'; id: QuestionId }
  | ({ type: 'message_logged' } & Message)
  | { type: 'pane_output'; agent: AgentId; data: Uint8Array }
  | { type: 'agent_status'; agent: AgentId; node: NodeId; status: AgentStatus }
  | ({ type: 'otp_offer' } & OtpOffer)
  | { type: 'devices'; devices: DeviceInfo[] }
  | { type: 'mailbox_log'; messages: Message[] }
  | ({ type: 'hub_version' } & VersionInfo)
  | { type: 'node_version'; node: NodeId; info: VersionInfo }
  | { type: 'update_status'; node: NodeId; status: UpdateState }
  | { type: 'agent_statuses'; statuses: [AgentId, NodeId, AgentStatus][] }
  | { type: 'agent_failed'; agent: AgentId; node: NodeId; reason: string }
  | { type: 'credentials'; credentials: CredentialInfo[] }
  | { type: 'repos'; credential: CredentialId; query: string | null; repos: RepoInfo[]; has_more: boolean }
  | { type: 'branches'; credential: CredentialId; repo: RepoRef; branches: string[]; default_branch: string }
  | { type: 'queue'; items: QueuedSpawn[] }
  | { type: 'queue_changed'; item: QueuedSpawn }
  | { type: 'queue_removed'; id: QueueId }
  | { type: 'error'; message: string };

/** A frame this build does not know (newer hub) or could not parse. Never fatal. */
export interface UnknownFrame {
  type: 'unknown';
  raw: unknown;
  reason: string;
}

export type HubToUiType = HubToUi['type'];
export type UiToHubType = UiToHub['type'];

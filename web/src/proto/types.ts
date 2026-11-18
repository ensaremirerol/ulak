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
  | {
      kind: 'repo';
      repo: RepoRef;
      branch: string | null;
      credential: CredentialId;
      allow_push: boolean;
      /** Always clone anew. On the wire only when true; absent means reuse a free clone. */
      fresh?: boolean;
    };

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

/** An agent profile as a node sees it. */
export interface ProfileInfo {
  name: ProfileKey;
  binary: string;
  config_dir: string | null;
  installed: boolean;
  login_blocked: boolean;
  /** From the hub, not the node's agents.json. On the wire only when true. */
  from_hub?: boolean;
  /** The node has an install command for it (`install_profile` works). On the wire only when true. */
  installable?: boolean;
}

/** A hub-defined profile. Optional fields are absent on the wire when empty. */
export interface HubProfile {
  name: string;
  binary: string;
  args?: string[];
  config_dir?: string;
  env?: Record<string, string>;
  /** Shell script run on the node as `sh -c` (1-4096 chars, no NUL) when the binary is missing or on `install_profile`. */
  install?: string;
}

/** `node: null` = every node; a node-scoped profile beats a global one of the same name. */
export interface ScopedProfile {
  node: NodeId | null;
  profile: HubProfile;
}

/** Hub-stored node settings; an absent field = the node's own file or default. */
export interface NodeSettings {
  repo_roots?: string[];
  index_interval_mins?: number;
  clean_auto?: boolean;
  clean_after_exit_hours?: number;
  auto_update?: boolean;
  cpu_busy_pct?: number;
  gpu_busy_pct?: number;
}

export interface NodeSettingsEntry {
  node: NodeId;
  settings: NodeSettings;
}

/** A git checkout on a node. */
export interface RepoEntry {
  full_name: RepoRef;
  path: string;
  branch: string | null;
  clean: boolean;
  ahead: number | null;
  behind: number | null;
  last_fetch_ms: number | null;
  in_use: boolean;
  managed: boolean;
}

/** One checkout a clean removed or kept, with why. */
export interface CleanItem {
  path: string;
  full_name: RepoRef | null;
  reason: string;
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
  | { type: 'list_queue' }
  | { type: 'save_profile'; node: NodeId | null; profile: HubProfile }
  | { type: 'delete_profile'; node: NodeId | null; name: string }
  | { type: 'save_node_settings'; node: NodeId; settings: NodeSettings }
  | { type: 'node_stats'; node: NodeId }
  /** `node: null` = every connected node reporting the profile installable. */
  | { type: 'install_profile'; node: NodeId | null; name: string }
  | { type: 'clean_node'; node: NodeId; dry_run: boolean }
  | { type: 'clean_repo'; node: NodeId; path: string };

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
  | { type: 'release_cache'; latest: string | null; checked_ms: number; error: string | null }
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
  | { type: 'node_profiles'; node: NodeId; profiles: ProfileInfo[] }
  | { type: 'node_repos'; node: NodeId; repos: RepoEntry[] }
  | { type: 'hub_config'; profiles: ScopedProfile[]; settings: NodeSettingsEntry[] }
  | { type: 'node_stats'; node: NodeId; connected_ms: number | null; samples: Telemetry[] }
  /** `done: false` = started; `done: true` with `ok` and `detail` (error, or output tail) = finished. */
  | { type: 'install_status'; node: NodeId; name: string; done: boolean; ok: boolean; detail: string }
  | { type: 'clean_report'; node: NodeId; dry_run: boolean; removed: CleanItem[]; kept: CleanItem[] }
  | { type: 'error'; message: string };

/** A frame this build does not know (newer hub) or could not parse. Never fatal. */
export interface UnknownFrame {
  type: 'unknown';
  raw: unknown;
  reason: string;
}

export type HubToUiType = HubToUi['type'];
export type UiToHubType = UiToHub['type'];

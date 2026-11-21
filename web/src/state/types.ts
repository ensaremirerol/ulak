import type {
  AgentId, AgentStatus, Blocker, DeviceInfo, DeviceKind, HubToUi, Message, NodeId, NodeInfo,
  OtpOffer, Question, CredentialInfo, RepoInfo, QueuedSpawn, QuestionId, ThreadId, UpdateState, VersionInfo, BlockerId,
  ProfileInfo, RepoEntry, CleanItem, DirListing, ScopedProfile, NodeSettings, Telemetry,
} from '../proto';
import type { ConnState } from '../net';

/** Raw bytes of one agent's terminal, newest last, capped (see TERMINAL_CAP_BYTES). */
export interface TermBuf {
  chunks: Uint8Array[];
  bytes: number;
  /** Total chunks ever appended; changes on every append (use as a render key / change signal). */
  seq: number;
  /** The cap dropped old chunks: a byte stream can no longer be replayed from the start. */
  truncated?: boolean;
}

export type ToastKind = 'error' | 'info' | 'ok';
export interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
  /** Wall-clock ms after which a tick removes it; stamped by the first tick that sees the toast. */
  expires: number | null;
}

export interface State {
  conn: ConnState;
  nodes: NodeInfo[];
  questions: Question[];
  /** Open blockers, oldest first. */
  blockers: Blocker[];
  statuses: Record<AgentId, AgentStatus>;
  /** Last reported failure reason per agent. */
  lastFailure: Record<AgentId, { node: NodeId; reason: string; at: number }>;
  mailbox: Message[];
  devices: DeviceInfo[];
  otp: OtpOffer | null;
  terminals: Record<AgentId, TermBuf>;
  /** Folder listings by `node\0requested path` (the New agent folder browser). */
  dirListings: Record<string, DirListing>;
  /** The last size each agent's terminal was fitted to (cells); a stream (re)attach asks for it. */
  termSizes: Record<AgentId, { cols: number; rows: number }>;
  selected: AgentId | null;
  toasts: Toast[];
  nextToastId: number;
  now: number;
  hubVersion: VersionInfo | null;
  releaseCache: { latest: string | null; checkedMs: number; error: string | null } | null;
  nodeVersions: Record<NodeId, VersionInfo>;
  updates: Record<NodeId, UpdateState>;
  credentials: CredentialInfo[];
  /** Repo pages by `credential\0query\0`; the latest frame per key wins. */
  repos: Record<string, { repos: RepoInfo[]; hasMore: boolean }>;
  /** Branches by `credential\0repo`. */
  branches: Record<string, { branches: string[]; defaultBranch: string }>;
  queue: QueuedSpawn[];
  nodeProfiles: Record<NodeId, ProfileInfo[]>;
  nodeRepos: Record<NodeId, RepoEntry[]>;
  /** The latest clean report per node. */
  lastClean: Record<NodeId, { dryRun: boolean; removed: CleanItem[]; kept: CleanItem[] }>;
  /** Hub-defined profiles (global and node-scoped), replaced by every `hub_config`. */
  hubProfiles: ScopedProfile[];
  /** Hub-stored settings per node, replaced by every `hub_config`. */
  nodeSettings: Record<NodeId, NodeSettings>;
  /** Telemetry history of the node whose screen is open (oldest first, capped); null when none is. */
  stats: { node: NodeId; connectedMs: number | null; samples: Telemetry[] } | null;
  /** Counts of `hub_config` and `error` frames, plus the last error text: a form that sent a
   *  config change waits for one of them to move (see `useHubAck`). */
  acks: { config: number; error: number; lastError: string };
  /** The latest install per `node\0profile` (see `installKey`); `ok` only once done. */
  installs: Record<string, { running: boolean; ok?: boolean; detail: string; at: number }>;
}

export type Event =
  | { kind: 'hub'; frame: HubToUi }
  | { kind: 'conn'; state: ConnState }
  /** Wall clock in unix ms: question ages and toast expiry. */
  | { kind: 'tick'; now: number }
  | { kind: 'toast'; toast: ToastKind; text: string }
  | { kind: 'dismiss_toast'; id: number };

/** Raw fields of the spawn form. */
export interface SpawnForm {
  /** Empty = let the hub pick. */
  node: string;
  agentId: string;
  binary: string;
  /** Whitespace-separated. */
  args: string;
  workdir: string;
  prompt: string;
  /** Optional profile directory (CLAUDE_CONFIG_DIR style); empty = none. */
  configDir: string;
  /** Present = repo spawn (workdir is then sent as ''); absent = folder spawn at `workdir`. */
  repo?: { credential: string; repo: string; branch: string; allowPush: boolean; fresh?: boolean };
}

/** Raw fields of the hub profile dialog. */
export interface ProfileForm {
  name: string;
  binary: string;
  /** Whitespace-separated. */
  args: string;
  configDir: string;
  /** `KEY=value` lines (added to / overriding the kept env). */
  env: string;
  /** Install script; empty = none. */
  install: string;
}

export type Tri = 'inherit' | 'on' | 'off';

/** Raw fields of the node settings form; empty / 'inherit' = no hub value (the node decides). */
export interface SettingsForm {
  /** One folder per line. */
  repoRoots: string;
  indexIntervalMins: string;
  cleanAuto: Tri;
  cleanAfterExitHours: string;
  autoUpdate: Tri;
  cpuBusyPct: string;
  gpuBusyPct: string;
}

export type Intent =
  | { kind: 'select'; agent: AgentId }
  | { kind: 'detach' }
  | { kind: 'send_keys'; data: Uint8Array }
  | { kind: 'answer'; question: QuestionId; text: string }
  | { kind: 'spawn'; form: SpawnForm }
  /** The token only lives in the frame; it is never kept in state. */
  | { kind: 'add_credential'; name: string; token: string }
  | { kind: 'delete_credential'; id: string }
  | { kind: 'list_credentials' }
  | { kind: 'list_repos'; credential: string; query: string | null; page: number }
  | { kind: 'list_branches'; credential: string; repo: string }
  | { kind: 'cancel_queued'; id: string }
  | { kind: 'list_queue' }
  | { kind: 'kill'; agent: AgentId }
  /** The agent terminal's size in the viewer (cells); nodes from 0.1.10 resize the pane to it. */
  | { kind: 'resize'; agent: AgentId; cols: number; rows: number }
  | { kind: 'list_dir'; node: NodeId; path: string }
  /** Restart the agent's stream for a full repaint (the buffered bytes cannot be replayed). */
  | { kind: 'reattach'; agent: AgentId }
  | { kind: 'add_device'; device: DeviceKind }
  | { kind: 'revoke_device'; id: NodeId }
  | { kind: 'refresh_devices' }
  | { kind: 'refresh_mailbox'; thread?: ThreadId }
  | { kind: 'resolve_blocker'; id: BlockerId }
  /** `node` omitted = every connected node. */
  | { kind: 'check_update'; node?: NodeId }
  | { kind: 'update_node'; node?: NodeId }
  /** `keepEnv`: env of the profile being edited that the user kept (values never shown). */
  | { kind: 'save_profile'; node: NodeId | null; form: ProfileForm; keepEnv?: Record<string, string> }
  | { kind: 'delete_profile'; node: NodeId | null; name: string }
  | { kind: 'save_node_settings'; node: NodeId; form: SettingsForm }
  /** The node screen opened (asks for its history) / closed (stops collecting, unless another node took over). */
  | { kind: 'watch_node'; node: NodeId }
  | { kind: 'unwatch_node'; node: NodeId }
  /** `node` null = every connected node that can install it. */
  | { kind: 'install_profile'; node: NodeId | null; name: string }
  | { kind: 'clean_node'; node: NodeId; dryRun: boolean }
  | { kind: 'clean_repo'; node: NodeId; path: string };

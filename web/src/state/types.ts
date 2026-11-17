import type {
  AgentId, AgentStatus, Blocker, DeviceInfo, DeviceKind, HubToUi, Message, NodeId, NodeInfo,
  OtpOffer, Question, CredentialInfo, RepoInfo, QueuedSpawn, QuestionId, ThreadId, UpdateState, VersionInfo, BlockerId,
} from '../proto';
import type { ConnState } from '../net';

/** Raw bytes of one agent's terminal, newest last, capped (see TERMINAL_CAP_BYTES). */
export interface TermBuf {
  chunks: Uint8Array[];
  bytes: number;
  /** Total chunks ever appended; changes on every append (use as a render key / change signal). */
  seq: number;
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
  repo?: { credential: string; repo: string; branch: string; allowPush: boolean };
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
  | { kind: 'add_device'; device: DeviceKind }
  | { kind: 'revoke_device'; id: NodeId }
  | { kind: 'refresh_devices' }
  | { kind: 'refresh_mailbox'; thread?: ThreadId }
  | { kind: 'resolve_blocker'; id: BlockerId }
  /** `node` omitted = every connected node. */
  | { kind: 'check_update'; node?: NodeId }
  | { kind: 'update_node'; node?: NodeId };

import type {
  AgentId, AgentStatus, Blocker, DeviceInfo, DeviceKind, HubToUi, Message, NodeId, NodeInfo,
  OtpOffer, Question, QuestionId, ThreadId, UpdateState, VersionInfo, BlockerId,
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
  mailbox: Message[];
  devices: DeviceInfo[];
  otp: OtpOffer | null;
  terminals: Record<AgentId, TermBuf>;
  selected: AgentId | null;
  toasts: Toast[];
  nextToastId: number;
  now: number;
  hubVersion: VersionInfo | null;
  nodeVersions: Record<NodeId, VersionInfo>;
  updates: Record<NodeId, UpdateState>;
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
}

export type Intent =
  | { kind: 'select'; agent: AgentId }
  | { kind: 'detach' }
  | { kind: 'send_keys'; data: Uint8Array }
  | { kind: 'answer'; question: QuestionId; text: string }
  | { kind: 'spawn'; form: SpawnForm }
  | { kind: 'kill'; agent: AgentId }
  | { kind: 'add_device'; device: DeviceKind }
  | { kind: 'revoke_device'; id: NodeId }
  | { kind: 'refresh_devices' }
  | { kind: 'refresh_mailbox'; thread?: ThreadId }
  | { kind: 'resolve_blocker'; id: BlockerId }
  /** `node` omitted = every connected node. */
  | { kind: 'check_update'; node?: NodeId }
  | { kind: 'update_node'; node?: NodeId };

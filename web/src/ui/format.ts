import type { NodeState } from '../proto';
import type { Tone } from './components';

export function nodeStateLabel(s: NodeState): string {
  if (typeof s === 'string') return s;
  return s.Busy === 'Load' ? 'Busy: load' : `Gaming: ${s.Busy.Gaming.process}`;
}
export const nodeStateTone = (s: NodeState): Tone => (s === 'Idle' ? 'ok' : s === 'Offline' ? 'muted' : 'warn');

/** "just now", "5 min ago", "2 h ago", "3 d ago". */
export function ageLabel(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 10) return 'just now';
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}

export const clock = (ms: number): string => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
export const day = (ms: number): string => new Date(ms).toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' });

/** Same rule as the Rust AgentId. */
export const AGENT_ID = /^[a-z0-9-]{1,64}$/;

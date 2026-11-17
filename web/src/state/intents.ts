import type { UiToHub } from '../proto';
import { utf8 } from '../proto';
import { MAILBOX_LIMIT } from './reducer';
import type { Intent, SpawnForm, State } from './types';

/** A form or input the hub would reject; the message says what to fix. */
export class IntentError extends Error {}

const SLUG = /^[a-z0-9-]{1,64}$/;
const slug = (v: string, what: string): string => {
  if (!SLUG.test(v)) throw new IntentError(`${what} must be 1-64 characters: lowercase letters, digits and "-" (got "${v.slice(0, 40)}")`);
  return v;
};

export const defaultSpawnForm = (): SpawnForm => ({
  node: '', agentId: '', binary: 'claude', args: '', workdir: '', prompt: '', configDir: '',
});

export function spawnFormToFrame(f: SpawnForm): UiToHub {
  const node = f.node.trim();
  const binary = f.binary.trim();
  if (!binary) throw new IntentError('Program is empty: enter the agent binary, e.g. claude');
  if (!f.prompt.trim()) throw new IntentError('Prompt is empty: tell the agent what to do');
  const configDir = f.configDir.trim();
  return {
    type: 'spawn',
    target: node === '' ? 'Auto' : { Node: slug(node, 'Node') },
    spec: {
      agent_id: slug(f.agentId.trim(), 'Agent name'),
      profile: { binary, args: f.args.split(/\s+/).filter(Boolean), config_dir: configDir || null },
      workdir: f.workdir.trim(),
      prompt: f.prompt,
      env: {},
    },
  };
}

/** Frames for `intent` against `s`. Pure; throws IntentError for input the hub would refuse. */
export function intentToFrames(s: State, i: Intent): UiToHub[] {
  switch (i.kind) {
    case 'select':
      if (s.selected === i.agent) return [];
      return [...(s.selected ? [{ type: 'detach', agent: s.selected } as const] : []), { type: 'attach', agent: i.agent }];
    case 'detach':
      return s.selected ? [{ type: 'detach', agent: s.selected }] : [];
    case 'send_keys':
      return s.selected && i.data.length > 0 ? [{ type: 'pane_input', agent: s.selected, data: i.data }] : [];
    case 'answer':
      if (!i.text.trim()) throw new IntentError('The answer is empty: type something first');
      return [{ type: 'answer', question: i.question, text: i.text }];
    case 'spawn':
      return [spawnFormToFrame(i.form)];
    case 'kill':
      return [{ type: 'kill', agent: i.agent }];
    case 'add_device':
      return [{ type: 'add_device', kind: i.device }];
    case 'revoke_device':
      return [{ type: 'revoke_device', id: i.id }];
    case 'refresh_devices':
      return [{ type: 'list_devices' }];
    case 'refresh_mailbox':
      return [{ type: 'mailbox_log', thread: i.thread ?? null, limit: MAILBOX_LIMIT }];
    case 'resolve_blocker':
      return [{ type: 'resolve_blocker', id: i.id }];
    case 'check_update':
      return [{ type: 'check_update', node: i.node ?? null }];
    case 'update_node':
      return [{ type: 'update_node', node: i.node ?? null }];
  }
}

/** Local effects of an intent that went through (selection, offer cleanup). */
export function applyIntentLocally(s: State, i: Intent): State {
  switch (i.kind) {
    case 'select': {
      if (s.selected === i.agent) return s;
      // The hub answers an attach with a full redraw: drop the stale bytes.
      const { [i.agent]: _old, ...terminals } = s.terminals;
      return { ...s, selected: i.agent, terminals };
    }
    case 'detach':
      return { ...s, selected: null };
    case 'kill':
      return s.selected === i.agent ? { ...s, selected: null } : s;
    case 'add_device':
      return { ...s, otp: null };
    default:
      return s;
  }
}

export const keysFromText = (text: string): Uint8Array => utf8(text);

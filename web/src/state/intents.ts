import type { HubProfile, NodeSettings, UiToHub } from '../proto';
import { utf8 } from '../proto';
import { attachFrame, awaitsSession, MAILBOX_LIMIT } from './reducer';
import type { Intent, ProfileForm, SettingsForm, SpawnForm, State, Tri } from './types';

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
      workdir: f.repo ? '' : f.workdir.trim(),
      prompt: f.prompt,
      env: {},
      ...(f.repo ? { source: { kind: 'repo' as const, repo: f.repo.repo, branch: f.repo.branch || null, credential: f.repo.credential, allow_push: f.repo.allowPush, ...(f.repo.fresh ? { fresh: true } : {}) } } : {}),
    },
  };
}

// ---- hub profiles and node settings: the limits documented in dto.rs (the hub re-checks) ----

export type FormErrors<F> = Partial<Record<keyof F, string>>;

const PROFILE_NAME = /^[A-Za-z0-9._-]{1,64}$/;
const ENV_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/;
const CONTROL = /\p{Cc}/u;
const chars = (v: string) => [...v].length;
const words = (v: string) => v.split(/\s+/).filter(Boolean);
const lines = (v: string) => v.split('\n').map((l) => l.trim()).filter(Boolean);

export const emptyProfileForm = (): ProfileForm => ({ name: '', binary: '', args: '', configDir: '', env: '', install: '' });

/** The dialog fields of a stored profile; env values stay out (they are kept separately, masked). */
export const profileToForm = (p: HubProfile): ProfileForm =>
  ({ name: p.name, binary: p.binary, args: (p.args ?? []).join(' '), configDir: p.config_dir ?? '', env: '', install: p.install ?? '' });

/** `KEY=value` lines; a malformed line is reported, not dropped. */
function parseEnv(text: string): { env: [string, string][]; error: string | null } {
  const env: [string, string][] = [];
  for (const line of lines(text)) {
    const at = line.indexOf('=');
    const key = at < 0 ? line : line.slice(0, at).trim();
    if (at < 0) return { env, error: `"${key.slice(0, 40)}" needs a value: write KEY=value` };
    if (!ENV_KEY.test(key)) return { env, error: `"${key.slice(0, 40)}" is not a valid name: letters, digits and _, not starting with a digit` };
    if (key.toUpperCase().startsWith('ULAK')) return { env, error: `${key}: ULAK* variables are reserved for ulak` };
    env.push([key, line.slice(at + 1)]);
  }
  return { env, error: null };
}

const mergedEnv = (f: ProfileForm, keep: Record<string, string> = {}) => ({ ...keep, ...Object.fromEntries(parseEnv(f.env).env) });

export function profileErrors(f: ProfileForm, keepEnv?: Record<string, string>): FormErrors<ProfileForm> {
  const e: FormErrors<ProfileForm> = {};
  const name = f.name.trim();
  if (!PROFILE_NAME.test(name)) e.name = 'Use 1-64 characters: letters, digits, ".", "_" and "-".';
  const binary = f.binary.trim();
  if (chars(binary) < 1 || chars(binary) > 256 || CONTROL.test(binary)) e.binary = 'Enter the program: 1-256 printable characters.';
  const args = words(f.args);
  if (args.length > 32) e.args = 'At most 32 arguments.';
  else if (args.some((a) => chars(a) > 512 || CONTROL.test(a))) e.args = 'Each argument: at most 512 printable characters.';
  if (CONTROL.test(f.configDir.trim())) e.configDir = 'Only printable characters.';
  const env = parseEnv(f.env);
  if (env.error) e.env = env.error;
  else if (Object.keys(mergedEnv(f, keepEnv)).length > 32) e.env = 'At most 32 variables.';
  const install = f.install.trim();
  if (chars(install) > 4096) e.install = 'At most 4096 characters.';
  else if (install.includes('\0')) e.install = 'No NUL characters.';
  return e;
}

/** The profile a valid form describes; empty optional fields are left out (as on the wire). */
export function profileFromForm(f: ProfileForm, keepEnv?: Record<string, string>): HubProfile {
  const args = words(f.args);
  const configDir = f.configDir.trim();
  const env = mergedEnv(f, keepEnv);
  const install = f.install.trim();
  return {
    name: f.name.trim(),
    binary: f.binary.trim(),
    ...(args.length > 0 ? { args } : {}),
    ...(configDir ? { config_dir: configDir } : {}),
    ...(Object.keys(env).length > 0 ? { env } : {}),
    ...(install ? { install } : {}),
  };
}

const triOf = (v: boolean | undefined): Tri => (v === undefined ? 'inherit' : v ? 'on' : 'off');
const numText = (v: number | undefined) => (v === undefined ? '' : String(v));

export function settingsToForm(s: NodeSettings = {}): SettingsForm {
  return {
    repoRoots: (s.repo_roots ?? []).join('\n'),
    indexIntervalMins: numText(s.index_interval_mins),
    cleanAuto: triOf(s.clean_auto),
    cleanAfterExitHours: numText(s.clean_after_exit_hours),
    autoUpdate: triOf(s.auto_update),
    cpuBusyPct: numText(s.cpu_busy_pct),
    gpuBusyPct: numText(s.gpu_busy_pct),
  };
}

/** Empty = inherit; otherwise an integer (or decimal) in [min, max]. */
const inRange = (v: string, min: number, max: number, integer: boolean): boolean => {
  const t = v.trim();
  if (t === '') return true;
  const n = Number(t);
  return (integer ? /^\d+$/.test(t) : /^\d+(\.\d+)?$/.test(t)) && n >= min && n <= max;
};

export function settingsErrors(f: SettingsForm): FormErrors<SettingsForm> {
  const e: FormErrors<SettingsForm> = {};
  const roots = lines(f.repoRoots);
  if (roots.length > 16) e.repoRoots = 'At most 16 folders.';
  else if (roots.some((r) => CONTROL.test(r))) e.repoRoots = 'Only printable characters.';
  if (!inRange(f.indexIntervalMins, 1, 1440, true)) e.indexIntervalMins = 'A whole number of minutes, 1-1440.';
  if (!inRange(f.cleanAfterExitHours, 1, 8760, true)) e.cleanAfterExitHours = 'A whole number of hours, 1-8760.';
  if (!inRange(f.cpuBusyPct, 1, 100, false)) e.cpuBusyPct = 'A percentage, 1-100.';
  if (!inRange(f.gpuBusyPct, 1, 100, false)) e.gpuBusyPct = 'A percentage, 1-100.';
  return e;
}

const triValue = (t: Tri) => (t === 'inherit' ? undefined : t === 'on');
const numValue = (v: string) => (v.trim() === '' ? undefined : Number(v.trim()));

/** The settings a valid form describes; inherited fields are left out. */
export function settingsFromForm(f: SettingsForm): NodeSettings {
  const roots = lines(f.repoRoots);
  const all: NodeSettings = {
    repo_roots: roots.length > 0 ? roots : undefined,
    index_interval_mins: numValue(f.indexIntervalMins),
    clean_auto: triValue(f.cleanAuto),
    clean_after_exit_hours: numValue(f.cleanAfterExitHours),
    auto_update: triValue(f.autoUpdate),
    cpu_busy_pct: numValue(f.cpuBusyPct),
    gpu_busy_pct: numValue(f.gpuBusyPct),
  };
  return Object.fromEntries(Object.entries(all).filter(([, v]) => v !== undefined)) as NodeSettings;
}

/** Throws the first message of `errors` as an IntentError. */
function check(errors: Record<string, string | undefined>) {
  const first = Object.values(errors).find(Boolean);
  if (first) throw new IntentError(first);
}

/** Frames for `intent` against `s`. Pure; throws IntentError for input the hub would refuse. */
export function intentToFrames(s: State, i: Intent): UiToHub[] {
  switch (i.kind) {
    case 'select':
      if (s.selected === i.agent) return [];
      // A starting agent has no session yet: the store attaches once it is Running.
      return [...(s.selected ? [{ type: 'detach', agent: s.selected } as const] : []), ...(awaitsSession(s.statuses[i.agent]) ? [] : [attachFrame(s, i.agent)])];
    case 'detach':
      return s.selected ? [{ type: 'detach', agent: s.selected }] : [];
    case 'send_keys':
      return s.selected && i.data.length > 0 ? [{ type: 'pane_input', agent: s.selected, data: i.data }] : [];
    case 'answer':
      if (!i.text.trim()) throw new IntentError('The answer is empty: type something first');
      return [{ type: 'answer', question: i.question, text: i.text }];
    case 'spawn':
      return [spawnFormToFrame(i.form)];
    case 'add_credential':
      if (!i.name.trim()) throw new IntentError('Name is empty: give the token a name, e.g. work');
      if (!i.token.trim()) throw new IntentError('Token is empty: paste a GitHub token');
      return [{ type: 'add_credential', name: i.name.trim(), token: i.token.trim() }];
    case 'delete_credential':
      return [{ type: 'delete_credential', id: i.id }];
    case 'list_credentials':
      return [{ type: 'list_credentials' }];
    case 'list_repos':
      return [{ type: 'list_repos', credential: i.credential, query: i.query, page: i.page }];
    case 'list_branches':
      return [{ type: 'list_branches', credential: i.credential, repo: i.repo }];
    case 'cancel_queued':
      return [{ type: 'cancel_queued', id: i.id }];
    case 'list_queue':
      return [{ type: 'list_queue' }];
    case 'kill':
      return [{ type: 'kill', agent: i.agent }];
    case 'resize':
      return [{ type: 'resize', agent: i.agent, cols: Math.round(i.cols), rows: Math.round(i.rows) }];
    case 'reattach':
      return [attachFrame(s, i.agent)];
    case 'list_dir':
      return [{ type: 'list_dir', node: i.node, path: i.path }];
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
    case 'save_profile':
      check(profileErrors(i.form, i.keepEnv));
      return [{ type: 'save_profile', node: i.node, profile: profileFromForm(i.form, i.keepEnv) }];
    case 'delete_profile':
      return [{ type: 'delete_profile', node: i.node, name: i.name }];
    case 'save_node_settings':
      check(settingsErrors(i.form));
      return [{ type: 'save_node_settings', node: i.node, settings: settingsFromForm(i.form) }];
    case 'watch_node':
      return [{ type: 'node_stats', node: i.node }];
    case 'unwatch_node':
      return [];
    case 'install_profile':
      return [{ type: 'install_profile', node: i.node, name: i.name }];
    case 'clean_node':
      return [{ type: 'clean_node', node: i.node, dry_run: i.dryRun }];
    case 'clean_repo':
      return [{ type: 'clean_repo', node: i.node, path: i.path }];
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
    case 'resize':
      return { ...s, termSizes: { ...s.termSizes, [i.agent]: { cols: Math.round(i.cols), rows: Math.round(i.rows) } } };
    case 'reattach': {
      const { [i.agent]: _old, ...terminals } = s.terminals;
      return { ...s, terminals };
    }
    case 'kill':
      return s.selected === i.agent ? { ...s, selected: null } : s;
    case 'add_device':
      return { ...s, otp: null };
    case 'watch_node':
      return s.stats?.node === i.node ? s : { ...s, stats: { node: i.node, connectedMs: null, samples: [] } };
    case 'unwatch_node':
      return s.stats?.node === i.node ? { ...s, stats: null } : s;
    default:
      return s;
  }
}

export const keysFromText = (text: string): Uint8Array => utf8(text);

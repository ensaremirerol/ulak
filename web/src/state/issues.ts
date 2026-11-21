// Issues: template filling (port of ulak_proto::render_template) and per-issue agent state.
import type { AgentId, Issue, IssueRef, PromptTemplate } from '../proto';
import type { State } from './types';

/** `body` in a code fence longer than any backtick run inside it, so it cannot close early. */
export function fenced(body: string): string {
  let run = 0;
  let max = 0;
  for (const c of body) {
    run = c === '`' ? run + 1 : 0;
    max = Math.max(max, run);
  }
  const fence = '`'.repeat(Math.max(max + 1, 3));
  return `${fence}issue\n${body.trimEnd()}\n${fence}`;
}

export const TEMPLATE_VARS: { name: string; desc: string }[] = [
  { name: '{repo}', desc: 'owner/name' },
  { name: '{issue.number}', desc: 'the number' },
  { name: '{issue.title}', desc: 'the title' },
  { name: '{issue.author}', desc: 'who opened it' },
  { name: '{issue.url}', desc: 'link on GitHub' },
  { name: '{issue.labels}', desc: 'comma-separated' },
  { name: '{issue.body | fenced}', desc: 'the text, fenced as data' },
  { name: '{branch}', desc: 'the work branch' },
];

const repoOf = (i: IssueRef) => i.slice(0, i.lastIndexOf('#'));
export const issueNumber = (i: IssueRef): number => Number(i.slice(i.lastIndexOf('#') + 1));

/** Fills `{repo}`, `{issue.*}` and `{branch}` in one pass (a value is never expanded again). */
export function renderTemplate(tpl: string, issue: Issue, branch: string): string {
  const vars: [string, string][] = [
    ['{repo}', repoOf(issue.issue)],
    ['{issue.number}', String(issueNumber(issue.issue))],
    ['{issue.title}', issue.title],
    ['{issue.author}', issue.author],
    ['{issue.url}', issue.url],
    ['{issue.labels}', issue.labels.join(', ')],
    ['{issue.body | fenced}', fenced(issue.body)],
    ['{branch}', branch],
  ];
  let out = '';
  let rest = tpl;
  for (let i = rest.indexOf('{'); i >= 0; i = rest.indexOf('{')) {
    out += rest.slice(0, i);
    rest = rest.slice(i);
    const v = vars.find(([k]) => rest.startsWith(k));
    out += v ? v[1] : '{';
    rest = rest.slice(v ? v[0].length : 1);
  }
  return out + rest;
}

/** The first template whose labels the issue has, else the first. */
export function suggestedTemplate(templates: PromptTemplate[], labels: string[]): PromptTemplate | null {
  const lower = labels.map((l) => l.toLowerCase());
  return templates.find((t) => t.labels.some((l) => lower.includes(l.toLowerCase()))) ?? templates[0] ?? null;
}

export type IssueAgentState = 'none' | 'working' | 'forwarded';

/** Agents linked to the issue, newest first, with whether each still runs. */
export function issueAgents(s: State, issue: IssueRef): { agent: AgentId; live: boolean; pr: number | null }[] {
  return s.issueLinks
    .filter((l) => l.issue === issue)
    .sort((a, b) => b.at_ms - a.at_ms)
    .map((l) => {
      const st = s.statuses[l.agent];
      const live = st === undefined || (st !== 'Failed' && typeof st !== 'object') || s.queue.some((q) => q.agent === l.agent);
      return { agent: l.agent, live, pr: l.pr };
    });
}

export function issueAgentState(s: State, issue: IssueRef): IssueAgentState {
  if (issueAgents(s, issue).some((a) => a.live)) return 'working';
  if (s.forwards.some((f) => f.issue === issue && (f.status === 'delivered' || f.status === 'needs_owner'))) return 'forwarded';
  return 'none';
}

/** Forwards waiting for you (Inbox), newest first. */
export const waitingForwards = (s: State) => s.forwards.filter((f) => f.status === 'needs_owner');

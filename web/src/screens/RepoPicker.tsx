import { useEffect, useRef, useState } from 'preact/hooks';
import type { RepoInfo } from '../proto';
import { branchKey, repoKey, store } from '../state';
import { Button, TextField } from '../ui/components';
import { ageLabel } from '../ui/format';

export const SEARCH_DEBOUNCE_MS = 300;
export interface RepoChoice { repo: string; branch: string }

/** Search + pick a repo of `credential`, then its branch (default preselected). Reports the choice, or null while incomplete. */
export function RepoPicker({ credential, error, onChoice }: { credential: string; error?: string; onChoice(c: RepoChoice | null): void }) {
  const s = store.state.value;
  const [query, setQuery] = useState('');
  const [list, setList] = useState<RepoInfo[]>([]);
  const [more, setMore] = useState(false);
  const [page, setPage] = useState(1);
  const [loading, setLoading] = useState(false);
  const [repo, setRepo] = useState('');
  const [branch, setBranch] = useState('');
  const q = query.trim().slice(0, 100);
  const key = repoKey(credential, q || null);
  // The cached entry at request time: only a different (newer) entry is the answer.
  const seen = useRef<unknown>(undefined);
  const want = useRef(1);

  const request = (p: number) => {
    seen.current = store.state.peek().repos[key];
    want.current = p;
    setPage(p);
    setLoading(store.intent({ kind: 'list_repos', credential, query: q || null, page: p }));
  };
  useEffect(() => {
    setList([]);
    setMore(false);
    setLoading(true);
    if (q === '') return request(1);
    const t = setTimeout(() => request(1), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [credential, q]);

  const entry = s.repos[key];
  useEffect(() => {
    if (!loading || !entry || entry === seen.current) return;
    seen.current = entry;
    setList((l) => (want.current > 1 ? [...l, ...entry.repos] : entry.repos));
    setMore(entry.hasMore);
    setLoading(false);
  }, [entry, loading]);
  useEffect(() => { if (error) setLoading(false); }, [error]);

  const br = s.branches[branchKey(credential, repo)];
  const chosen = branch || br?.defaultBranch || '';
  useEffect(() => onChoice(repo && chosen ? { repo, branch: chosen } : null), [repo, chosen]);

  const pick = (r: RepoInfo) => {
    setRepo(r.full_name);
    setBranch('');
    store.intent({ kind: 'list_branches', credential, repo: r.full_name });
  };
  return (
    <div class="stack tight">
      <TextField label="Search repositories" value={query} placeholder="owner/name" autocapitalize="off" spellcheck={false} onValue={setQuery} />
      <ul class="agent-list repo-list" aria-label="Repositories">
        {list.map((r) => (
          <li key={r.full_name}>
            <button type="button" class="agent-row" aria-current={repo === r.full_name ? 'true' : undefined} onClick={() => pick(r)}>
              <span class="agent-name">
                <span>{r.full_name}</span>
                {r.description && <span class="muted small">{r.description}</span>}
              </span>
              <span class="muted small">{r.private ? 'Private' : 'Public'}{r.pushed_at_ms > 0 ? ` · pushed ${ageLabel(Math.max(0, s.now - r.pushed_at_ms))}` : ''}</span>
            </button>
          </li>
        ))}
      </ul>
      {loading && <p class="muted small" role="status">Loading repositories...</p>}
      {!loading && !error && list.length === 0 && entry && <p class="muted small">No repositories found.</p>}
      {error && !loading && <p class="err" role="alert">{error}</p>}
      {more && !loading && <Button size="sm" onClick={() => request(page + 1)}>Load more</Button>}
      {repo && (
        <div class="field">
          <label for="repo-branch">Branch</label>
          <select id="repo-branch" class="input mono" value={chosen} disabled={!br} onChange={(e) => setBranch(e.currentTarget.value)}>
            {(br?.branches ?? []).map((b) => <option key={b} value={b}>{b}</option>)}
          </select>
        </div>
      )}
    </div>
  );
}

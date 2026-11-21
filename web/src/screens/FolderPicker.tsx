import { useEffect, useState } from 'preact/hooks';
import { dirKey, listsDirs, store } from '../state';
import { Button, TextField } from '../ui/components';

/** No listing after this long: the node is offline or too old to answer. */
export const LIST_TIMEOUT_MS = 6000;

export interface FolderChoice { node: string; path: string }

/** `/home/me/src` -> [['/', '/'], ['home', '/home'], ['me', '/home/me'], ['src', '/home/me/src']]. */
export function crumbs(path: string): [label: string, path: string][] {
  const parts = path.split('/').filter(Boolean);
  return [['/', '/'], ...parts.map((p, i): [string, string] => [p, '/' + parts.slice(0, i + 1).join('/')])];
}

/** Browse a node's folders (nodes from 0.1.12; older ones: type a path or pick a known project). */
export function FolderPicker({ initial, onPick }: { initial: FolderChoice | null; onPick(c: FolderChoice): void }) {
  const s = store.state.value;
  const [node, setNode] = useState(initial?.node ?? s.nodes[0]?.id ?? '');
  const [at, setAt] = useState('~');
  const [typed, setTyped] = useState(initial?.path ?? '');
  const [late, setLate] = useState(false);
  const browse = node !== '' && listsDirs(s, node);
  const listing = s.dirListings[dirKey(node, at)];

  useEffect(() => {
    setLate(false);
    if (!browse) return;
    store.intent({ kind: 'list_dir', node, path: at });
    const t = setTimeout(() => setLate(true), LIST_TIMEOUT_MS);
    return () => clearTimeout(t);
  }, [node, at, browse]);
  useEffect(() => {
    if (listing && !listing.error) setTyped(listing.path);
  }, [listing]);

  const open = (path: string) => setAt(path);
  const known = s.nodeRepos[node] ?? [];
  const path = typed.trim();
  const host = s.nodes.find((n) => n.id === node)?.hostname ?? node;
  return (
    <div class="stack tight">
      <div class="field">
        <label for="fp-node">Node</label>
        <select id="fp-node" class="input" value={node} onChange={(e) => { setNode(e.currentTarget.value); setAt('~'); setTyped(''); }}>
          {s.nodes.map((n) => <option key={n.id} value={n.id}>{n.hostname}</option>)}
        </select>
      </div>
      {browse ? (
        <>
          {listing && !listing.error && (
            <nav class="crumbs mono small" aria-label="Path">
              {crumbs(listing.path).map(([label, p], i, all) => (i === all.length - 1
                ? <span key={p} aria-current="page">{label}</span>
                : <button key={p} type="button" class="linkish" onClick={() => open(p)}>{label}</button>))}
            </nav>
          )}
          {!listing && !late && <p class="muted small" role="status">Loading folders...</p>}
          {!listing && late && <p class="warn-note small" role="alert">{host} did not answer. Type a path below.</p>}
          {listing?.error && (
            <div class="row">
              <p class="err small grow" role="alert">{listing.error}</p>
              {at !== '~' && <Button size="sm" variant="ghost" onClick={() => open('~')}>Go to home folder</Button>}
            </div>
          )}
          {listing && !listing.error && (
            <ul class="plain folder-list" aria-label={`Folders in ${listing.path}`}>
              {listing.dirs.length === 0 && <li class="muted small">No sub-folders.</li>}
              {listing.dirs.map((d) => {
                const full = `${listing.path.replace(/\/$/, '')}/${d}`;
                const repo = known.find((r) => r.path === full);
                return (
                  <li key={d} class="folder-row">
                    <button type="button" class="folder-pick" aria-pressed={path === full} onClick={() => setTyped(full)}>
                      <span class="mono">{d}</span>
                      {repo && <span class="muted small">git · {repo.branch ?? 'detached'}</span>}
                    </button>
                    <Button size="sm" variant="ghost" aria-label={`Open ${d}`} onClick={() => open(full)}>›</Button>
                  </li>
                );
              })}
            </ul>
          )}
        </>
      ) : (
        <>
          {node && <p class="muted small">{host} cannot list its folders yet (update it to 0.1.12). Type a path, or pick a project it knows.</p>}
          {known.length > 0 && (
            <ul class="plain folder-list" aria-label="Known projects">
              {known.map((r) => (
                <li key={r.path} class="folder-row">
                  <button type="button" class="folder-pick" aria-pressed={path === r.path} onClick={() => setTyped(r.path)}>
                    <span class="mono">{r.path}</span>
                    <span class="muted small">{r.full_name} · {r.branch ?? 'detached'}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      <TextField label="Path" value={typed} placeholder="/home/me/project" mono autocapitalize="off" spellcheck={false} onValue={setTyped} />
      <div class="row end">
        <Button variant="primary" disabled={!node || path === ''} onClick={() => onPick({ node, path })}>Use this folder</Button>
      </div>
    </div>
  );
}

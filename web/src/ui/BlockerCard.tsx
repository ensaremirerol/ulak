import type { Blocker } from '../proto';
import { kindTitle, store } from '../state';
import { Button } from './components';
import { ConfirmButton } from './ConfirmButton';
import { navigate } from './router';

/** A permanent blocker (login / install failed): stays until marked resolved. */
export function BlockerCard({ b }: { b: Blocker }) {
  const s = store.state.value;
  const host = s.nodes.find((n) => n.id === b.node)?.hostname ?? b.node;
  const open = () => {
    if (b.agent && (s.selected === b.agent || store.intent({ kind: 'select', agent: b.agent }))) navigate('agents');
  };
  return (
    <article class="blocker" aria-label={`${kindTitle(b.kind)} on ${host}`}>
      <div class="card-title"><strong>{kindTitle(b.kind)}</strong><span class="small">Stays until resolved</span></div>
      <span>{b.profile.split('@')[0]} on {host}{b.kind === 'Login' ? ': new agents there are paused until you log in.' : ': the install failed, new agents there are paused.'}</span>
      {b.detail && <pre class="mono detail">{b.detail}</pre>}
      <div class="row">
        {b.agent && <Button variant="ghost" onClick={open}>Open agent</Button>}
        <ConfirmButton label="Mark resolved" confirmLabel="Click again to resolve" variant="primary" onConfirm={() => store.intent({ kind: 'resolve_blocker', id: b.id })} />
      </div>
    </article>
  );
}

import { useState } from 'preact/hooks';
import type { Question } from '../proto';
import { questionAgeMs, store } from '../state';
import { BlockerCard } from '../ui/BlockerCard';
import { Button, EmptyState } from '../ui/components';
import { ageLabel } from '../ui/format';

function QuestionCard({ q }: { q: Question }) {
  const [text, setText] = useState('');
  const age = questionAgeMs(store.state.value, q);
  const send = () => {
    store.intent({ kind: 'answer', question: q.id, text });
  };
  return (
    <article class="card" aria-label={`Question from ${q.from}`}>
      <div class="card-title"><strong>{q.from}</strong><span class="muted small">{ageLabel(age)}</span></div>
      <span class="question-text">{q.text}</span>
      <form class="row nowrap" onSubmit={(e) => { e.preventDefault(); send(); }}>
        <label class="sr-only" for={`ans-${q.id}`}>Answer to {q.from}</label>
        <input id={`ans-${q.id}`} class="input grow" type="text" autocomplete="off" placeholder="Your answer" value={text} onInput={(e) => setText(e.currentTarget.value)} />
        <Button type="submit" variant="primary">Send</Button>
      </form>
    </article>
  );
}

export function Inbox() {
  const s = store.state.value;
  return (
    <>
      <header class="page-head"><h1>Inbox</h1></header>
      <div class="stack">
        {s.questions.length + s.blockers.length === 0 && <EmptyState title="Nothing waiting">Questions from agents and login or install problems show up here.</EmptyState>}
        {s.blockers.map((b) => <BlockerCard key={b.id} b={b} />)}
        {s.questions.map((q) => <QuestionCard key={q.id} q={q} />)}
      </div>
    </>
  );
}

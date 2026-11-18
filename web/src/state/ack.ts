import { useEffect, useState } from 'preact/hooks';
import { store } from './store';

export type Ack = { kind: 'idle' } | { kind: 'waiting' } | { kind: 'ok' } | { kind: 'error'; message: string };

export const ACK_TIMEOUT_MS = 10_000;
const NO_ANSWER = 'No answer from the hub yet; check the connection and try again.';

/**
 * Waits for the hub's answer to a config change (profile / node settings): the next `hub_config`
 * broadcast means it was applied, the next `error` frame means it was refused.
 * ponytail: hub errors carry no request id, so an unrelated error in the same window counts as the answer.
 */
export function useHubAck(): { ack: Ack; start(): void; reset(): void } {
  const acks = store.state.value.acks;
  const [mark, setMark] = useState<{ config: number; error: number } | null>(null);
  const [ack, setAck] = useState<Ack>({ kind: 'idle' });
  useEffect(() => {
    if (!mark) return;
    if (acks.error !== mark.error) {
      setMark(null);
      setAck({ kind: 'error', message: acks.lastError });
    } else if (acks.config !== mark.config) {
      setMark(null);
      setAck({ kind: 'ok' });
    }
  }, [acks.config, acks.error, mark]);
  useEffect(() => {
    if (!mark) return;
    const t = setTimeout(() => { setMark(null); setAck({ kind: 'error', message: NO_ANSWER }); }, ACK_TIMEOUT_MS);
    return () => clearTimeout(t);
  }, [mark]);
  return {
    ack,
    start() {
      const a = store.state.value.acks;
      setMark({ config: a.config, error: a.error });
      setAck({ kind: 'waiting' });
    },
    reset() { setMark(null); setAck({ kind: 'idle' }); },
  };
}

export const BACKOFF_BASE_MS = 500;
export const BACKOFF_CAP_MS = 30_000;

/** Delay before reconnect attempt `attempt` (0-based): capped exponential, jittered to 50..100%. */
export function backoffDelay(attempt: number, random: () => number = Math.random): number {
  const ceil = Math.min(BACKOFF_CAP_MS, BACKOFF_BASE_MS * 2 ** Math.min(attempt, 20));
  return Math.round(ceil * (0.5 + 0.5 * random()));
}

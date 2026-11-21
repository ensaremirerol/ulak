import { LoginScreen } from './auth/LoginScreen';
import type { Client } from './net';
import { SCREENS } from './screens';
import { AgentWindow } from './screens/Agents';
import { store } from './state';
import { parseTermAgent, route } from './ui/router';
import { Shell } from './ui/Shell';

export function App({ client }: { client: Client }) {
  if (store.state.value.conn === 'unauthorized') return <LoginScreen onDone={() => client.retry()} />;
  const popout = parseTermAgent(globalThis.location?.hash ?? '');
  if (popout) return <AgentWindow agent={popout} />;
  const Screen = SCREENS[route.value];
  return <Shell><Screen /></Shell>;
}

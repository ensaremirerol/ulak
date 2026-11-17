import { LoginScreen } from './auth/LoginScreen';
import type { Client } from './net';
import { SCREENS } from './screens';
import { store } from './state';
import { route } from './ui/router';
import { Shell } from './ui/Shell';

export function App({ client }: { client: Client }) {
  if (store.state.value.conn === 'unauthorized') return <LoginScreen onDone={() => client.retry()} />;
  const Screen = SCREENS[route.value];
  return <Shell><Screen /></Shell>;
}

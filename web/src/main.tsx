import { render } from 'preact';
import { App } from './App';
import { createClient, probeSession, takeTokenFromLocation, wsUrl } from './net';
import { registerPwa } from './pwa/register';
import { store } from './state';
import './ui/tokens.css';
import './ui/base.css';

takeTokenFromLocation();

const client = createClient({
  url: wsUrl,
  onFrame: store.onFrame,
  onState: (state) => store.dispatch({ kind: 'conn', state }),
  resync: store.resync,
  probe: probeSession,
});
store.setSender(client.send);

setInterval(() => store.dispatch({ kind: 'tick', now: Date.now() }), 1000);
client.start();
registerPwa();

render(<App client={client} />, document.getElementById('app')!);

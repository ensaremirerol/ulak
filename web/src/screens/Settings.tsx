import { Card } from '../ui/components';
import { store } from '../state';

export function Settings() {
  const s = store.state.value;
  return (
    <>
      <header class="page-head"><h1>Settings</h1></header>
      <Card title="Hub">{s.hubVersion ? `${s.hubVersion.version} (${s.hubVersion.git})` : "unknown"}</Card>
    </>
  );
}

import { signal } from '@preact/signals';
import { registerSW } from 'virtual:pwa-register';
import { store } from '../state';

/** Set while a new service worker waits; call it to activate and reload. The Shell shows a banner for it. */
export const updateReload = signal<(() => void) | null>(null);

export function registerPwa(): void {
  const update = registerSW({
    onNeedRefresh() {
      updateReload.value = () => void update(true);
      store.toast('info', 'New version available - Reload');
    },
    onOfflineReady: () => store.toast('ok', 'Ready to work offline'),
  });
}

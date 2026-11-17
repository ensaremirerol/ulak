import preact from '@preact/preset-vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [preact()],
  test: {
    environment: 'jsdom',
    include: ['test/**/*.test.{ts,tsx}'],
    setupFiles: ['test/setup.ts'],
  },
  resolve: { alias: { 'virtual:pwa-register': new URL('./test/pwa-stub.ts', import.meta.url).pathname } },
});

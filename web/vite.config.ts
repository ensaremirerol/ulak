import preact from '@preact/preset-vite';
import { defineConfig } from 'vite';
import { VitePWA } from 'vite-plugin-pwa';

// Hub CSP: everything is same-origin (no inline script/style), so keep assets as files.
export default defineConfig({
  plugins: [
    preact(),
    VitePWA({
      registerType: 'prompt',
      injectRegister: false,
      manifestFilename: 'manifest.json',
      includeAssets: ['icon.svg', 'icons/*.png'],
      manifest: {
        name: 'Ulak',
        short_name: 'Ulak',
        description: 'Console for your ulak agents and nodes',
        start_url: '/',
        scope: '/',
        display: 'standalone',
        background_color: '#12151a',
        theme_color: '#12151a',
        icons: [
          { src: '/icons/icon-192.png', sizes: '192x192', type: 'image/png', purpose: 'any' },
          { src: '/icons/icon-512.png', sizes: '512x512', type: 'image/png', purpose: 'any' },
          { src: '/icons/maskable-512.png', sizes: '512x512', type: 'image/png', purpose: 'maskable' },
        ],
      },
      workbox: {
        // App shell and static assets only. The hub's dynamic paths are never cached nor
        // answered with index.html.
        globPatterns: ['**/*.{js,css,html,woff2,svg,png,json}'],
        navigateFallback: '/index.html',
        navigateFallbackDenylist: [/^\/(api|auth|ws|mcp|enroll)(\/|$)/],
        cleanupOutdatedCaches: true,
        clientsClaim: true,
        // registerType 'prompt': the new worker waits until the user taps Reload.
        skipWaiting: false,
      },
    }),
  ],
  build: { target: 'es2022', sourcemap: false, assetsInlineLimit: 0 },
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/auth': 'http://localhost:8080',
      '/ws': { target: 'ws://localhost:8080', ws: true },
    },
  },
});

// Minimal service worker: makes the app installable. Network only; the hub is the source of truth
// and nothing sensitive is cached. Add an app-shell cache here if offline start is wanted.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));
self.addEventListener("fetch", () => {});

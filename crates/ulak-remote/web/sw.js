// Minimal service worker: kept so the app is installable and a new version activates at once.
// No fetch handler on purpose: an empty one makes Chrome warn about navigation overhead and
// the hub is the source of truth (nothing is cached). Add an app-shell cache here for offline start.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

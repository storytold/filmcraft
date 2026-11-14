// Keep the app installable without duplicating the large WASM bundle or user media.
// Network fetches use the browser HTTP cache; offline editing is not guaranteed.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", event => event.waitUntil(self.clients.claim()));

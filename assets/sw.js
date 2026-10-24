// Minimal service worker: makes the app installable. Pages are dynamic and
// private, so nothing is cached; offline navigations get a small fallback.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
self.addEventListener('fetch', (e) => {
  if (e.request.mode !== 'navigate') return;
  e.respondWith(
    fetch(e.request).catch(
      () =>
        new Response(
          '<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>CyclAI</title><body style="font-family:sans-serif;background:#0e1218;color:#e6e8ec;display:grid;place-items:center;height:100vh;margin:0"><p>CyclAI est injoignable (VPN actif ?).</p>',
          { headers: { 'content-type': 'text/html; charset=utf-8' } }
        )
    )
  );
});

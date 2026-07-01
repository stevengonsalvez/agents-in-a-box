// ainb fleet dashboard — service worker.
//
// Three jobs:
//   1. Cache the app shell so the PWA opens offline (and is installable).
//   2. Receive web-push messages and raise a notification.
//   3. Route notification clicks back into an open (or new) dashboard window.
//
// The dynamic surfaces (/api/*, /events/*, /ws/*) are NEVER cached — they are
// live data and a stale cache would lie about fleet state.

"use strict";

const CACHE = "ainb-shell-v1";
const SHELL = ["/", "/static/styles.css", "/static/app.js", "/static/icon.svg", "/manifest.webmanifest"];

self.addEventListener("install", (event) => {
  // Pre-cache the shell, then take over immediately.
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  // Drop old shell caches and claim open clients.
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);

  // Never intercept live data — let it hit the network directly.
  if (
    url.pathname.startsWith("/api/") ||
    url.pathname.startsWith("/events") ||
    url.pathname.startsWith("/ws/") ||
    url.pathname === "/sw.js"
  ) {
    return;
  }

  // Navigations: network-first, fall back to the cached shell when offline.
  if (event.request.mode === "navigate") {
    event.respondWith(fetch(event.request).catch(() => caches.match("/")));
    return;
  }

  // Static shell assets: cache-first, populate on miss.
  event.respondWith(
    caches.match(event.request).then(
      (hit) =>
        hit ||
        fetch(event.request).then((resp) => {
          if (resp.ok && url.origin === self.location.origin) {
            const copy = resp.clone();
            caches.open(CACHE).then((c) => c.put(event.request, copy));
          }
          return resp;
        })
    )
  );
});

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch (_) {
    data = { title: "ainb", body: event.data ? event.data.text() : "" };
  }
  const title = data.title || "ainb · fleet";
  const options = {
    body: data.body || "A session needs attention.",
    tag: data.tag || "ainb",
    renotify: true,
    requireInteraction: !!data.requireInteraction,
    icon: "/static/icon.svg",
    badge: "/static/icon.svg",
    data: { sessionId: data.sessionId || "", kind: data.kind || "" },
  };
  event.waitUntil(self.registration.showNotification(title, options));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((clients) => {
      // Focus an existing dashboard window if one is open, else open one.
      for (const client of clients) {
        if ("focus" in client) {
          client.focus();
          return;
        }
      }
      if (self.clients.openWindow) return self.clients.openWindow("/");
    })
  );
});

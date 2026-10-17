// SDK lifecycle and protocol regressions. Run: node --test tests/extensions/sdk.test.cjs
// Uses a fake host and fake timers; no browser, npm dependencies or real delays.
"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "../../apps/filmcraft/src/extensions/sdk.js"), "utf8");

function host() {
  const sent = [];
  const timers = new Map();
  const handlers = new Map();
  const style = new Map();
  const loggedErrors = [];
  let timerId = 0;
  const window = {
    ipc: { postMessage: message => sent.push(JSON.parse(message)) },
    addEventListener: (name, fn) => handlers.set(name, fn),
  };
  const document = {
    readyState: "loading",
    documentElement: { style: { setProperty: (key, value) => style.set(key, value) } },
    addEventListener: (name, fn) => handlers.set(name, fn),
  };
  vm.runInNewContext(source, {
    window,
    document,
    TextEncoder,
    CSS: { supports: (property, value) => property === "color" && /^#[0-9a-f]{6}$/i.test(value) },
    console: { error: (...args) => loggedErrors.push(args) },
    setTimeout: (callback, delay) => {
      const id = ++timerId;
      timers.set(id, { callback, delay });
      return id;
    },
    clearTimeout: id => timers.delete(id),
  }, { filename: "sdk.js" });
  return {
    sdk: window.filmcraft,
    receive: window.__filmcraftReceive,
    sent,
    timers,
    style,
    loggedErrors,
    window,
    event: name => handlers.get(name)(),
    expire: () => {
      const [id, timer] = timers.entries().next().value;
      timers.delete(id);
      timer.callback();
    },
  };
}

test("replies can arrive out of order and duplicate replies cannot settle a later request", async () => {
  const h = host();
  const first = h.sdk.invoke("document.snapshot");
  const second = h.sdk.invoke("host.info");
  const [a, b] = h.sent;
  assert.notEqual(a.id, b.id);
  h.receive({ id: b.id, result: { apiVersion: 1 } });
  assert.deepEqual(await second, { apiVersion: 1 });
  h.receive({ id: b.id, result: { apiVersion: 99 } });
  h.receive(JSON.stringify({ id: a.id, result: { documentId: "42" } }));
  assert.equal((await first).documentId, "42");
  assert.equal(h.timers.size, 0);
});

test("a timed-out request is not retried and its late reply cannot resolve the next request", async () => {
  const h = host();
  const timeout = assert.rejects(h.sdk.invoke("markers.add", {}), error => error.code === "TIMEOUT");
  assert.equal(h.timers.values().next().value.delay, 15000);
  const oldId = h.sent[0].id;
  h.expire();
  await timeout;
  assert.equal(h.sent.length, 1, "mutations must not be retried automatically");
  let settled = false;
  const current = h.sdk.invoke("document.snapshot").then(value => { settled = true; return value; });
  h.receive({ id: oldId, result: { markerId: "7" } });
  await Promise.resolve();
  assert.equal(settled, false);
  h.receive({ id: h.sent[1].id, result: { documentId: "2" } });
  assert.deepEqual(await current, { documentId: "2" });
});

test("malformed and unrelated messages leave a valid request pending", async () => {
  const h = host();
  let settled = false;
  const promise = h.sdk.invoke("host.info").then(value => { settled = true; return value; });
  const id = h.sent[0].id;
  for (const message of [null, [], false, "not JSON", "x".repeat(262145), {}, { id }, { id: String(id), result: 1 }, { id: id + 1, result: 1 }]) {
    assert.doesNotThrow(() => h.receive(message));
  }
  await Promise.resolve();
  assert.equal(settled, false);
  assert.equal(h.timers.size, 1);
  h.receive({ id, result: null });
  assert.equal(await promise, null);
});

test("host errors retain their code and do not poison later requests", async () => {
  const h = host();
  const denied = assert.rejects(h.sdk.invoke("document.snapshot"), error => error.code === "PERMISSION_DENIED" && error.message === "Read permission required");
  h.receive({ id: h.sent[0].id, error: { code: "PERMISSION_DENIED", message: "Read permission required" } });
  await denied;
  const allowed = h.sdk.invoke("host.info");
  h.receive({ id: h.sent[1].id, result: { apiVersion: 1 } });
  assert.deepEqual(await allowed, { apiVersion: 1 });
});

test("pagehide rejects all pending calls, clears subscribers and prevents new requests", async () => {
  const h = host();
  const pending = [h.sdk.invoke("host.info"), h.sdk.invoke("document.snapshot")];
  const results = Promise.allSettled(pending);
  let events = 0;
  h.sdk.on("document.changed", () => events++);
  h.event("pagehide");
  for (const result of await results) {
    assert.equal(result.status, "rejected");
    assert.equal(result.reason.code, "HOST_UNAVAILABLE");
  }
  assert.equal(h.timers.size, 0);
  h.receive({ event: "document.changed", payload: {} });
  h.receive({ id: h.sent[0].id, result: {} });
  assert.equal(events, 0);
  await assert.rejects(h.sdk.invoke("host.info"), error => error.code === "HOST_UNAVAILABLE");
  assert.equal(h.sent.length, 2);
});

test("one throwing event subscriber does not block others and unsubscribe is idempotent", () => {
  const h = host();
  const seen = [];
  h.sdk.on("document.changed", () => { throw new Error("example callback failed"); });
  const off = h.sdk.on("document.changed", payload => seen.push(payload.documentId));
  h.receive({ event: "document.changed", payload: { documentId: "8" } });
  off();
  off();
  h.receive({ event: "document.changed", payload: { documentId: "9" } });
  assert.deepEqual(seen, ["8"]);
  assert.equal(h.loggedErrors.length, 2);
});

test("theme changes apply only known color tokens and initial theme waits for the document", async () => {
  const h = host();
  assert.equal(h.sent.length, 0);
  h.event("DOMContentLoaded");
  assert.equal(h.sent[0].method, "host.info");
  h.receive({ id: h.sent[0].id, result: { theme: { bg: "#112233", text: "#ffffff" } } });
  await Promise.resolve();
  assert.equal(h.style.get("--filmcraft-bg"), "#112233");
  h.receive({ event: "theme.changed", payload: { bg: "#eeeeee", accent: "url(https://example.com)", unknown: "#abcdef" } });
  assert.equal(h.style.get("--filmcraft-bg"), "#eeeeee");
  assert.equal(h.style.has("--filmcraft-accent"), false);
  assert.equal(h.style.has("--filmcraft-unknown"), false);
});

test("invalid, cyclic and oversized UTF-8 parameters never reach the host", async () => {
  const h = host();
  const circular = {}; circular.self = circular;
  for (const [method, params] of [["", {}], ["a".repeat(65), {}], ["host.info", null], ["host.info", []], ["host.info", circular]]) {
    await assert.rejects(h.sdk.invoke(method, params), error => error.code === "INVALID_ARGUMENT");
  }
  for (const text of ["a".repeat(262145), "🌒".repeat(80000)]) {
    await assert.rejects(h.sdk.invoke("host.info", { text }), error => error.code === "REQUEST_TOO_LARGE");
  }
  assert.equal(h.sent.length, 0);
  assert.equal(h.timers.size, 0);
});

test("the UTF-8 payload leaves room for the host's newline frame delimiter", async () => {
  const h = host();
  const envelopeBytes = Buffer.byteLength(JSON.stringify({ id: 1, method: "host.info", params: { text: "" } }));
  const text = "x".repeat(256 * 1024 - envelopeBytes);
  await assert.rejects(h.sdk.invoke("host.info", { text }), error => error.code === "REQUEST_TOO_LARGE");
  assert.equal(h.sent.length, 0);
});

test("backpressure permits recovery after outstanding calls finish", async () => {
  const h = host();
  const requests = Array.from({ length: 16 }, () => h.sdk.invoke("host.info"));
  await assert.rejects(h.sdk.invoke("host.info"), error => error.code === "TOO_MANY_REQUESTS");
  assert.equal(h.sent.length, 16);
  for (const request of h.sent) h.receive({ id: request.id, result: null });
  await Promise.all(requests);
  const recovered = h.sdk.invoke("host.info");
  h.receive({ id: h.sent.at(-1).id, result: "connected" });
  assert.equal(await recovered, "connected");
  assert.equal(h.timers.size, 0);
});

test("a throwing transport clears pending state and a missing transport rejects cleanly", async () => {
  const h = host();
  h.window.ipc.postMessage = () => { throw new Error("pipe closed"); };
  await assert.rejects(h.sdk.invoke("host.info"), error => error.code === "HOST_UNAVAILABLE");
  assert.equal(h.timers.size, 0);
  delete h.window.ipc;
  await assert.rejects(h.sdk.invoke("host.info"), error => error.code === "HOST_UNAVAILABLE");
});

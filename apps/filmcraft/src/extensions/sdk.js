// FilmCraft experimental web extension API v1. MIT OR Apache-2.0.
// Injected by FilmCraft before the extension's own scripts run.
(() => {
  "use strict";

  const MAX_REQUEST_BYTES = 256 * 1024;
  const MAX_REQUEST_ID = 0xffffffff;
  const MAX_PENDING = 16;
  const TIMEOUT_MS = 15000;
  const pending = new Map();
  const listeners = new Map();
  const themeKeys = ["bg", "text", "muted", "accent", "border"];
  let nextId = 1;
  let closed = false;

  function failure(code, message) {
    const error = new Error(message);
    error.name = "FilmCraftError";
    error.code = code;
    return error;
  }

  function applyTheme(theme) {
    if (!theme || typeof theme !== "object") return;
    const root = document.documentElement;
    if (!root) return;
    for (const key of themeKeys) {
      const value = theme[key];
      if (typeof value === "string" && value.length <= 128 && CSS.supports("color", value)) {
        root.style.setProperty(`--filmcraft-${key}`, value);
      }
    }
  }

  function invoke(method, params = {}) {
    return new Promise((resolve, reject) => {
      if (closed || !window.ipc || typeof window.ipc.postMessage !== "function") {
        reject(failure("HOST_UNAVAILABLE", "This window is not connected to FilmCraft."));
        return;
      }
      if (typeof method !== "string" || method.length === 0 || method.length > 64 ||
          params === null || typeof params !== "object" || Array.isArray(params)) {
        reject(failure("INVALID_ARGUMENT", "A method name and an object of parameters are required."));
        return;
      }
      if (pending.size >= MAX_PENDING) {
        reject(failure("TOO_MANY_REQUESTS", "Wait for an outstanding FilmCraft request to finish."));
        return;
      }
      const id = nextId;
      do {
        nextId = nextId >= MAX_REQUEST_ID ? 1 : nextId + 1;
      } while (pending.has(nextId));
      let serialized;
      try {
        serialized = JSON.stringify({ id, method, params });
      } catch (_) {
        reject(failure("INVALID_ARGUMENT", "Parameters must be JSON serializable."));
        return;
      }
      if (serialized.length >= MAX_REQUEST_BYTES || new TextEncoder().encode(serialized).length >= MAX_REQUEST_BYTES) {
        reject(failure("REQUEST_TOO_LARGE", "A FilmCraft request must be smaller than 256 KiB."));
        return;
      }
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(failure("TIMEOUT", "FilmCraft did not reply within 15 seconds. Refresh before retrying an edit."));
      }, TIMEOUT_MS);
      pending.set(id, { resolve, reject, timer });
      try {
        window.ipc.postMessage(serialized);
      } catch (_) {
        clearTimeout(timer);
        pending.delete(id);
        reject(failure("HOST_UNAVAILABLE", "The request could not reach FilmCraft."));
      }
    });
  }

  function on(event, callback) {
    if (typeof event !== "string" || event.length === 0 || event.length > 128 || typeof callback !== "function") {
      throw failure("INVALID_ARGUMENT", "An event name and callback are required.");
    }
    let callbacks = listeners.get(event);
    if (!callbacks) {
      callbacks = new Set();
      listeners.set(event, callbacks);
    }
    callbacks.add(callback);
    return () => {
      callbacks.delete(callback);
      if (callbacks.size === 0) listeners.delete(event);
    };
  }

  function receive(message) {
    if (typeof message === "string") {
      if (message.length > MAX_REQUEST_BYTES) return;
      try { message = JSON.parse(message); } catch (_) { return; }
    }
    if (!message || typeof message !== "object") return;
    if (typeof message.event === "string") {
      if (message.event === "theme.changed") applyTheme(message.payload);
      for (const callback of [...(listeners.get(message.event) || [])]) {
        try { callback(message.payload); } catch (error) {
          // A failing subscriber must not prevent other subscribers receiving the event.
          console.error("FilmCraft extension event callback failed", error);
        }
      }
      return;
    }
    const request = pending.get(message.id);
    if (!request) return;
    if (!Object.prototype.hasOwnProperty.call(message, "result") && !message.error) return;
    clearTimeout(request.timer);
    pending.delete(message.id);
    if (message.error) {
      const code = typeof message.error.code === "string" ? message.error.code : "HOST_ERROR";
      const detail = typeof message.error.message === "string" ? message.error.message : "FilmCraft could not complete the request.";
      request.reject(failure(code, detail));
    } else {
      request.resolve(message.result);
    }
  }

  Object.defineProperty(window, "__filmcraftReceive", { value: receive });
  Object.defineProperty(window, "filmcraft", {
    value: Object.freeze({ apiVersion: 1, invoke, on, applyTheme }),
  });
  window.addEventListener("pagehide", () => {
    closed = true;
    for (const request of pending.values()) {
      clearTimeout(request.timer);
      request.reject(failure("HOST_UNAVAILABLE", "The extension window closed."));
    }
    pending.clear();
    listeners.clear();
  });
  function initializeTheme() {
    invoke("host.info").then(info => applyTheme(info.theme)).catch(() => {});
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initializeTheme, { once: true });
  } else {
    initializeTheme();
  }
})();

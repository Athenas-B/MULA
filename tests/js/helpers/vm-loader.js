// Shared helpers for loading MULA's browser code into a node:vm context with minimal stubs, so
// it can be unit-tested without modifying the production sources or pulling in a fake-DOM
// library: the extension's plain scripts (loadScripts) and the app's ES modules (loadModules,
// which needs `node --experimental-vm-modules`; see package.json).

import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";
import vm from "node:vm";

export const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
export const MAIN_JS = path.join(REPO_ROOT, "src", "main.js");
export const EXT_SRC = path.join(REPO_ROOT, "modules", "vsd", "extension", "src");

/**
 * Objects created inside a vm context belong to another realm, so
 * assert.deepStrictEqual rejects them (different prototypes). Normalize via JSON.
 */
export function plain(value) {
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value));
}

/** Let pending promise callbacks (including ones queued inside the vm context) run. */
export async function flush(times = 3) {
  for (let i = 0; i < times; i++) await new Promise((r) => setImmediate(r));
}

/**
 * Create a vm context from `globals` (which also serves as `window`/`self`) and run
 * the given script files in it, in order. Returns the context plus helpers.
 */
export function loadScripts(files, globals) {
  globals.window ??= globals;
  globals.self ??= globals;
  const ctx = vm.createContext(globals);
  for (const file of files) {
    vm.runInContext(readFileSync(file, "utf8"), ctx, { filename: file });
  }
  return {
    ctx,
    /** The scripts' top-level functions (an alias of the context, for readability). */
    fn: ctx,
    /** Evaluate an expression in the context (useful for top-level const/let bindings). */
    run: (code) => vm.runInContext(code, ctx),
    /** Assign a top-level `let` binding (or any lvalue) from the test realm. */
    assign: (lvalue, value) => {
      ctx.__testValue = value;
      vm.runInContext(`${lvalue} = __testValue`, ctx);
      delete ctx.__testValue;
    },
  };
}

/**
 * Create a vm context from `globals` (which also serves as `window`/`self`) and evaluate the ES
 * module `entry` with everything it imports, each test getting its own module instances.
 * Returns the context and every module's namespace, keyed by file name without `.js`.
 */
export function loadModules(entry, globals) {
  globals.window ??= globals;
  globals.self ??= globals;
  const ctx = vm.createContext(globals);
  const modules = new Map();
  const load = (file) => {
    let mod = modules.get(file);
    if (mod) return mod;
    mod = new vm.SourceTextModule(readFileSync(file, "utf8"), { context: ctx, identifier: pathToFileURL(file).href });
    modules.set(file, mod);
    mod.linkRequests(mod.moduleRequests.map(({ specifier }) => load(path.resolve(path.dirname(file), specifier))));
    return mod;
  };
  const root = load(entry);
  root.instantiate();
  // Without top-level await, evaluation runs synchronously; the promise only reports errors.
  root.evaluate().catch(() => {});
  if (root.status === "errored") throw root.error;
  const namespaces = {};
  for (const [file, mod] of modules) namespaces[path.basename(file, ".js")] = mod.namespace;
  return { ctx, modules: namespaces };
}

// ── Timers ──────────────────────────────────────────────────────────────────

/** Fake timers: nothing runs unless the test calls runAll(), so no handles leak. */
export function createFakeTimers() {
  let nextId = 1;
  const timers = new Map();
  const api = {
    setTimeout(fn, ms = 0) {
      const id = nextId++;
      timers.set(id, { fn, ms, repeat: false });
      return id;
    },
    setInterval(fn, ms = 0) {
      const id = nextId++;
      timers.set(id, { fn, ms, repeat: true });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
    clearInterval(id) {
      timers.delete(id);
    },
    /** Pending timers as [{ id, ms, repeat }]. */
    pending() {
      return [...timers.entries()].map(([id, t]) => ({ id, ms: t.ms, repeat: t.repeat }));
    },
    /** Fire every currently pending timer once (timeouts are removed, intervals kept). */
    runAll() {
      for (const [id, t] of [...timers.entries()]) {
        if (!t.repeat) timers.delete(id);
        t.fn();
      }
    },
  };
  /** The timer functions, to spread into a script's globals. */
  api.globals = {
    setTimeout: api.setTimeout,
    clearTimeout: api.clearTimeout,
    setInterval: api.setInterval,
    clearInterval: api.clearInterval,
  };
  return api;
}

/** A fetch stub that records every call in `calls` and answers with `impl(url, opts)`. */
export function createFakeFetch(impl) {
  const calls = [];
  return {
    calls,
    fetch: (url, opts) => {
      calls.push({ url, opts });
      return impl(url, opts);
    },
  };
}

export function createFakeConsole() {
  const calls = [];
  const make = (level) => (...args) => calls.push({ level, args });
  return { calls, log: make("log"), info: make("info"), warn: make("warn"), error: make("error"), debug: make("debug") };
}

// ── Minimal DOM ─────────────────────────────────────────────────────────────

class FakeClassList {
  constructor() {
    this.set = new Set();
  }
  add(...names) {
    names.forEach((n) => this.set.add(n));
  }
  remove(...names) {
    names.forEach((n) => this.set.delete(n));
  }
  contains(name) {
    return this.set.has(name);
  }
  toggle(name, force) {
    const on = force === undefined ? !this.set.has(name) : !!force;
    if (on) this.set.add(name);
    else this.set.delete(name);
    return on;
  }
}

/**
 * A tiny element stand-in. querySelector / querySelectorAll return lazily created (3 for querySelectorAll),
 * stable stub elements per selector (unless configured with setQuery / setQueryAll),
 * which is enough for code that wires listeners onto freshly rendered rows.
 */
export class FakeElement {
  constructor(props = {}) {
    this.tagName = "DIV";
    this.id = "";
    this.value = "";
    this.checked = false;
    this.disabled = false;
    this.textContent = "";
    this._innerHTML = "";
    this.title = "";
    this.style = {};
    this.dataset = {};
    this.attributes = {};
    this.children = [];
    this.listeners = {};
    this.classList = new FakeClassList();
    this.scrollTop = 0;
    this.scrollHeight = 0;
    this._query = new Map();
    this._queryAll = new Map();
    Object.assign(this, props);
    if (props.tagName) this.tagName = String(props.tagName).toUpperCase();
  }
  get innerHTML() {
    return this._innerHTML;
  }
  /** Setting innerHTML to "" also drops appended children, like a real element. */
  set innerHTML(value) {
    this._innerHTML = String(value);
    if (this._innerHTML === "") this.children = [];
  }
  get className() {
    return [...this.classList.set].join(" ");
  }
  set className(value) {
    this.classList.set = new Set(String(value).split(/\s+/).filter(Boolean));
  }
  setAttribute(name, value) {
    this.attributes[name] = String(value);
  }
  getAttribute(name) {
    return name in this.attributes ? this.attributes[name] : null;
  }
  addEventListener(type, fn) {
    (this.listeners[type] ||= []).push(fn);
  }
  /** Invoke registered listeners; returns their results (e.g. promises). */
  dispatch(type, event = {}) {
    return (this.listeners[type] || []).map((fn) => fn({ target: this, ...event }));
  }
  appendChild(child) {
    this.children.push(child);
    return child;
  }
  append(...children) {
    this.children.push(...children);
  }
  replaceChildren(...children) {
    this.children = [...children];
  }
  removeChild(child) {
    this.children = this.children.filter((c) => c !== child);
    return child;
  }
  /** Minimal insertAdjacentHTML("beforeend"): each call becomes one child that keeps its markup. */
  insertAdjacentHTML(position, html) {
    if (position !== "beforeend") throw new Error(`insertAdjacentHTML: unsupported position ${position}`);
    const child = new FakeElement({ outerHTML: String(html), parentElement: this });
    this.children.push(child);
    this._innerHTML += child.outerHTML;
  }
  get childElementCount() {
    return this.children.length;
  }
  get firstElementChild() {
    return this.children[0] ?? null;
  }
  remove() {
    const parent = this.parentElement;
    if (!parent) return;
    parent.children = parent.children.filter((c) => c !== this);
    parent._innerHTML = parent.children.map((c) => c.outerHTML ?? "").join("");
  }
  select() {}
  setQuery(selector, el) {
    this._query.set(selector, el);
    return el;
  }
  setQueryAll(selector, list) {
    this._queryAll.set(selector, list);
    return list;
  }
  querySelector(selector) {
    if (!this._query.has(selector)) this._query.set(selector, new FakeElement());
    return this._query.get(selector);
  }
  querySelectorAll(selector) {
    if (!this._queryAll.has(selector)) {
      this._queryAll.set(selector, [new FakeElement(), new FakeElement(), new FakeElement()]);
    }
    return this._queryAll.get(selector);
  }
}

/**
 * A fake `document`. getElementById returns only elements registered with add(),
 * querySelector/querySelectorAll return only what was registered (null / []).
 */
export function createFakeDocument() {
  const byId = new Map();
  const query = new Map();
  const queryAll = new Map();
  const listeners = {};
  const doc = {
    title: "",
    documentElement: new FakeElement({ tagName: "html" }),
    body: new FakeElement({ tagName: "body" }),
    listeners,
    add(id, props = {}) {
      const el = props instanceof FakeElement ? props : new FakeElement({ id, ...props });
      byId.set(id, el);
      return el;
    },
    setQuery(selector, el) {
      query.set(selector, el);
      return el;
    },
    setQueryAll(selector, list) {
      queryAll.set(selector, list);
      return list;
    },
    getElementById: (id) => byId.get(id) ?? null,
    querySelector: (selector) => query.get(selector) ?? null,
    querySelectorAll: (selector) => queryAll.get(selector) ?? [],
    createElement: (tagName) => new FakeElement({ tagName }),
    addEventListener(type, fn) {
      (listeners[type] ||= []).push(fn);
    },
    execCommand: () => false,
  };
  return doc;
}

/**
 * Fake MutationObserver class plus the list of created instances. Callbacks only run
 * when the test calls trigger() (on one instance, or on all via the returned helper).
 */
export function createFakeMutationObserver() {
  const instances = [];
  class FakeMutationObserver {
    constructor(callback) {
      this.callback = callback;
      this.targets = [];
      this.disconnected = false;
      instances.push(this);
    }
    observe(target, options) {
      this.targets.push({ target, options });
    }
    disconnect() {
      this.disconnected = true;
      this.targets = [];
    }
    trigger(records = []) {
      if (!this.disconnected) this.callback(records, this);
    }
  }
  return {
    MutationObserver: FakeMutationObserver,
    instances,
    trigger: (records) => instances.forEach((o) => o.trigger(records)),
  };
}

export function createFakeLocalStorage(initial = {}, { throwOnAccess = false } = {}) {
  const map = new Map(Object.entries(initial));
  const guard = () => {
    if (throwOnAccess) throw new Error("SecurityError: storage disabled");
  };
  return {
    map,
    getItem(key) {
      guard();
      return map.has(key) ? map.get(key) : null;
    },
    setItem(key, value) {
      guard();
      map.set(key, String(value));
    },
    removeItem(key) {
      guard();
      map.delete(key);
    },
  };
}

// ── chrome.* ────────────────────────────────────────────────────────────────

function fakeEvent() {
  const listeners = [];
  const calls = [];
  return {
    listeners,
    calls,
    addListener(fn, ...rest) {
      listeners.push(fn);
      calls.push(rest);
    },
    removeListener(fn) {
      const i = listeners.indexOf(fn);
      if (i >= 0) listeners.splice(i, 1);
    },
    fire(...args) {
      return listeners.map((fn) => fn(...args));
    },
  };
}

const clone = (v) => (v === undefined ? undefined : JSON.parse(JSON.stringify(v)));

/**
 * Fake `chrome` API with an in-memory, synchronous storage.local, recorded
 * sendMessage calls (answered by `onSend`), tabs and webRequest events.
 * With `asyncStorage: true`, storage.local callbacks run on a later macrotask (reads
 * still snapshot at call time), which exposes lost-update races. Every set() call is
 * recorded in `writes`.
 */
export function createFakeChrome({ storage = {}, session = {}, tabs = [], onSend = () => undefined, asyncStorage = false } = {}) {
  const store = clone(storage);
  const sessionStore = clone(session);
  const writes = [];
  const later = (fn) => (asyncStorage ? setImmediate(fn) : fn());
  const sent = [];
  const icons = [];
  const created = [];
  const port = { name: "content", onDisconnect: fakeEvent() };

  const chrome = {
    store,
    sessionStore,
    writes,
    sent,
    icons,
    created,
    port,
    tabsList: tabs,
    onSend,
    runtime: {
      id: "test-extension-id",
      lastError: undefined,
      getURL: (p) => `chrome-extension://test-extension-id/${p}`,
      onMessage: fakeEvent(),
      onConnect: fakeEvent(),
      connect: (info) => {
        port.name = info?.name;
        return port;
      },
      sendMessage(msg, cb) {
        sent.push(clone(msg));
        const res = chrome.onSend(msg);
        cb?.(res);
      },
    },
    storage: {
      onChanged: fakeEvent(),
      // A plain in-memory area (no change events, no write log).
      session: {
        get(keys, cb) {
          const result = Object.fromEntries(
            Object.entries(keys).map(([k, d]) => [k, k in sessionStore ? clone(sessionStore[k]) : clone(d)])
          );
          later(() => cb(result));
        },
        set(obj, cb) {
          for (const [k, v] of Object.entries(obj)) sessionStore[k] = clone(v);
          later(() => cb?.());
        },
        remove(key, cb) {
          delete sessionStore[key];
          later(() => cb?.());
        },
      },
      local: {
        get(keys, cb) {
          let result;
          if (keys === null || keys === undefined) result = clone(store);
          else if (typeof keys === "string") result = keys in store ? { [keys]: clone(store[keys]) } : {};
          else if (Array.isArray(keys)) result = Object.fromEntries(keys.filter((k) => k in store).map((k) => [k, clone(store[k])]));
          else result = Object.fromEntries(Object.entries(keys).map(([k, d]) => [k, k in store ? clone(store[k]) : clone(d)]));
          later(() => cb(result));
        },
        set(obj, cb) {
          writes.push(clone(obj));
          const changes = {};
          for (const [k, v] of Object.entries(obj)) {
            changes[k] = { oldValue: clone(store[k]), newValue: clone(v) };
            store[k] = clone(v);
          }
          chrome.storage.onChanged.fire(changes, "local");
          later(() => cb?.());
        },
      },
    },
    tabs: {
      query(_q, cb) {
        cb(clone(chrome.tabsList));
      },
      get(id, cb) {
        const tab = chrome.tabsList.find((t) => t.id === id);
        if (!tab) {
          chrome.runtime.lastError = { message: `No tab with id: ${id}.` };
          cb(undefined);
          chrome.runtime.lastError = undefined;
          return;
        }
        cb(clone(tab));
      },
      create(info) {
        created.push(info);
      },
    },
    webRequest: {
      onBeforeRequest: fakeEvent(),
      onBeforeSendHeaders: fakeEvent(),
    },
    action: {
      setIcon(arg) {
        icons.push(arg);
      },
    },
    windows: {
      create(info) {
        created.push(info);
      },
    },
  };
  return chrome;
}

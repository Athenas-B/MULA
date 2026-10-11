// Unit tests for the VSD browser extension scripts (modules/vsd/extension/src/*.js).
// Each script is evaluated in a node:vm context with a fake `chrome` API.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { readFileSync } from "node:fs";
import {
  EXT_SRC,
  loadScripts,
  plain,
  flush,
  createFakeChrome,
  createFakeConsole,
  createFakeDocument,
  createFakeTimers,
  createFakeMutationObserver,
} from "./helpers/vm-loader.js";

const ERRORLOG_JS = path.join(EXT_SRC, "errorlog.js");
const CONFIG_JS = path.join(EXT_SRC, "config.js");
const BACKGROUND_JS = path.join(EXT_SRC, "background.js");
const CONTENT_JS = path.join(EXT_SRC, "content.js");
const POPUP_JS = path.join(EXT_SRC, "popup.js");
const OPTIONS_JS = path.join(EXT_SRC, "options.js");

/** Minimal fetch Response stand-in. */
function response(body, { ok = true, status = 200 } = {}) {
  return {
    ok,
    status,
    text: async () => (typeof body === "string" ? body : JSON.stringify(body)),
    json: async () => (typeof body === "string" ? JSON.parse(body) : body),
  };
}

const MEDIA_PLAYLIST = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nseg-001.ts\n#EXTINF:10,\nseg-002.ts\n";
const MASTER_PLAYLIST =
  "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n360p/index.m3u8\n" +
  "#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720\n720p/index.m3u8\n";

// ── errorlog.js ─────────────────────────────────────────────────────────────

function loadErrorlog({ storage = {} } = {}) {
  const chrome = createFakeChrome({ storage });
  const console = createFakeConsole();
  const listeners = {};
  const globals = { chrome, console, addEventListener: (type, fn) => (listeners[type] ||= []).push(fn) };
  const loaded = loadScripts([ERRORLOG_JS], globals);
  return { ...loaded, fn: loaded.ctx, chrome, console, listeners, log: () => chrome.store.errorLog || [] };
}

describe("errorlog.js", () => {
  it("vsdAppendErrorLog appends entries to storage", () => {
    const e = loadErrorlog();
    e.fn.vsdAppendErrorLog({ message: "one" });
    e.fn.vsdAppendErrorLog({ message: "two" });
    assert.deepEqual(e.log().map((x) => x.message), ["one", "two"]);
  });

  it("vsdAppendErrorLog keeps only the newest 100 entries", () => {
    const existing = Array.from({ length: 100 }, (_, i) => ({ message: `m${i}` }));
    const e = loadErrorlog({ storage: { errorLog: existing } });
    e.fn.vsdAppendErrorLog({ message: "new" });
    const log = e.log();
    assert.equal(log.length, 100);
    assert.equal(log[0].message, "m1");
    assert.equal(log[99].message, "new");
  });

  it("vsdAppendErrorLog never throws when storage is unavailable", () => {
    const e = loadErrorlog();
    e.chrome.storage.local.get = () => {
      throw new Error("Extension context invalidated.");
    };
    assert.doesNotThrow(() => e.fn.vsdAppendErrorLog({ message: "x" }));
  });

  it("vsdInstallErrorCapture records console.error/warn and still calls the originals", () => {
    const e = loadErrorlog();
    e.fn.vsdInstallErrorCapture("popup");
    e.ctx.console.error("failed", 42);
    e.ctx.console.warn("careful");
    const log = e.log();
    assert.deepEqual(
      log.map(({ context, level, message }) => ({ context, level, message })),
      [
        { context: "popup", level: "error", message: "failed 42" },
        { context: "popup", level: "warn", message: "careful" },
      ]
    );
    assert.ok(!Number.isNaN(Date.parse(log[0].timestamp)));
    assert.deepEqual(e.console.calls.map((c) => c.level), ["error", "warn"]);
  });

  it("uses the stack for Error arguments", () => {
    const e = loadErrorlog();
    e.fn.vsdInstallErrorCapture("background");
    const err = e.run("(() => { const err = new Error('boom'); err.stack = 'Error: boom\\n    at test'; return err; })()");
    e.ctx.console.error("Caught:", err);
    assert.equal(e.log()[0].message, "Caught: Error: boom\n    at test");
  });

  it("records global error events", () => {
    const e = loadErrorlog();
    e.fn.vsdInstallErrorCapture("popup");
    e.listeners.error[0]({ message: "Script error", error: { stack: "trace" } });
    e.listeners.error[0]({ error: "raw" });
    const log = e.log();
    assert.equal(log[0].message, "Script error");
    assert.equal(log[0].stack, "trace");
    assert.equal(log[1].message, "raw");
  });

  it("records unhandled promise rejections", () => {
    const e = loadErrorlog();
    e.fn.vsdInstallErrorCapture("popup");
    const err = e.run("(() => { const err = new Error('nope'); err.stack = 'S'; return err; })()");
    e.listeners.unhandledrejection[0]({ reason: err });
    e.listeners.unhandledrejection[0]({ reason: "plain" });
    const log = e.log();
    assert.equal(log[0].message, "Unhandled rejection: nope");
    assert.equal(log[0].stack, "S");
    assert.equal(log[1].message, "Unhandled rejection: plain");
    assert.equal(log[1].stack, undefined);
  });
});

// ── config.js ───────────────────────────────────────────────────────────────

function loadConfig({ storage = {}, fetch } = {}) {
  const chrome = createFakeChrome({ storage });
  const timers = createFakeTimers();
  const fetchCalls = [];
  const state = { fetch: fetch || (async () => response({ ok: true })) };
  const globals = {
    chrome,
    AbortController,
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
    fetch: (url, opts) => {
      fetchCalls.push({ url, opts });
      return state.fetch(url, opts);
    },
  };
  const loaded = loadScripts([CONFIG_JS], globals);
  return { ...loaded, fn: loaded.ctx, chrome, timers, state, fetchCalls };
}

describe("config.js", () => {
  it("defines the default server", () => {
    assert.equal(loadConfig().run("VSD_DEFAULT_SERVER"), "http://127.0.0.1:8765");
  });

  it("vsdGetServerUrl falls back to the default", async () => {
    assert.equal(await loadConfig().fn.vsdGetServerUrl(), "http://127.0.0.1:8765");
    assert.equal(await loadConfig({ storage: { serverUrl: "" } }).fn.vsdGetServerUrl(), "http://127.0.0.1:8765");
    assert.equal(await loadConfig({ storage: { serverUrl: "   " } }).fn.vsdGetServerUrl(), "http://127.0.0.1:8765");
  });

  it("vsdGetServerUrl returns the stored URL without trailing slashes", async () => {
    assert.equal(await loadConfig({ storage: { serverUrl: "http://10.0.0.2:9000" } }).fn.vsdGetServerUrl(), "http://10.0.0.2:9000");
    assert.equal(await loadConfig({ storage: { serverUrl: " http://h:1// " } }).fn.vsdGetServerUrl(), "http://h:1");
  });

  it("vsdGetServerUrl resolves the default when storage throws", async () => {
    const c = loadConfig();
    c.chrome.storage.local.get = () => {
      throw new Error("Extension context invalidated.");
    };
    assert.equal(await c.fn.vsdGetServerUrl(), "http://127.0.0.1:8765");
  });

  it("vsdServerJson fetches from the configured server and returns the JSON body", async () => {
    const c = loadConfig({ storage: { serverUrl: "http://h:1/" }, fetch: async () => response({ job_id: "j" }) });
    const init = { method: "POST", body: "{}" };
    assert.deepEqual(plain(await c.fn.vsdServerJson("/download", init)), { job_id: "j" });
    assert.equal(c.fetchCalls[0].url, "http://h:1/download");
    assert.equal(c.fetchCalls[0].opts, init);
  });

  it("vsdServerJson falls back to {} for non-JSON or non-object bodies", async () => {
    assert.deepEqual(plain(await loadConfig({ fetch: async () => response("not json") }).fn.vsdServerJson("/x")), {});
    assert.deepEqual(plain(await loadConfig({ fetch: async () => response("null") }).fn.vsdServerJson("/x")), {});
  });

  it("vsdServerJson throws the server's error message, or the status, with err.status", async () => {
    const withMessage = loadConfig({ fetch: async () => response({ error: "ffmpeg missing" }, { ok: false, status: 500 }) });
    await assert.rejects(withMessage.fn.vsdServerJson("/download"), (err) => err.message === "ffmpeg missing" && err.status === 500);
    const withoutBody = loadConfig({ fetch: async () => response("bad gateway", { ok: false, status: 502 }) });
    await assert.rejects(withoutBody.fn.vsdServerJson("/download"), (err) => err.message === "Server error: 502" && err.status === 502);
  });

  it("vsdServerJson propagates network errors", async () => {
    const c = loadConfig({
      fetch: async () => {
        throw new TypeError("Failed to fetch");
      },
    });
    await assert.rejects(c.fn.vsdServerJson("/logs"), /Failed to fetch/);
  });

  it("vsdServerJson aborts after the timeout and clears its timer otherwise", async () => {
    const hanging = loadConfig({
      fetch: (_url, opts) =>
        new Promise((_resolve, reject) => opts.signal.addEventListener("abort", () => reject(new Error("aborted")))),
    });
    const promise = hanging.fn.vsdServerJson("/logs", { method: "GET" }, 1500);
    await flush();
    assert.deepEqual(hanging.timers.pending().map((t) => t.ms), [1500]);
    assert.equal(hanging.fetchCalls[0].opts.method, "GET");
    hanging.timers.runAll();
    await assert.rejects(promise, /aborted/);

    const fast = loadConfig();
    await fast.fn.vsdServerJson("/logs", {}, 1500);
    assert.deepEqual(fast.timers.pending(), []);
  });
});

// ── background.js ───────────────────────────────────────────────────────────

function loadBackground({ storage = {}, tabs = [], fetch, asyncStorage = false } = {}) {
  const chrome = createFakeChrome({ storage, tabs, asyncStorage });
  const console = createFakeConsole();
  const timers = createFakeTimers();
  const fetchCalls = [];
  const state = { fetch: fetch || (async () => response(MEDIA_PLAYLIST)) };
  const globals = {
    chrome,
    console,
    URL,
    AbortController,
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
    fetch: (url, opts) => {
      fetchCalls.push({ url, opts });
      return state.fetch(url, opts);
    },
    addEventListener: () => {},
  };
  const loaded = loadScripts([ERRORLOG_JS, CONFIG_JS, BACKGROUND_JS], globals);
  const send = (message, sender = {}) =>
    new Promise((resolve) => {
      const [keepOpen] = chrome.runtime.onMessage.fire(message, sender, resolve);
      if (keepOpen !== true) resolve(undefined);
    });
  return {
    ...loaded,
    fn: loaded.ctx,
    chrome,
    console,
    timers,
    state,
    fetchCalls,
    send,
    streams: () => chrome.store.streams || [],
    onBeforeRequest: (details) => chrome.webRequest.onBeforeRequest.listeners[0](details),
    onBeforeSendHeaders: (details) => chrome.webRequest.onBeforeSendHeaders.listeners[0](details),
    /** Fire every pending timer once and let the resulting promise chains settle. */
    tick: async () => {
      timers.runAll();
      await flush();
    },
  };
}

/** A fetch stub answering /progress requests from `progress` (url -> body | Error). */
function progressServer(progress) {
  return async (url) => {
    const id = decodeURIComponent(url.split("/progress/")[1] || "");
    const value = typeof progress === "function" ? progress(id) : progress[id];
    if (value instanceof Error) throw value;
    if (value && value.httpStatus) return response({ error: value.error }, { ok: false, status: value.httpStatus });
    return response(value || {});
  };
}

const runningJob = (id, extra = {}) => ({ id, status: "running", stage: "starting", current: 0, total: 0, output: null, error: null, ...extra });

describe("background.js helpers", () => {
  const bg = loadBackground();

  it("isM3u8 matches .m3u8/.m3u URLs with optional query", () => {
    for (const url of [
      "https://cdn.example.com/a/index.m3u8",
      "https://cdn.example.com/a/index.m3u8?token=abc",
      "https://cdn.example.com/a/INDEX.M3U8",
      "https://cdn.example.com/list.m3u",
    ]) {
      assert.equal(bg.fn.isM3u8(url), true, url);
    }
  });

  it("isM3u8 rejects other URLs", () => {
    for (const url of [
      "https://cdn.example.com/video.mp4",
      "https://cdn.example.com/seg.ts",
      "https://cdn.example.com/index.m3u8.bak",
      "https://cdn.example.com/m3u8/page.html",
      "",
    ]) {
      assert.equal(bg.fn.isM3u8(url), false, url);
    }
  });

  it("isMasterPlaylistUrl detects master playlists by file name", () => {
    assert.equal(bg.fn.isMasterPlaylistUrl("https://cdn.example.com/hls/master.m3u8"), true);
    assert.equal(bg.fn.isMasterPlaylistUrl("https://cdn.example.com/hls/MASTER.m3u8?x=1"), true);
    assert.equal(bg.fn.isMasterPlaylistUrl("https://cdn.example.com/hls/master_1080p.m3u8"), true);
    assert.equal(bg.fn.isMasterPlaylistUrl("https://cdn.example.com/hls/master_a-b.m3u8"), true);
  });

  it("isMasterPlaylistUrl ignores media playlists and look-alikes", () => {
    assert.equal(bg.fn.isMasterPlaylistUrl("https://cdn.example.com/hls/index.m3u8"), false);
    assert.equal(bg.fn.isMasterPlaylistUrl("https://cdn.example.com/hls/remaster.m3u8"), false);
    assert.equal(bg.fn.isMasterPlaylistUrl("https://cdn.example.com/master/index.m3u8"), false);
    assert.equal(bg.fn.isMasterPlaylistUrl("https://cdn.example.com/?file=master.m3u8"), false);
    assert.equal(bg.fn.isMasterPlaylistUrl("not a url"), false);
  });

  it("isExtensionUrl detects chrome/moz extension pages", () => {
    assert.equal(bg.fn.isExtensionUrl("chrome-extension://abc/popup.html"), true);
    assert.equal(bg.fn.isExtensionUrl("moz-extension://abc/popup.html"), true);
    assert.equal(bg.fn.isExtensionUrl("https://example.com"), false);
    assert.ok(!bg.fn.isExtensionUrl(""));
    assert.ok(!bg.fn.isExtensionUrl(undefined));
  });

  it("makePageHeaders builds Referer/Origin/Accept for http(s) pages", () => {
    assert.deepEqual(plain(bg.fn.makePageHeaders("https://www.example.com/watch?v=1")), {
      Referer: "https://www.example.com/watch?v=1",
      Origin: "https://www.example.com",
      Accept: "application/vnd.apple.mpegurl, video/*, */*",
    });
  });

  it("makePageHeaders returns null for missing, unknown, non-http or invalid pages", () => {
    assert.equal(bg.fn.makePageHeaders(null), null);
    assert.equal(bg.fn.makePageHeaders(""), null);
    assert.equal(bg.fn.makePageHeaders("unknown"), null);
    assert.equal(bg.fn.makePageHeaders("file:///C:/x.html"), null);
    assert.equal(bg.fn.makePageHeaders("chrome-extension://abc/popup.html"), null);
    assert.equal(bg.fn.makePageHeaders("http//broken"), null);
  });

  it("titleFromTabs matches the page or another tab of the same origin", () => {
    const tabs = [
      { url: "chrome-extension://abc/popup.html", title: "Popup" },
      { url: "https://site.example.com/other", title: "Site tab" },
    ];
    assert.equal(bg.fn.titleFromTabs(tabs, "https://site.example.com/v/1"), "Site tab");
    assert.equal(bg.fn.titleFromTabs(tabs, "https://nomatch.example.com/"), "");
    assert.equal(bg.fn.titleFromTabs(tabs, "unknown"), "");
    assert.equal(bg.fn.titleFromTabs(tabs, "chrome-extension://abc/popup.html"), "");
  });

  it("registers the webRequest listeners with the expected options", () => {
    assert.deepEqual(plain(bg.chrome.webRequest.onBeforeRequest.calls[0]), [{ urls: ["<all_urls>"] }, []]);
    assert.deepEqual(plain(bg.chrome.webRequest.onBeforeSendHeaders.calls[0]), [
      { urls: ["<all_urls>"] },
      ["requestHeaders", "extraHeaders"],
    ]);
  });

  it("sets the colored action icon when detection is enabled", () => {
    assert.equal(bg.chrome.icons.length, 1);
    assert.equal(bg.chrome.icons[0].path[16], "icons/icon16.png");
    assert.equal(bg.chrome.icons[0].path[128], "icons/icon128.png");
  });

  it("uses the shared config instead of its own default server", () => {
    assert.equal(bg.run("typeof DEFAULT_SERVER"), "undefined");
    assert.equal(bg.run("typeof getServerUrl"), "undefined");
    assert.equal(bg.run("typeof vsdServerJson"), "function");
  });
});

describe("background.js hasTsSegments", () => {
  it("returns true for a media playlist with .ts segments", async () => {
    const bg = loadBackground();
    assert.equal(await bg.fn.hasTsSegments("https://cdn/x.m3u8", { Referer: "r" }), true);
    assert.deepEqual(plain(bg.fetchCalls[0]), { url: "https://cdn/x.m3u8", opts: { headers: { Referer: "r" } } });
  });

  it("accepts segment URLs with query strings and CRLF line endings", async () => {
    const bg = loadBackground({ fetch: async () => response("#EXTM3U\r\n#EXTINF:4,\r\nhttps://cdn/seg1.ts?token=1\r\n") });
    assert.equal(await bg.fn.hasTsSegments("https://cdn/x.m3u8", null), true);
  });

  it("returns false for a master playlist (no .ts segments)", async () => {
    const bg = loadBackground({ fetch: async () => response(MASTER_PLAYLIST) });
    assert.equal(await bg.fn.hasTsSegments("https://cdn/x.m3u8", null), false);
  });

  it("ignores .ts mentioned only in comment/tag lines", async () => {
    const bg = loadBackground({ fetch: async () => response("#EXTM3U\n#EXT-X-MAP:URI=init.ts\n#EXT-X-ENDLIST\n") });
    assert.equal(await bg.fn.hasTsSegments("https://cdn/x.m3u8", null), false);
  });

  it("fails open (true) on HTTP errors and network failures", async () => {
    const httpError = loadBackground({ fetch: async () => response("", { ok: false, status: 403 }) });
    assert.equal(await httpError.fn.hasTsSegments("https://cdn/x.m3u8", null), true);
    const networkError = loadBackground({
      fetch: async () => {
        throw new TypeError("Failed to fetch");
      },
    });
    assert.equal(await networkError.fn.hasTsSegments("https://cdn/x.m3u8", null), true);
  });
});

describe("background.js updateStore", () => {
  it("serializes concurrent updates so both persist", async () => {
    const bg = loadBackground({ asyncStorage: true });
    await Promise.all([
      bg.fn.updateStore((d) => {
        d.streams.push({ url: "a" });
      }),
      bg.fn.updateStore((d) => {
        d.streams.push({ url: "b" });
      }),
    ]);
    assert.deepEqual(bg.streams().map((s) => s.url), ["a", "b"]);
  });

  it("the fake async storage really loses unserialized updates (control)", async () => {
    const chrome = createFakeChrome({ asyncStorage: true });
    const naive = (url) =>
      new Promise((resolve) =>
        chrome.storage.local.get({ streams: [] }, ({ streams }) => {
          streams.push({ url });
          chrome.storage.local.set({ streams }, resolve);
        })
      );
    await Promise.all([naive("a"), naive("b")]);
    assert.equal(chrome.store.streams.length, 1);
  });

  it("writes only when something changed, and only the changed keys", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }] } });
    await bg.fn.updateStore(() => {});
    await bg.fn.updateStore((d) => {
      d.streams[0].url = "a";
    });
    assert.equal(bg.chrome.writes.length, 0);
    await bg.fn.updateStore((d) => {
      d.streams[0].title = "T";
    });
    assert.deepEqual(bg.chrome.writes, [{ streams: [{ url: "a", title: "T" }] }]);
  });

  it("resolves with the callback's result and survives a throwing update", async () => {
    const bg = loadBackground();
    await assert.rejects(
      bg.fn.updateStore(() => {
        throw new Error("bad update");
      }),
      /bad update/
    );
    assert.equal(
      await bg.fn.updateStore((d) => {
        d.streams.push({ url: "x" });
        return 42;
      }),
      42
    );
    assert.deepEqual(bg.streams(), [{ url: "x" }]);
  });
});

describe("background.js stream storage", () => {
  const URL_A = "https://cdn.example.com/a/index.m3u8";

  it("addStream stores a new stream with page headers", async () => {
    const bg = loadBackground();
    await bg.fn.addStream(URL_A, "https://site.example.com/watch/1", "hls", "Title");
    const [s] = bg.streams();
    assert.equal(bg.streams().length, 1);
    assert.equal(s.url, URL_A);
    assert.equal(s.pageUrl, "https://site.example.com/watch/1");
    assert.equal(s.type, "hls");
    assert.equal(s.title, "Title");
    assert.equal(s.metadata, null);
    assert.equal(s.job, undefined);
    assert.equal(typeof s.detectedAt, "number");
    assert.deepEqual(s.headers, {
      Referer: "https://site.example.com/watch/1",
      Origin: "https://site.example.com",
      Accept: "application/vnd.apple.mpegurl, video/*, */*",
    });
  });

  it("addStream ignores empty and non-HLS URLs", async () => {
    const bg = loadBackground();
    await bg.fn.addStream("", "https://site/");
    await bg.fn.addStream("https://cdn/video.mp4", "https://site/");
    assert.deepEqual(bg.streams(), []);
  });

  it("addStream deduplicates, only fills a missing title and skips unchanged writes", async () => {
    const bg = loadBackground();
    await bg.fn.addStream(URL_A, "unknown", "hls", "");
    await bg.fn.addStream(URL_A, "unknown", "hls", "First");
    await bg.fn.addStream(URL_A, "unknown", "hls", "Second");
    assert.equal(bg.streams().length, 1);
    assert.equal(bg.streams()[0].title, "First");
    assert.equal(bg.streams()[0].headers, null);
    assert.equal(bg.chrome.writes.length, 2);
  });

  it("addStream keeps the newest 50 streams, newest first", async () => {
    const existing = Array.from({ length: 50 }, (_, i) => ({ url: `https://cdn/${i}.m3u8` }));
    const bg = loadBackground({ storage: { streams: existing } });
    await bg.fn.addStream(URL_A, "unknown");
    const streams = bg.streams();
    assert.equal(streams.length, 50);
    assert.equal(streams[0].url, URL_A);
    assert.equal(streams[49].url, "https://cdn/48.m3u8");
  });

  it("addStream prefers explicitly passed headers", async () => {
    const bg = loadBackground();
    await bg.fn.addStream(URL_A, "https://site/", "hls", "", { Referer: "explicit" });
    assert.deepEqual(bg.streams()[0].headers, { Referer: "explicit" });
  });

  it("removeStreams removes only the given URLs", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }, { url: "b" }, { url: "c" }] } });
    await bg.fn.removeStreams(["a", "c"]);
    assert.deepEqual(bg.streams(), [{ url: "b" }]);
  });
});

describe("background.js detection dedupe", () => {
  const URL_A = "https://cdn.example.com/a/index.m3u8";
  const PAGE = "https://site.example.com/watch/1";

  it("verifies a new playlist before storing it", async () => {
    const bg = loadBackground();
    assert.equal(await bg.fn.detectStream(URL_A, PAGE, "hls", "T"), true);
    assert.equal(bg.fetchCalls.length, 1);
    assert.equal(bg.fetchCalls[0].opts.headers.Referer, PAGE);
    assert.equal(bg.streams()[0].url, URL_A);
  });

  it("a second detection of a verified URL does no fetch and no storage write", async () => {
    const bg = loadBackground();
    await bg.fn.detectStream(URL_A, PAGE, "hls", "T");
    const writes = bg.chrome.writes.length;
    assert.equal(await bg.fn.detectStream(URL_A, PAGE, "hls", "T"), true);
    assert.equal(bg.fetchCalls.length, 1);
    assert.equal(bg.chrome.writes.length, writes);
  });

  it("concurrent detections of the same URL share one playlist fetch", async () => {
    const bg = loadBackground();
    await Promise.all([bg.fn.detectStream(URL_A, PAGE), bg.fn.detectStream(URL_A, PAGE), bg.fn.detectStream(URL_A, PAGE)]);
    assert.equal(bg.fetchCalls.length, 1);
    assert.equal(bg.streams().length, 1);
  });

  it("playlists without .ts segments are never stored and not refetched", async () => {
    const bg = loadBackground({ fetch: async () => response(MASTER_PLAYLIST) });
    assert.equal(await bg.fn.detectStream(URL_A, PAGE), false);
    assert.equal(await bg.fn.detectStream(URL_A, PAGE), false);
    bg.onBeforeRequest({ url: URL_A, tabId: -1, initiator: "https://site.example.com" });
    await flush();
    assert.equal(bg.fetchCalls.length, 1);
    assert.deepEqual(bg.streams(), []);
    assert.equal(bg.chrome.writes.length, 0);
  });

  it("repeated webRequest hits of a live playlist fetch it once", async () => {
    const bg = loadBackground({ tabs: [{ id: 7, url: PAGE, title: "Live" }] });
    for (let i = 0; i < 5; i++) {
      bg.onBeforeRequest({ url: URL_A, tabId: 7, initiator: "https://site.example.com" });
      await flush();
    }
    assert.equal(bg.fetchCalls.length, 1);
    assert.equal(bg.chrome.writes.length, 1);
  });

  it("remembers at most 500 verified URLs", async () => {
    const bg = loadBackground();
    for (let i = 0; i <= 500; i++) await bg.fn.verifyPlaylist(`https://cdn/${i}.m3u8`, null);
    assert.equal(bg.run("verifiedUrls.size"), 500);
    assert.equal(bg.run("verifiedUrls.has('https://cdn/0.m3u8')"), false);
  });
});

describe("background.js header capture (onBeforeSendHeaders)", () => {
  const URL_A = "https://cdn.example.com/a/index.m3u8";
  const headers = [
    { name: "Referer", value: "https://site.example.com/" },
    { name: "Cookie", value: "session=secret" },
    { name: "User-Agent", value: "UA/1.0" },
    { name: "accept", value: "*/*" },
    { name: "Origin", value: "https://site.example.com" },
    { name: "Authorization", value: "Bearer x" },
  ];

  it("captures only referer/origin/user-agent/accept and uses them for new streams", async () => {
    const bg = loadBackground();
    bg.onBeforeSendHeaders({ url: URL_A, initiator: "https://site.example.com", requestHeaders: headers });
    const expected = {
      Referer: "https://site.example.com/",
      "User-Agent": "UA/1.0",
      accept: "*/*",
      Origin: "https://site.example.com",
    };
    assert.deepEqual(plain(bg.run("capturedHeaders").get(URL_A)), expected);
    await bg.fn.addStream(URL_A, "https://other.example.com/page");
    assert.deepEqual(bg.streams()[0].headers, expected);
  });

  it("ignores non-HLS requests, extension initiators and requests without relevant headers", () => {
    const bg = loadBackground();
    bg.onBeforeSendHeaders({ url: "https://cdn/video.mp4", initiator: "https://site", requestHeaders: headers });
    bg.onBeforeSendHeaders({ url: URL_A, initiator: "chrome-extension://abc", requestHeaders: headers });
    bg.onBeforeSendHeaders({ url: URL_A, initiator: "https://site", requestHeaders: [{ name: "Cookie", value: "x" }] });
    assert.equal(bg.run("capturedHeaders.size"), 0);
  });

  it("evicts the oldest entry beyond 100 captured URLs", () => {
    const bg = loadBackground();
    for (let i = 0; i <= 100; i++) {
      bg.onBeforeSendHeaders({ url: `https://cdn/${i}.m3u8`, initiator: "https://site", requestHeaders: headers });
    }
    assert.equal(bg.run("capturedHeaders.size"), 100);
    assert.equal(bg.run("capturedHeaders.has('https://cdn/0.m3u8')"), false);
    assert.equal(bg.run("capturedHeaders.has('https://cdn/100.m3u8')"), true);
  });
});

describe("background.js request interception (onBeforeRequest)", () => {
  const URL_A = "https://cdn.example.com/a/index.m3u8";

  it("ignores master playlists", async () => {
    const bg = loadBackground();
    bg.onBeforeRequest({ url: "https://cdn.example.com/a/master.m3u8", tabId: -1, initiator: "https://site.example.com" });
    await flush();
    assert.deepEqual(bg.streams(), []);
    assert.equal(bg.fetchCalls.length, 0);
  });

  it("ignores non-HLS requests and requests from extension pages", async () => {
    const bg = loadBackground();
    bg.onBeforeRequest({ url: "https://cdn/video.mp4", tabId: -1, initiator: "https://site" });
    bg.onBeforeRequest({ url: URL_A, tabId: -1, initiator: "chrome-extension://abc" });
    await flush();
    assert.deepEqual(bg.streams(), []);
  });

  it("stores streams from background requests using documentUrl/initiator as page", async () => {
    const bg = loadBackground();
    bg.onBeforeRequest({ url: URL_A, tabId: -1, documentUrl: "https://doc.example.com/p", initiator: "https://init.example.com" });
    await flush();
    const [s] = bg.streams();
    assert.equal(s.pageUrl, "https://doc.example.com/p");
    assert.equal(s.headers.Referer, "https://doc.example.com/p");
    assert.equal(bg.fetchCalls[0].url, URL_A);
    assert.equal(bg.fetchCalls[0].opts.headers.Referer, "https://doc.example.com/p");
  });

  it("uses the tab URL and title when the request belongs to a tab", async () => {
    const bg = loadBackground({ tabs: [{ id: 7, url: "https://site.example.com/watch/7", title: "Cool video" }] });
    bg.onBeforeRequest({ url: URL_A, tabId: 7, initiator: "https://site.example.com" });
    await flush();
    const [s] = bg.streams();
    assert.equal(s.pageUrl, "https://site.example.com/watch/7");
    assert.equal(s.title, "Cool video");
  });

  it("falls back to the initiator when the tab lookup fails", async () => {
    const bg = loadBackground();
    bg.onBeforeRequest({ url: URL_A, tabId: 99, initiator: "https://init.example.com" });
    await flush();
    assert.equal(bg.streams()[0].pageUrl, "https://init.example.com");
  });

  it("never stores a playlist without .ts segments", async () => {
    const bg = loadBackground({ fetch: async () => response(MASTER_PLAYLIST) });
    bg.onBeforeRequest({ url: URL_A, tabId: -1, initiator: "https://site.example.com" });
    await flush();
    assert.deepEqual(bg.streams(), []);
    assert.equal(bg.chrome.writes.length, 0);
  });

  it("stops detecting when detection is disabled in storage", async () => {
    const bg = loadBackground();
    bg.chrome.storage.local.set({ detectionEnabled: false });
    bg.onBeforeRequest({ url: URL_A, tabId: -1, initiator: "https://site.example.com" });
    await flush();
    assert.deepEqual(bg.streams(), []);
    assert.equal(bg.run("detectionEnabled"), false);
  });
});

describe("background.js downloads", () => {
  const STREAM = { url: "https://cdn/x.m3u8", title: "Video", headers: { Referer: "r" }, pageUrl: "https://site/v", metadata: { tags: ["t"] } };

  it("download: posts the stream to the configured server, stores the job and schedules polling", async () => {
    const bg = loadBackground({
      storage: { serverUrl: "http://127.0.0.1:9999", streams: [STREAM] },
      fetch: async () => response({ ok: true, job_id: "job-1" }),
    });
    await bg.tick(); // startup poll: nothing running
    const res = await bg.send({ type: "download", url: "https://cdn/x.m3u8" });
    assert.deepEqual(plain(res), { ok: true, job_id: "job-1" });
    const call = bg.fetchCalls[0];
    assert.equal(call.url, "http://127.0.0.1:9999/download");
    assert.equal(call.opts.method, "POST");
    assert.deepEqual(JSON.parse(call.opts.body), {
      url: "https://cdn/x.m3u8",
      title: "Video",
      headers: { Referer: "r" },
      pageUrl: "https://site/v",
      metadata: { tags: ["t"] },
    });
    const { startedAt, ...job } = bg.streams()[0].job;
    assert.equal(typeof startedAt, "number");
    assert.deepEqual(job, { id: "job-1", status: "running", stage: "starting", current: 0, total: 0, output: null, error: null });
    assert.deepEqual(bg.timers.pending().map((t) => t.ms), [1000]);
    assert.equal(bg.chrome.store.activeJobs, undefined);
  });

  it("download: falls back to the default server and looks up a missing title from tabs", async () => {
    const bg = loadBackground({
      storage: { streams: [{ url: "https://cdn/x.m3u8", pageUrl: "https://site.example.com/v" }] },
      tabs: [{ url: "https://site.example.com/v", title: "Tab title" }],
      fetch: async () => response({ job_id: "j" }),
    });
    await bg.send({ type: "download", url: "https://cdn/x.m3u8" });
    const call = bg.fetchCalls[0];
    assert.equal(call.url, "http://127.0.0.1:8765/download");
    const body = JSON.parse(call.opts.body);
    assert.equal(body.title, "Tab title");
    assert.equal(body.headers.Referer, "https://site.example.com/v");
    assert.equal(bg.streams()[0].title, "Tab title");
  });

  it("download: reports server errors and unknown streams without storing a job", async () => {
    const storage = { streams: [{ url: "https://cdn/x.m3u8" }] };
    const withMessage = loadBackground({ storage, fetch: async () => response({ error: "ffmpeg missing" }, { ok: false, status: 500 }) });
    assert.deepEqual(plain(await withMessage.send({ type: "download", url: "https://cdn/x.m3u8" })), { error: "ffmpeg missing" });
    assert.equal(withMessage.streams()[0].job, undefined);
    const withoutBody = loadBackground({ storage, fetch: async () => response("not json", { ok: false, status: 502 }) });
    assert.deepEqual(plain(await withoutBody.send({ type: "download", url: "https://cdn/x.m3u8" })), { error: "Server error: 502" });
    const noJobId = loadBackground({ storage, fetch: async () => response({ ok: true }) });
    assert.deepEqual(plain(await noJobId.send({ type: "download", url: "https://cdn/x.m3u8" })), { error: "Server did not return a job id" });
    const unknown = loadBackground();
    assert.deepEqual(plain(await unknown.send({ type: "download", url: "https://cdn/x.m3u8" })), { error: "Stream not found" });
    assert.equal(unknown.fetchCalls.length, 0);
  });

  it("download: returns the running job instead of starting a second one", async () => {
    const bg = loadBackground({ storage: { streams: [{ ...STREAM, job: runningJob("j1") }] } });
    assert.deepEqual(plain(await bg.send({ type: "download", url: STREAM.url })), { ok: true, job_id: "j1" });
    assert.equal(bg.fetchCalls.length, 0);
  });
});

describe("background.js job polling", () => {
  it("writes progress into the stream record and stops polling when done", async () => {
    const progress = { j1: { status: "running", stage: "downloading", current: 3, total: 10 } };
    const bg = loadBackground({
      storage: { streams: [{ url: "u1", job: runningJob("j1") }] },
      fetch: progressServer(progress),
    });
    await bg.tick();
    assert.equal(bg.fetchCalls[0].url, "http://127.0.0.1:8765/progress/j1");
    assert.deepEqual(plain(bg.streams()[0].job), runningJob("j1", { stage: "downloading", current: 3, total: 10 }));
    assert.deepEqual(bg.timers.pending().map((t) => t.ms), [1000]);

    // Unchanged progress does not rewrite storage.
    const writes = bg.chrome.writes.length;
    await bg.tick();
    assert.equal(bg.chrome.writes.length, writes);

    progress.j1 = { status: "done", stage: "done", current: 10, total: 10, output: "C:/out.mp4" };
    await bg.tick();
    assert.deepEqual(plain(bg.streams()[0].job), {
      id: "j1", status: "done", stage: "done", current: 10, total: 10, output: "C:/out.mp4", error: null,
    });
    assert.deepEqual(bg.timers.pending(), []);
    assert.equal(bg.fetchCalls.length, 3);
  });

  it("stores server-side job errors and stops", async () => {
    const bg = loadBackground({
      storage: { streams: [{ url: "u1", job: runningJob("j1") }] },
      fetch: progressServer({ j1: { status: "error", error: "HTTP 403" } }),
    });
    await bg.tick();
    assert.equal(bg.streams()[0].job.status, "error");
    assert.equal(bg.streams()[0].job.error, "HTTP 403");
    assert.deepEqual(bg.timers.pending(), []);
  });

  it("resumes polling of running jobs from storage when the worker starts", async () => {
    const bg = loadBackground({
      storage: {
        streams: [
          { url: "u1", job: runningJob("j1") },
          { url: "u2", job: { id: "j2", status: "done", output: "x" } },
          { url: "u3" },
        ],
      },
      fetch: progressServer({ j1: { status: "running", current: 1, total: 2 } }),
    });
    assert.deepEqual(bg.timers.pending().map((t) => t.ms), [0]);
    await bg.tick();
    assert.deepEqual(bg.fetchCalls.map((c) => c.url), ["http://127.0.0.1:8765/progress/j1"]);
    assert.equal(bg.streams()[0].job.current, 1);
  });

  it("does nothing on start when no job is running", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "u2", job: { id: "j2", status: "done" } }] } });
    await bg.tick();
    assert.equal(bg.fetchCalls.length, 0);
    assert.deepEqual(bg.timers.pending(), []);
  });

  it("uses one poll loop for all running jobs", async () => {
    const bg = loadBackground({
      storage: { streams: [{ url: "u1", job: runningJob("j1") }, { url: "u2", job: runningJob("j2") }] },
      fetch: progressServer({ j1: { status: "running", current: 1, total: 5 }, j2: { status: "done", output: "o2" } }),
    });
    assert.equal(bg.timers.pending().length, 1);
    await bg.tick();
    assert.equal(bg.fetchCalls.length, 2);
    assert.deepEqual(bg.streams().map((s) => s.job.status), ["running", "done"]);
    assert.equal(bg.timers.pending().length, 1);
    bg.fn.schedulePoll();
    bg.fn.schedulePoll(0);
    assert.equal(bg.timers.pending().length, 1);
  });

  it("a new job started while polling is idle restarts the loop", async () => {
    const bg = loadBackground({
      storage: { streams: [{ url: "u1" }] },
      fetch: async (url) => (url.endsWith("/download") ? response({ job_id: "j1" }) : response({ status: "running", current: 2, total: 4 })),
    });
    await bg.tick();
    assert.deepEqual(bg.timers.pending(), []);
    await bg.send({ type: "download", url: "u1" });
    await bg.tick();
    assert.equal(bg.streams()[0].job.current, 2);
  });

  it("marks a job unknown to the server as failed right away", async () => {
    const bg = loadBackground({
      storage: { streams: [{ url: "u1", job: runningJob("j1") }] },
      fetch: progressServer({ j1: { httpStatus: 404, error: "Unknown job" } }),
    });
    await bg.tick();
    assert.equal(bg.streams()[0].job.status, "error");
    assert.equal(bg.streams()[0].job.error, "Unknown job");
    assert.deepEqual(bg.timers.pending(), []);
  });

  it("retries network failures before giving up", async () => {
    let fail = true;
    const bg = loadBackground({
      storage: { streams: [{ url: "u1", job: runningJob("j1") }] },
      fetch: progressServer(() => (fail ? new TypeError("Failed to fetch") : { status: "running", current: 5, total: 9 })),
    });
    await bg.tick();
    await bg.tick();
    assert.equal(bg.streams()[0].job.status, "running");
    assert.equal(bg.streams()[0].job.failures, 2);
    fail = false;
    await bg.tick();
    assert.equal(bg.streams()[0].job.failures, undefined);
    assert.equal(bg.streams()[0].job.current, 5);
    fail = true;
    for (let i = 0; i < 10; i++) await bg.tick();
    assert.equal(bg.streams()[0].job.status, "error");
    assert.equal(bg.streams()[0].job.error, "Failed to fetch");
    assert.deepEqual(bg.timers.pending(), []);
  });

  it("does not resurrect a stream removed while its progress request was in flight", async () => {
    let release;
    const bg = loadBackground({
      storage: { streams: [{ url: "u1", job: runningJob("j1") }] },
      fetch: () => new Promise((resolve) => (release = () => resolve(response({ status: "running", current: 1, total: 2 })))),
    });
    bg.timers.runAll();
    await flush();
    await bg.send({ type: "remove", url: "u1" });
    release();
    await flush();
    assert.deepEqual(bg.streams(), []);
    assert.deepEqual(bg.timers.pending(), []);
  });

  it("refresh: resumes polling and fills missing titles from open tabs", async () => {
    const bg = loadBackground({
      storage: {
        streams: [
          { url: "a", pageUrl: "https://site.example.com/v/1", title: "" },
          { url: "b", pageUrl: "unknown" },
          { url: "c", pageUrl: "https://x.example.com/", title: "Keep" },
          { url: "d", pageUrl: "chrome-extension://abc/popup.html" },
          { url: "e", pageUrl: "https://nomatch.example.com/" },
        ],
      },
      tabs: [
        { url: "chrome-extension://abc/popup.html", title: "Popup" },
        { url: "https://site.example.com/other", title: "Site tab" },
        { url: "https://x.example.com/", title: "X tab" },
      ],
    });
    await bg.tick();
    assert.deepEqual(plain(await bg.send({ type: "refresh" })), { ok: true });
    assert.deepEqual(bg.streams().map((s) => s.title ?? null), ["Site tab", null, "Keep", null, null]);
    assert.deepEqual(bg.timers.pending().map((t) => t.ms), [0]);
  });
});

describe("background.js message handling", () => {
  it("metadata: stores page metadata and backfills existing streams", async () => {
    const bg = loadBackground({
      storage: {
        streams: [
          { url: "a.m3u8", pageUrl: "https://site/p" },
          { url: "b.m3u8", pageUrl: "https://site/p", metadata: { title: "old" } },
          { url: "c.m3u8", pageUrl: "https://other/" },
        ],
      },
    });
    const res = await bg.send({ type: "metadata", metadata: { title: "new" } }, { tab: { url: "https://site/p" } });
    await flush();
    assert.deepEqual(plain(res), { ok: true });
    assert.deepEqual(bg.streams().map((s) => s.metadata?.title ?? null), ["new", "old", null]);
    // New streams for the same page pick the metadata up as well.
    await bg.fn.addStream("https://cdn/d.m3u8", "https://site/p");
    assert.deepEqual(bg.streams()[0].metadata, { title: "new" });
  });

  it("detected: verifies and adds reported streams with the page title", async () => {
    const bg = loadBackground();
    const res = await bg.send(
      { type: "detected", title: "Page title", streams: [{ url: "https://cdn/x.m3u8", type: "hls" }] },
      { tab: { url: "https://site.example.com/v" } }
    );
    await flush();
    assert.deepEqual(plain(res), { ok: true });
    const [s] = bg.streams();
    assert.equal(s.url, "https://cdn/x.m3u8");
    assert.equal(s.title, "Page title");
    assert.equal(s.headers.Origin, "https://site.example.com");
    assert.equal(bg.fetchCalls.length, 1);
  });

  it("detected: ignored while detection is disabled", async () => {
    const bg = loadBackground({ storage: { detectionEnabled: false } });
    const res = await bg.send({ type: "detected", streams: [{ url: "https://cdn/x.m3u8", type: "hls" }] }, { tab: { url: "https://s/" } });
    await flush();
    assert.deepEqual(plain(res), { ok: true });
    assert.deepEqual(bg.streams(), []);
  });

  it("remove: removes one stream", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }, { url: "b" }] } });
    assert.deepEqual(plain(await bg.send({ type: "remove", url: "a" })), { ok: true });
    assert.deepEqual(bg.streams(), [{ url: "b" }]);
  });

  const MIXED = [
    { url: "idle" },
    { url: "running", job: { id: "j1", status: "running" } },
    { url: "done", job: { id: "j2", status: "done" } },
    { url: "failed", job: { id: "j3", status: "error" } },
  ];

  it("clear: keeps finished jobs and, when protected, running ones", async () => {
    const bg = loadBackground({ storage: { streams: MIXED } });
    await bg.send({ type: "clear", protectActive: true });
    assert.deepEqual(bg.streams().map((s) => s.url), ["running", "done", "failed"]);
  });

  it("clear: drops running downloads when not protected", async () => {
    const bg = loadBackground({ storage: { streams: MIXED } });
    await bg.send({ type: "clear", protectActive: false });
    assert.deepEqual(bg.streams().map((s) => s.url), ["done", "failed"]);
  });

  it("clearSaved: removes streams whose stored job is done", async () => {
    const bg = loadBackground({ storage: { streams: MIXED } });
    await bg.send({ type: "clearSaved" });
    assert.deepEqual(bg.streams().map((s) => s.url), ["idle", "running", "failed"]);
  });

  it("clearAllData resets streams and the error log", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }], errorLog: [{}], serverUrl: "x" } });
    await bg.send({ type: "clearAllData" });
    assert.deepEqual(bg.chrome.store, { streams: [], errorLog: [], serverUrl: "x" });
  });

  it("does not keep the channel open for unknown or removed message types", () => {
    const bg = loadBackground();
    for (const type of ["nope", "getActiveJobs", "progress", "clearJob", "getStreams"]) {
      assert.deepEqual(bg.chrome.runtime.onMessage.fire({ type }, {}, () => {}), [false], type);
    }
  });
});

// ── content.js ──────────────────────────────────────────────────────────────

function loadContent({ title = "Page", href = "https://site.example.com/watch/1", queries = {}, queryAll = {} } = {}) {
  const chrome = createFakeChrome();
  const console = createFakeConsole();
  const timers = createFakeTimers();
  const document = createFakeDocument();
  const mo = createFakeMutationObserver();
  document.title = title;
  for (const [sel, el] of Object.entries(queries)) document.setQuery(sel, el);
  for (const [sel, list] of Object.entries(queryAll)) document.setQueryAll(sel, list);
  const connects = [];
  const connect = chrome.runtime.connect;
  chrome.runtime.connect = (info) => {
    connects.push(info);
    return connect(info);
  };
  const location = { href };
  const globals = {
    chrome,
    console,
    document,
    location,
    MutationObserver: mo.MutationObserver,
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
    setInterval: timers.setInterval,
    clearInterval: timers.clearInterval,
  };
  const loaded = loadScripts([CONTENT_JS], globals);
  return {
    ...loaded,
    fn: loaded.ctx,
    chrome,
    console,
    timers,
    document,
    location,
    mo,
    connects,
    ofType: (type) => chrome.sent.filter((m) => m.type === type),
    /** Simulate a DOM mutation and let the throttled scan run. */
    mutate: () => {
      mo.trigger();
      timers.runAll();
    },
  };
}

describe("content.js", () => {
  it("extractText returns the first non-empty, trimmed match", () => {
    const c = loadContent({
      queries: {
        ".videoUserInfo .username": { textContent: "   " },
        ".channel-name": { textContent: "  My Channel \n" },
        ".username": { textContent: "Later" },
      },
    });
    assert.equal(c.fn.extractText([".videoUserInfo .username", ".missing", ".channel-name", ".username"]), "My Channel");
    assert.equal(c.fn.extractText([".missing"]), "");
  });

  it("extractList returns de-duplicated values from the first matching selector", () => {
    const c = loadContent({
      queryAll: {
        ".tags a": [{ textContent: " HD " }, { textContent: "HD" }, { textContent: "" }, { textContent: "4K" }],
        ".tag-list a": [{ textContent: "Other" }],
      },
    });
    assert.deepEqual(plain(c.fn.extractList([".empty", ".tags a", ".tag-list a"])), ["HD", "4K"]);
    assert.deepEqual(plain(c.fn.extractList([".empty"])), []);
  });

  it("extractMetadata collects all fields", () => {
    const c = loadContent({
      title: "Video page",
      queries: { ".uploader a": { textContent: "Uploader" }, ".upload-date": { textContent: " 2024-01-02 " } },
      queryAll: {
        ".categories a": [{ textContent: "Music" }],
        ".tagsWrapper a": [{ textContent: "live" }],
        ".pornstars a": [{ textContent: "Actor" }],
      },
    });
    assert.deepEqual(plain(c.fn.extractMetadata()), {
      title: "Video page",
      channel: "Uploader",
      categories: ["Music"],
      tags: ["live"],
      actors: ["Actor"],
      uploadDate: "2024-01-02",
    });
  });

  it("connects the port, sends metadata once on load and observes the DOM instead of polling", () => {
    const c = loadContent();
    assert.equal(c.chrome.port.name, "content");
    assert.equal(c.chrome.sent.length, 1);
    assert.equal(c.chrome.sent[0].type, "metadata");
    assert.equal(c.chrome.sent[0].pageUrl, "https://site.example.com/watch/1");
    assert.deepEqual(c.timers.pending(), []);
    assert.equal(c.mo.instances.length, 1);
    const [{ target, options }] = c.mo.instances[0].targets;
    assert.equal(target, c.document.documentElement);
    assert.equal(options.subtree, true);
    assert.equal(options.childList, true);
  });

  it("sends metadata only when it changed", () => {
    const c = loadContent({ title: "One" });
    c.mutate();
    c.mutate();
    assert.equal(c.ofType("metadata").length, 1);
    c.document.title = "Two";
    c.mutate();
    assert.equal(c.ofType("metadata").length, 2);
    assert.equal(c.ofType("metadata")[1].metadata.title, "Two");
    c.mutate();
    assert.equal(c.ofType("metadata").length, 2);
    // A SPA navigation (new URL, same metadata) is reported as well.
    c.location.href = "https://site.example.com/watch/2";
    c.mutate();
    assert.equal(c.ofType("metadata").length, 3);
    assert.equal(c.ofType("metadata")[2].pageUrl, "https://site.example.com/watch/2");
  });

  it("throttles mutation-triggered scans", () => {
    const c = loadContent();
    for (let i = 0; i < 5; i++) c.mo.trigger();
    assert.deepEqual(c.timers.pending().map((t) => t.ms), [1000]);
    c.timers.runAll();
    assert.deepEqual(c.timers.pending(), []);
  });

  it("scan reports only HLS <video> sources", () => {
    const c = loadContent({
      title: "Watch",
      queryAll: {
        video: [
          { src: "https://cdn/x.m3u8?token=1" },
          { src: "https://cdn/video.mp4" },
          { src: "" },
          { src: "https://cdn/list.M3U" },
        ],
      },
    });
    const [detected] = c.ofType("detected");
    assert.ok(detected);
    assert.deepEqual(detected.streams, [
      { url: "https://cdn/x.m3u8?token=1", type: "hls" },
      { url: "https://cdn/list.M3U", type: "hls" },
    ]);
    assert.equal(detected.title, "Watch");
    assert.equal(typeof detected.metadata, "object");
  });

  it("reports each HLS source only once", () => {
    const videos = [{ src: "https://cdn/a.m3u8" }];
    const c = loadContent({ queryAll: { video: videos } });
    c.mutate();
    assert.equal(c.ofType("detected").length, 1);
    videos.push({ src: "https://cdn/b.m3u8" });
    c.mutate();
    const detected = c.ofType("detected");
    assert.equal(detected.length, 2);
    assert.deepEqual(detected[1].streams, [{ url: "https://cdn/b.m3u8", type: "hls" }]);
  });

  it("re-reports sources when detection is switched back on", () => {
    const c = loadContent({ queryAll: { video: [{ src: "https://cdn/a.m3u8" }] } });
    c.chrome.storage.local.set({ detectionEnabled: false });
    c.mutate();
    assert.equal(c.ofType("detected").length, 1);
    c.chrome.storage.local.set({ detectionEnabled: true });
    c.timers.runAll();
    assert.equal(c.ofType("detected").length, 2);
  });

  it("scan sends nothing when no HLS video is present", () => {
    const c = loadContent({ queryAll: { video: [{ src: "https://cdn/video.mp4" }] } });
    assert.equal(c.chrome.sent.some((m) => m.type === "detected"), false);
  });

  it("stops all work after the port disconnects with an invalidated context", () => {
    const c = loadContent();
    c.mo.trigger();
    c.chrome.runtime.id = undefined;
    c.chrome.port.onDisconnect.fire();
    assert.equal(c.run("contextValid"), false);
    assert.equal(c.mo.instances[0].disconnected, true);
    assert.deepEqual(c.timers.pending(), []);
    const before = c.chrome.sent.length;
    c.fn.scan();
    c.fn.sendMetadata();
    c.fn.scheduleScan();
    assert.equal(c.chrome.sent.length, before);
    assert.equal(c.connects.length, 1);
  });

  it("treats an immediate disconnect as a dead context", () => {
    const c = loadContent();
    c.chrome.port.onDisconnect.fire();
    assert.equal(c.run("contextValid"), false);
    assert.equal(c.connects.length, 1);
  });

  it("reconnects and re-sends everything when only the background worker restarted", () => {
    const c = loadContent({ queryAll: { video: [{ src: "https://cdn/a.m3u8" }] } });
    assert.equal(c.ofType("detected").length, 1);
    c.run("Date.now = () => 8.64e15");
    c.chrome.port.onDisconnect.fire();
    assert.equal(c.run("contextValid"), true);
    assert.equal(c.connects.length, 2);
    c.timers.runAll();
    assert.equal(c.ofType("metadata").length, 2);
    assert.equal(c.ofType("detected").length, 2);
  });

  it("treats a throwing chrome.runtime.id as an invalidated context", () => {
    const c = loadContent();
    Object.defineProperty(c.chrome.runtime, "id", {
      get() {
        throw new Error("Extension context invalidated.");
      },
    });
    const before = c.chrome.sent.length;
    c.document.title = "changed";
    c.fn.sendMetadata();
    assert.equal(c.chrome.sent.length, before);
    assert.equal(c.run("contextValid"), false);
    assert.equal(c.mo.instances[0].disconnected, true);
  });

  it("sendMessageSafe logs unexpected lastError values only", () => {
    const c = loadContent();
    c.chrome.runtime.lastError = { message: "Could not establish connection. Receiving end does not exist." };
    c.fn.sendMessageSafe({ type: "x" });
    assert.equal(c.console.calls.length, 0);
    c.chrome.runtime.lastError = { message: "Something else broke" };
    c.fn.sendMessageSafe({ type: "x" });
    c.chrome.runtime.lastError = undefined;
    assert.equal(c.console.calls.length, 1);
    assert.equal(c.console.calls[0].level, "error");
    assert.deepEqual(c.console.calls[0].args, ["sendMessage error:", "Something else broke"]);
  });

  it("sendMessageSafe invalidates the context when sendMessage throws", () => {
    const c = loadContent();
    c.chrome.runtime.sendMessage = () => {
      throw new Error("Extension context invalidated.");
    };
    assert.doesNotThrow(() => c.fn.sendMessageSafe({ type: "x" }));
    assert.equal(c.run("contextValid"), false);
  });
});

// ── popup.js ────────────────────────────────────────────────────────────────

function loadPopup({ storage = {}, search = "", onSend, chrome: sharedChrome, fetch } = {}) {
  const chrome = sharedChrome || createFakeChrome({ storage, onSend: onSend || (() => undefined) });
  const console = createFakeConsole();
  const timers = createFakeTimers();
  const document = createFakeDocument();
  const el = {};
  for (const id of ["streams", "status", "clear", "clearSaved", "copyLogs", "clearLogs", "popout", "protectActive", "detectionEnabled"]) {
    el[id] = document.add(id);
  }
  const fetchCalls = [];
  const clipboard = [];
  const globals = {
    chrome,
    console,
    document,
    URLSearchParams,
    AbortController,
    location: { search },
    screen: { availLeft: 0, availWidth: 1920, availTop: 0 },
    navigator: { clipboard: { writeText: async (text) => clipboard.push(text) } },
    fetch: (url, opts) => {
      fetchCalls.push({ url, opts });
      return (fetch || (async () => response({})))(url, opts);
    },
    addEventListener: () => {},
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
    setInterval: timers.setInterval,
    clearInterval: timers.clearInterval,
  };
  const loaded = loadScripts([ERRORLOG_JS, CONFIG_JS, POPUP_JS], globals);
  const items = () => el.streams.children;
  const buttons = (li) => li.children.at(-1).children;
  return { ...loaded, fn: loaded.ctx, chrome, console, timers, document, el, fetchCalls, clipboard, items, buttons };
}

describe("popup.js helpers", () => {
  const p = loadPopup();

  it("formatProgress describes each job state", () => {
    assert.equal(p.fn.formatProgress(null), "Starting...");
    assert.equal(p.fn.formatProgress(undefined), "Starting...");
    assert.equal(p.fn.formatProgress({ status: "error", error: "HTTP 403" }), "Error: HTTP 403");
    assert.equal(p.fn.formatProgress({ status: "error" }), "Error: failed");
    assert.equal(p.fn.formatProgress({ status: "done" }), "Saved");
    assert.equal(p.fn.formatProgress({ status: "running", stage: "muxing", total: 10, current: 10 }), "Muxing...");
    assert.equal(p.fn.formatProgress({ status: "running", total: 120, current: 7 }), "Downloading 7/120...");
    assert.equal(p.fn.formatProgress({ status: "running", total: 0 }), "Starting...");
  });

  it("buildTooltip lists the metadata written to the file", () => {
    const tooltip = p.fn.buildTooltip({
      url: "https://cdn/x.m3u8",
      pageUrl: "https://site/v",
      title: "Stream title",
      metadata: {
        title: "Meta title",
        channel: "Chan",
        actors: ["A", "B"],
        categories: ["C1"],
        tags: ["t1", "t2"],
        uploadDate: "2024-05-06",
      },
    });
    assert.equal(
      tooltip,
      [
        "Metadata saved in file:",
        "title: Stream title",
        "artist: Chan",
        "album_artist: A, B",
        "genre: C1",
        "keywords: t1, t2",
        "comment: https://site/v",
        "description: https://cdn/x.m3u8",
        "date: 2024-05-06",
      ].join("\n")
    );
  });

  it("buildTooltip falls back to the metadata title and skips unknown pages", () => {
    const tooltip = p.fn.buildTooltip({ url: "u", pageUrl: "unknown", title: "", metadata: { title: "Meta", tags: [] } });
    assert.equal(tooltip, "Metadata saved in file:\ntitle: Meta\ndescription: u");
  });

  it("buildTooltip returns an empty string when there is nothing to show", () => {
    assert.equal(p.fn.buildTooltip({}), "");
  });

  it("has no hardcoded server URL or in-memory job state", () => {
    assert.equal(p.run("typeof SERVER_URL"), "undefined");
    assert.equal(p.run("typeof jobProgress"), "undefined");
    assert.equal(p.run("typeof activeJobs"), "undefined");
  });
});

describe("popup.js UI", () => {
  it("adds the 'popout' class only in pop-out mode", () => {
    assert.equal(loadPopup({ search: "?popout=1" }).document.documentElement.classList.contains("popout"), true);
    assert.equal(loadPopup().document.documentElement.classList.contains("popout"), false);
  });

  it("loads toggles from storage and saves changes", () => {
    const p = loadPopup({ storage: { detectionEnabled: false, protectActiveDownloads: false } });
    assert.equal(p.el.detectionEnabled.checked, false);
    assert.equal(p.el.protectActive.checked, false);
    assert.equal(p.el.status.textContent, "Detection paused");
    p.el.detectionEnabled.checked = true;
    p.el.detectionEnabled.onchange();
    assert.equal(p.chrome.store.detectionEnabled, true);
    assert.equal(p.el.status.textContent, "No HLS streams detected yet.");
    p.el.detectionEnabled.checked = false;
    p.el.detectionEnabled.onchange();
    assert.equal(p.el.status.textContent, "Detection paused");
    p.el.protectActive.checked = true;
    p.el.protectActive.onchange();
    assert.equal(p.chrome.store.protectActiveDownloads, true);
  });

  it("asks the background to refresh on open and never polls", () => {
    const p = loadPopup();
    assert.deepEqual(p.chrome.sent, [{ type: "refresh" }]);
    assert.deepEqual(p.timers.pending(), []);
  });

  it("renders the empty state", () => {
    const p = loadPopup();
    assert.equal(p.el.status.textContent, "No HLS streams detected yet.");
    assert.equal(p.items().length, 0);
  });

  it("renders streams from storage with buttons reflecting their stored job", () => {
    const p = loadPopup({
      storage: {
        streams: [
          { url: "u1", title: "First", pageUrl: "https://site/1" },
          { url: "u2", job: { id: "j2", status: "done", output: "C:/out.mp4" } },
          { url: "u3", job: { id: "j3", status: "running", current: 3, total: 10 } },
          { url: "u4", job: { id: "j4", status: "running", stage: "muxing" } },
          { url: "u5", job: { id: "j5", status: "error", error: "boom" } },
          { url: "u6", job: { id: "j6", status: "done", output: null } },
        ],
      },
    });
    assert.equal(p.el.status.textContent, "6 stream(s) detected");
    const items = p.items();
    assert.equal(items.length, 6);
    const summary = items.map((li) => {
      const [dl, , openFile] = p.buttons(li);
      return [dl.textContent, dl.disabled, openFile.disabled];
    });
    assert.deepEqual(summary, [
      ["Download via ffmpeg", false, true],
      ["Saved", true, false],
      ["Downloading 3/10...", true, true],
      ["Muxing...", true, true],
      ["Error: boom", true, true],
      ["Saved", true, true],
    ]);

    // Title only rendered when present.
    assert.equal(items[0].children[0].className, "stream-title");
    assert.equal(items[0].children[0].textContent, "First");
    assert.equal(items[1].children[0].className, "stream-url");
    assert.match(items[0].title, /^Metadata saved in file:/);
  });

  it("re-renders on storage changes, reusing existing rows", () => {
    const p = loadPopup({ storage: { streams: [{ url: "u1" }] } });
    const [row1] = p.items();
    p.chrome.storage.local.set({ streams: [{ url: "u2", title: "New" }, { url: "u1", job: { id: "j", status: "running", current: 1, total: 4 } }] });
    const items = p.items();
    assert.equal(items.length, 2);
    assert.equal(items[1], row1);
    assert.equal(p.buttons(row1)[0].textContent, "Downloading 1/4...");
    assert.equal(items[0].children[0].textContent, "New");
    assert.equal(p.el.status.textContent, "2 stream(s) detected");

    p.chrome.storage.local.set({ streams: [{ url: "u1", title: "Now titled", job: { id: "j", status: "done", output: "o" } }] });
    assert.equal(p.items().length, 1);
    assert.equal(p.items()[0], row1);
    assert.equal(row1.children[0].textContent, "Now titled");
    assert.equal(p.buttons(row1)[0].textContent, "Saved");
    assert.equal(p.buttons(row1)[2].disabled, false);

    p.chrome.storage.local.set({ streams: [] });
    assert.equal(p.items().length, 0);
    assert.equal(p.run("rows.size"), 0);
  });

  it("ignores changes from other storage areas and unrelated keys", () => {
    const p = loadPopup({ storage: { streams: [{ url: "u1" }] } });
    p.chrome.storage.onChanged.fire({ streams: { newValue: [] } }, "sync");
    p.chrome.storage.local.set({ errorLog: [{}] });
    assert.equal(p.items().length, 1);
  });

  it("follows detection toggles made elsewhere", () => {
    const p = loadPopup({ storage: { streams: [{ url: "u1" }] } });
    p.chrome.storage.local.set({ detectionEnabled: false, protectActiveDownloads: false });
    assert.equal(p.el.detectionEnabled.checked, false);
    assert.equal(p.el.protectActive.checked, false);
    assert.equal(p.el.status.textContent, "1 stream(s) detected (detection paused)");
  });

  it("'Open page' opens a tab only for known pages", () => {
    const p = loadPopup({ storage: { streams: [{ url: "u1", pageUrl: "https://site/1" }, { url: "u2", pageUrl: "unknown" }] } });
    for (const li of p.items()) p.buttons(li)[1].onclick();
    assert.deepEqual(plain(p.chrome.created), [{ url: "https://site/1" }]);
  });

  it("'Open file' asks the configured server to open the stored output", async () => {
    const p = loadPopup({
      storage: { serverUrl: "http://h:2", streams: [{ url: "u1", job: { id: "j", status: "done", output: "C:/v.mp4" } }, { url: "u2" }] },
    });
    await p.buttons(p.items()[1])[2].onclick();
    assert.equal(p.fetchCalls.length, 0);
    await p.buttons(p.items()[0])[2].onclick();
    assert.equal(p.fetchCalls[0].url, "http://h:2/open");
    assert.equal(p.fetchCalls[0].opts.method, "POST");
    assert.deepEqual(JSON.parse(p.fetchCalls[0].opts.body), { path: "C:/v.mp4" });
  });

  it("'Remove' asks the background to remove the stream", () => {
    const p = loadPopup({ storage: { streams: [{ url: "u1" }] } });
    p.buttons(p.items()[0])[3].onclick();
    assert.deepEqual(p.chrome.sent.at(-1), { type: "remove", url: "u1" });
  });

  it("download shows 'Starting...' while the request is pending, then follows storage", async () => {
    let labelWhilePending;
    const p = loadPopup({
      storage: { streams: [{ url: "u1" }] },
      onSend: (msg) => {
        if (msg.type !== "download") return undefined;
        labelWhilePending = [p.buttons(p.items()[0])[0].textContent, p.buttons(p.items()[0])[0].disabled];
        p.chrome.storage.local.set({ streams: [{ url: "u1", job: { id: "j1", status: "running", stage: "starting" } }] });
        return { ok: true, job_id: "j1" };
      },
    });
    await p.buttons(p.items()[0])[0].onclick();
    assert.deepEqual(labelWhilePending, ["Starting...", true]);
    const dl = p.buttons(p.items()[0])[0];
    assert.equal(dl.textContent, "Starting...");
    assert.equal(dl.disabled, true);
    assert.deepEqual(p.timers.pending(), []);
    p.chrome.storage.local.set({ streams: [{ url: "u1", job: { id: "j1", status: "running", current: 2, total: 8 } }] });
    assert.equal(dl.textContent, "Downloading 2/8...");
  });

  it("download shows start errors and allows a retry", async () => {
    const p = loadPopup({
      storage: { streams: [{ url: "u1" }] },
      onSend: (msg) => (msg.type === "download" ? { error: "server down" } : undefined),
    });
    await p.fn.startDownload("u1");
    const dl = p.buttons(p.items()[0])[0];
    assert.equal(dl.textContent, "Error: server down");
    assert.equal(dl.disabled, false);
  });

  it("download reports a missing response or a runtime error", async () => {
    const p = loadPopup({ storage: { streams: [{ url: "u1" }] } });
    await p.fn.startDownload("u1");
    assert.equal(p.buttons(p.items()[0])[0].textContent, "Error: no response");
    p.chrome.runtime.sendMessage = (_msg, cb) => {
      p.chrome.runtime.lastError = { message: "Receiving end does not exist." };
      cb(undefined);
      p.chrome.runtime.lastError = undefined;
    };
    await p.fn.startDownload("u1");
    assert.equal(p.buttons(p.items()[0])[0].textContent, "Error: Receiving end does not exist.");
  });

  it("download is not sent for streams that already have a job", async () => {
    const p = loadPopup({ storage: { streams: [{ url: "u1", job: { id: "j", status: "running" } }] } });
    await p.fn.startDownload("u1");
    assert.equal(p.chrome.sent.some((m) => m.type === "download"), false);
  });

  it("'Clear' passes the protect-active-downloads setting", () => {
    const p = loadPopup();
    p.el.protectActive.checked = true;
    p.el.clear.onclick();
    p.el.protectActive.checked = false;
    p.el.clear.onclick();
    assert.deepEqual(
      p.chrome.sent.filter((m) => m.type === "clear"),
      [
        { type: "clear", protectActive: true },
        { type: "clear", protectActive: false },
      ]
    );
  });

  it("'Clear saved' and 'Clear logs' send their messages", () => {
    const p = loadPopup();
    p.el.clearSaved.onclick();
    p.el.clearLogs.onclick();
    assert.deepEqual(p.chrome.sent.slice(1), [{ type: "clearSaved" }, { type: "clearAllData" }]);
  });

  it("'Copy logs' copies storage, error log and the server log", async () => {
    const p = loadPopup({
      storage: { serverUrl: "http://h:3", streams: [{ url: "u1" }], errorLog: [{ timestamp: "T", context: "popup", level: "error", message: "oops" }] },
      fetch: async () => response({ log: "server line" }),
    });
    await p.el.copyLogs.onclick();
    assert.equal(p.fetchCalls[0].url, "http://h:3/logs?lines=200");
    const [report] = p.clipboard;
    assert.match(report, /=== VSD Diagnostics ===/);
    assert.match(report, /Server: http:\/\/h:3/);
    assert.match(report, /"url": "u1"/);
    assert.match(report, /\[T\] \(popup\/error\) oops/);
    assert.match(report, /--- Server Log ---\nserver line/);
    assert.equal(p.el.copyLogs.textContent, "Copied!");
  });

  it("'Copy logs' notes an unavailable server", async () => {
    const p = loadPopup({
      fetch: async () => {
        throw new TypeError("Failed to fetch");
      },
    });
    await p.el.copyLogs.onclick();
    assert.match(p.clipboard[0], /--- Server ---\nServer unavailable\./);
  });

  it("'Pop out' opens the popup in a window", () => {
    const p = loadPopup();
    p.el.popout.onclick();
    assert.equal(p.chrome.created[0].url, "chrome-extension://test-extension-id/popup.html?popout=1");
    assert.equal(p.chrome.created[0].type, "popup");
  });
});

describe("popup.js + background.js", () => {
  it("'Clear saved' removes streams whose stored job is done and the popup re-renders", async () => {
    const chrome = createFakeChrome({
      storage: {
        streams: [
          { url: "u1", job: { id: "j1", status: "done", output: "o" } },
          { url: "u2", job: { id: "j2", status: "error", error: "x" } },
          { url: "u3" },
        ],
      },
    });
    // Route popup messages to the background's onMessage listener.
    chrome.onSend = (msg) => {
      chrome.runtime.onMessage.fire(msg, {}, () => {});
      return { ok: true };
    };
    const timers = createFakeTimers();
    loadScripts([ERRORLOG_JS, CONFIG_JS, BACKGROUND_JS], {
      chrome,
      console: createFakeConsole(),
      URL,
      AbortController,
      setTimeout: timers.setTimeout,
      clearTimeout: timers.clearTimeout,
      fetch: async () => response({}),
      addEventListener: () => {},
    });
    const p = loadPopup({ chrome });
    assert.equal(p.items().length, 3);
    p.el.clearSaved.onclick();
    await flush();
    assert.deepEqual(chrome.store.streams.map((s) => s.url), ["u2", "u3"]);
    assert.equal(p.items().length, 2);
    assert.equal(p.el.status.textContent, "2 stream(s) detected");
  });
});

// ── options.js ──────────────────────────────────────────────────────────────

function loadOptions({ storage = {} } = {}) {
  const chrome = createFakeChrome({ storage });
  const timers = createFakeTimers();
  const document = createFakeDocument();
  const el = {
    serverUrl: document.add("serverUrl"),
    save: document.add("save"),
    msg: document.add("msg"),
  };
  loadScripts([CONFIG_JS, OPTIONS_JS], {
    chrome,
    document,
    console: createFakeConsole(),
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
  });
  return { chrome, timers, el };
}

describe("options.js", () => {
  it("shows the default server URL (also as placeholder)", async () => {
    const o = loadOptions();
    await flush();
    assert.equal(o.el.serverUrl.value, "http://127.0.0.1:8765");
    assert.equal(o.el.serverUrl.placeholder, "http://127.0.0.1:8765");
  });

  it("shows the stored server URL", async () => {
    const o = loadOptions({ storage: { serverUrl: "http://10.0.0.2:9000" } });
    await flush();
    assert.equal(o.el.serverUrl.value, "http://10.0.0.2:9000");
  });

  it("saves the trimmed URL and shows a transient confirmation", async () => {
    const o = loadOptions();
    await flush();
    o.el.serverUrl.value = "  http://localhost:1234  ";
    o.el.save.onclick();
    assert.equal(o.chrome.store.serverUrl, "http://localhost:1234");
    assert.equal(o.el.msg.textContent, "Saved.");
    assert.deepEqual(o.timers.pending().map((t) => t.ms), [2000]);
    o.timers.runAll();
    assert.equal(o.el.msg.textContent, "");
  });
});

// ── manifests / pages ───────────────────────────────────────────────────────

describe("extension packaging", () => {
  const EXT_DIR = path.dirname(EXT_SRC);
  const read = (...p) => readFileSync(path.join(EXT_DIR, ...p), "utf8");

  it("loads config.js before the scripts that use it", () => {
    const firefox = JSON.parse(read("firefox", "manifest.json"));
    assert.deepEqual(firefox.background.scripts, ["errorlog.js", "config.js", "background.js"]);
    const chrome = JSON.parse(read("chrome", "manifest.json"));
    assert.equal(chrome.background.service_worker, "background.js");
    assert.match(read("src", "background.js"), /importScripts\('errorlog\.js', 'config\.js'\)/);
    const scripts = (html) => [...html.matchAll(/<script src="([^"]+)"/g)].map((m) => m[1]);
    assert.deepEqual(scripts(read("src", "popup.html")), ["errorlog.js", "config.js", "popup.js"]);
    assert.deepEqual(scripts(read("src", "options.html")), ["config.js", "options.js"]);
  });
});

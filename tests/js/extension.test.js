// Unit tests for the VSD browser extension scripts (modules/vsd/extension/src/*.js).
// Each script is evaluated in a node:vm context with a fake `chrome` API.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import {
  EXT_SRC,
  loadScripts,
  plain,
  flush,
  createFakeChrome,
  createFakeConsole,
  createFakeDocument,
  createFakeTimers,
} from "./helpers/vm-loader.js";

const ERRORLOG_JS = path.join(EXT_SRC, "errorlog.js");
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

// ── background.js ───────────────────────────────────────────────────────────

function loadBackground({ storage = {}, tabs = [], fetch } = {}) {
  const chrome = createFakeChrome({ storage, tabs });
  const console = createFakeConsole();
  const fetchCalls = [];
  const state = { fetch: fetch || (async () => response(MEDIA_PLAYLIST)) };
  const globals = {
    chrome,
    console,
    URL,
    fetch: (url, opts) => {
      fetchCalls.push({ url, opts });
      return state.fetch(url, opts);
    },
    addEventListener: () => {},
  };
  const loaded = loadScripts([ERRORLOG_JS, BACKGROUND_JS], globals);
  const send = (message, sender = {}) =>
    new Promise((resolve) => {
      chrome.runtime.onMessage.fire(message, sender, resolve);
    });
  return {
    ...loaded,
    fn: loaded.ctx,
    chrome,
    state,
    fetchCalls,
    send,
    streams: () => chrome.store.streams || [],
    onBeforeRequest: (details) => chrome.webRequest.onBeforeRequest.listeners[0](details),
    onBeforeSendHeaders: (details) => chrome.webRequest.onBeforeSendHeaders.listeners[0](details),
  };
}

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

describe("background.js stream storage", () => {
  const URL_A = "https://cdn.example.com/a/index.m3u8";

  it("addStream stores a new stream with page headers", () => {
    const bg = loadBackground();
    bg.fn.addStream(URL_A, "https://site.example.com/watch/1", "hls", "Title");
    const [s] = bg.streams();
    assert.equal(bg.streams().length, 1);
    assert.equal(s.url, URL_A);
    assert.equal(s.pageUrl, "https://site.example.com/watch/1");
    assert.equal(s.type, "hls");
    assert.equal(s.title, "Title");
    assert.equal(s.metadata, null);
    assert.equal(typeof s.detectedAt, "number");
    assert.deepEqual(s.headers, {
      Referer: "https://site.example.com/watch/1",
      Origin: "https://site.example.com",
      Accept: "application/vnd.apple.mpegurl, video/*, */*",
    });
  });

  it("addStream ignores empty and non-HLS URLs", () => {
    const bg = loadBackground();
    bg.fn.addStream("", "https://site/");
    bg.fn.addStream("https://cdn/video.mp4", "https://site/");
    assert.deepEqual(bg.streams(), []);
  });

  it("addStream deduplicates and only fills a missing title", () => {
    const bg = loadBackground();
    bg.fn.addStream(URL_A, "unknown", "hls", "");
    bg.fn.addStream(URL_A, "unknown", "hls", "First");
    bg.fn.addStream(URL_A, "unknown", "hls", "Second");
    assert.equal(bg.streams().length, 1);
    assert.equal(bg.streams()[0].title, "First");
    assert.equal(bg.streams()[0].headers, null);
  });

  it("addStream keeps the newest 50 streams, newest first", () => {
    const existing = Array.from({ length: 50 }, (_, i) => ({ url: `https://cdn/${i}.m3u8` }));
    const bg = loadBackground({ storage: { streams: existing } });
    bg.fn.addStream(URL_A, "unknown");
    const streams = bg.streams();
    assert.equal(streams.length, 50);
    assert.equal(streams[0].url, URL_A);
    assert.equal(streams[49].url, "https://cdn/48.m3u8");
  });

  it("addStream prefers explicitly passed headers", () => {
    const bg = loadBackground();
    bg.fn.addStream(URL_A, "https://site/", "hls", "", { Referer: "explicit" });
    assert.deepEqual(bg.streams()[0].headers, { Referer: "explicit" });
  });

  it("removeStream removes only the given URL", () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }, { url: "b" }] } });
    bg.fn.removeStream("a");
    assert.deepEqual(bg.streams(), [{ url: "b" }]);
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

  it("captures only referer/origin/user-agent/accept and uses them for new streams", () => {
    const bg = loadBackground();
    bg.onBeforeSendHeaders({ url: URL_A, initiator: "https://site.example.com", requestHeaders: headers });
    const expected = {
      Referer: "https://site.example.com/",
      "User-Agent": "UA/1.0",
      accept: "*/*",
      Origin: "https://site.example.com",
    };
    assert.deepEqual(plain(bg.run("capturedHeaders").get(URL_A)), expected);
    bg.fn.addStream(URL_A, "https://other.example.com/page");
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

  it("removes the stream again when it turns out to have no .ts segments", async () => {
    const bg = loadBackground({ fetch: async () => response(MASTER_PLAYLIST) });
    bg.onBeforeRequest({ url: URL_A, tabId: -1, initiator: "https://site.example.com" });
    assert.equal(bg.streams().length, 1);
    await flush();
    assert.deepEqual(bg.streams(), []);
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
    assert.deepEqual(plain(res), { ok: true });
    assert.deepEqual(bg.streams().map((s) => s.metadata?.title ?? null), ["new", "old", null]);
    // New streams for the same page pick the metadata up as well.
    bg.fn.addStream("https://cdn/d.m3u8", "https://site/p");
    assert.deepEqual(bg.streams()[0].metadata, { title: "new" });
  });

  it("detected: adds reported streams with the page title and checks HLS segments", async () => {
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
    assert.deepEqual(plain(res), { ok: true });
    assert.deepEqual(bg.streams(), []);
  });

  it("remove: removes one stream", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }, { url: "b" }] } });
    assert.deepEqual(plain(await bg.send({ type: "remove", url: "a" })), { ok: true });
    assert.deepEqual(bg.streams(), [{ url: "b" }]);
  });

  it("clear: keeps only the given URLs and their jobs", async () => {
    const bg = loadBackground({
      storage: { streams: [{ url: "a" }, { url: "b" }, { url: "c" }], activeJobs: { a: "j1", b: "j2" } },
    });
    await bg.send({ type: "clear", keepUrls: ["b", "c"] });
    assert.deepEqual(bg.streams(), [{ url: "b" }, { url: "c" }]);
    assert.deepEqual(bg.chrome.store.activeJobs, { b: "j2" });
  });

  it("clear: without keepUrls removes everything", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }], activeJobs: { a: "j1" } } });
    await bg.send({ type: "clear" });
    assert.deepEqual(bg.streams(), []);
    assert.deepEqual(bg.chrome.store.activeJobs, {});
  });

  it("removeUrls: removes the given URLs and their jobs", async () => {
    const bg = loadBackground({
      storage: { streams: [{ url: "a" }, { url: "b" }], activeJobs: { a: "j1", b: "j2" } },
    });
    await bg.send({ type: "removeUrls", urls: ["a"] });
    assert.deepEqual(bg.streams(), [{ url: "b" }]);
    assert.deepEqual(bg.chrome.store.activeJobs, { b: "j2" });
  });

  it("clearSaved: removes saved streams and finished jobs, keeps running ones", async () => {
    const bg = loadBackground({
      storage: {
        streams: [{ url: "saved", saved: true }, { url: "done" }, { url: "running" }, { url: "idle" }],
        activeJobs: { done: "j1", running: "j2" },
      },
    });
    await bg.send({ type: "clearSaved", jobProgress: { j1: { status: "done" }, j2: { status: "running" } } });
    assert.deepEqual(bg.streams().map((s) => s.url), ["running", "idle"]);
    assert.deepEqual(bg.chrome.store.activeJobs, { running: "j2" });
  });

  it("clearJob: drops the job and marks the stream as saved", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }], activeJobs: { a: "j1", b: "j2" } } });
    await bg.send({ type: "clearJob", url: "a" });
    assert.deepEqual(bg.chrome.store.activeJobs, { b: "j2" });
    assert.equal(bg.streams()[0].saved, true);
  });

  it("getActiveJobs returns the stored jobs", async () => {
    const bg = loadBackground({ storage: { activeJobs: { a: "j1" } } });
    assert.deepEqual(plain(await bg.send({ type: "getActiveJobs" })), { a: "j1" });
  });

  it("clearAllData resets streams, jobs and the error log", async () => {
    const bg = loadBackground({ storage: { streams: [{ url: "a" }], activeJobs: { a: "j" }, errorLog: [{}], serverUrl: "x" } });
    await bg.send({ type: "clearAllData" });
    assert.deepEqual(bg.chrome.store, { streams: [], activeJobs: {}, errorLog: [], serverUrl: "x" });
  });

  it("getStreams fills missing titles from open tabs of the same origin", async () => {
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
    const streams = plain(await bg.send({ type: "getStreams" }));
    assert.deepEqual(streams.map((s) => s.title ?? null), ["Site tab", null, "Keep", null, null]);
  });

  it("download: posts the stream to the configured server and records the job", async () => {
    const bg = loadBackground({
      storage: {
        serverUrl: "http://127.0.0.1:9999",
        streams: [
          { url: "https://cdn/x.m3u8", title: "Video", headers: { Referer: "r" }, pageUrl: "https://site/v", metadata: { tags: ["t"] } },
        ],
      },
      fetch: async () => response({ job_id: "job-1" }),
    });
    const res = await bg.send({ type: "download", url: "https://cdn/x.m3u8" });
    assert.deepEqual(plain(res), { job_id: "job-1" });
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
    assert.deepEqual(bg.chrome.store.activeJobs, { "https://cdn/x.m3u8": "job-1" });
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
  });

  it("download: reports server errors", async () => {
    const withMessage = loadBackground({ fetch: async () => response({ error: "ffmpeg missing" }, { ok: false, status: 500 }) });
    assert.deepEqual(plain(await withMessage.send({ type: "download", url: "https://cdn/x.m3u8" })), { error: "ffmpeg missing" });
    const withoutBody = loadBackground({ fetch: async () => response("not json", { ok: false, status: 502 }) });
    assert.deepEqual(plain(await withoutBody.send({ type: "download", url: "https://cdn/x.m3u8" })), { error: "Server error: 502" });
  });

  it("progress: proxies the server and maps failures to status 'error'", async () => {
    const ok = loadBackground({ fetch: async () => response({ status: "running", current: 3, total: 10 }) });
    assert.deepEqual(plain(await ok.send({ type: "progress", job_id: "j1" })), { status: "running", current: 3, total: 10 });
    assert.equal(ok.fetchCalls[0].url, "http://127.0.0.1:8765/progress/j1");

    const failing = loadBackground({
      fetch: async () => {
        throw new Error("connection refused");
      },
    });
    assert.deepEqual(plain(await failing.send({ type: "progress", job_id: "j1" })), { status: "error", error: "connection refused" });
  });
});

// ── content.js ──────────────────────────────────────────────────────────────

function loadContent({ title = "Page", href = "https://site.example.com/watch/1", queries = {}, queryAll = {} } = {}) {
  const chrome = createFakeChrome();
  const console = createFakeConsole();
  const timers = createFakeTimers();
  const document = createFakeDocument();
  document.title = title;
  for (const [sel, el] of Object.entries(queries)) document.setQuery(sel, el);
  for (const [sel, list] of Object.entries(queryAll)) document.setQueryAll(sel, list);
  const globals = {
    chrome,
    console,
    document,
    location: { href },
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
    setInterval: timers.setInterval,
    clearInterval: timers.clearInterval,
  };
  const loaded = loadScripts([CONTENT_JS], globals);
  return { ...loaded, fn: loaded.ctx, chrome, console, timers, document };
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

  it("sends metadata on load and schedules periodic tasks", () => {
    const c = loadContent();
    assert.equal(c.chrome.port.name, "content");
    assert.equal(c.chrome.sent.length, 1);
    assert.equal(c.chrome.sent[0].type, "metadata");
    assert.equal(c.chrome.sent[0].pageUrl, "https://site.example.com/watch/1");
    assert.deepEqual(
      c.timers.pending().map(({ ms, repeat }) => ({ ms, repeat })),
      [
        { ms: 2000, repeat: false },
        { ms: 5000, repeat: true },
        { ms: 3000, repeat: true },
      ]
    );
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
    const detected = c.chrome.sent.find((m) => m.type === "detected");
    assert.ok(detected);
    assert.deepEqual(detected.streams, [
      { url: "https://cdn/x.m3u8?token=1", type: "hls" },
      { url: "https://cdn/list.M3U", type: "hls" },
    ]);
    assert.equal(detected.title, "Watch");
    assert.equal(typeof detected.metadata, "object");
  });

  it("scan sends nothing when no HLS video is present", () => {
    const c = loadContent({ queryAll: { video: [{ src: "https://cdn/video.mp4" }] } });
    assert.equal(c.chrome.sent.some((m) => m.type === "detected"), false);
  });

  it("stops all work after the background port disconnects", () => {
    const c = loadContent();
    c.chrome.port.onDisconnect.fire();
    assert.equal(c.run("contextValid"), false);
    assert.deepEqual(c.timers.pending().map((t) => t.ms), [2000]);
    const before = c.chrome.sent.length;
    c.timers.runAll();
    c.fn.scan();
    assert.equal(c.chrome.sent.length, before);
  });

  it("treats a throwing chrome.runtime.id as an invalidated context", () => {
    const c = loadContent();
    Object.defineProperty(c.chrome.runtime, "id", {
      get() {
        throw new Error("Extension context invalidated.");
      },
    });
    const before = c.chrome.sent.length;
    c.fn.sendMetadata();
    assert.equal(c.chrome.sent.length, before);
    assert.equal(c.run("contextValid"), false);
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

function loadPopup({ storage = {}, search = "", onSend } = {}) {
  const chrome = createFakeChrome({ storage, onSend: onSend || (() => undefined) });
  const console = createFakeConsole();
  const timers = createFakeTimers();
  const document = createFakeDocument();
  const el = {};
  for (const id of ["streams", "status", "clear", "clearSaved", "copyLogs", "clearLogs", "popout", "protectActive", "detectionEnabled"]) {
    el[id] = document.add(id);
  }
  const globals = {
    chrome,
    console,
    document,
    URLSearchParams,
    AbortController,
    location: { search },
    screen: { availLeft: 0, availWidth: 1920, availTop: 0 },
    fetch: async () => response({}),
    addEventListener: () => {},
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
    setInterval: timers.setInterval,
    clearInterval: timers.clearInterval,
  };
  const loaded = loadScripts([ERRORLOG_JS, POPUP_JS], globals);
  return { ...loaded, fn: loaded.ctx, chrome, timers, document, el };
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

  it("isJobActive is true for unknown or running jobs only", () => {
    const q = loadPopup();
    q.run("jobProgress.set('run', { status: 'running' }); jobProgress.set('done', { status: 'done' }); jobProgress.set('err', { status: 'error' })");
    assert.equal(q.fn.isJobActive("missing"), true);
    assert.equal(q.fn.isJobActive("run"), true);
    assert.equal(q.fn.isJobActive("done"), false);
    assert.equal(q.fn.isJobActive("err"), false);
  });
});

describe("popup.js UI", () => {
  it("adds the 'popout' class only in pop-out mode", () => {
    assert.equal(loadPopup({ search: "?popout=1" }).document.documentElement.classList.contains("popout"), true);
    assert.equal(loadPopup().document.documentElement.classList.contains("popout"), false);
  });

  it("loads toggles from storage and saves detection changes", () => {
    const p = loadPopup({ storage: { detectionEnabled: false, protectActiveDownloads: false } });
    assert.equal(p.el.detectionEnabled.checked, false);
    assert.equal(p.el.protectActive.checked, false);
    p.el.detectionEnabled.checked = true;
    p.el.detectionEnabled.onchange();
    assert.equal(p.chrome.store.detectionEnabled, true);
    assert.equal(p.el.status.textContent, "Scanning...");
    p.el.detectionEnabled.checked = false;
    p.el.detectionEnabled.onchange();
    assert.equal(p.el.status.textContent, "Detection paused");
    p.el.protectActive.checked = true;
    p.el.protectActive.onchange();
    assert.equal(p.chrome.store.protectActiveDownloads, true);
  });

  it("renders the empty state", () => {
    const p = loadPopup();
    p.fn.render([]);
    assert.equal(p.el.status.textContent, "No HLS streams detected yet.");
    assert.equal(p.el.streams.children.length, 0);
  });

  it("renders streams with buttons reflecting their job state", () => {
    const p = loadPopup();
    p.run(`
      activeJobs.set('u3', 'j3'); jobProgress.set('j3', { status: 'running', current: 3, total: 10 });
      activeJobs.set('u4', 'j4'); jobProgress.set('j4', { status: 'done', output: 'C:/out.mp4' });
      activeJobs.set('u5', 'j5'); jobProgress.set('j5', { status: 'error', error: 'boom' });
    `);
    p.fn.render([
      { url: "u1", title: "First", pageUrl: "https://site/1" },
      { url: "u2", saved: true },
      { url: "u3" },
      { url: "u4" },
      { url: "u5" },
    ]);
    assert.equal(p.el.status.textContent, "5 stream(s) detected");
    const items = p.el.streams.children;
    assert.equal(items.length, 5);

    const buttons = (li) => li.children.at(-1).children;
    const summary = items.map((li) => {
      const [dl, , openFile] = buttons(li);
      return [dl.textContent, dl.disabled, openFile.disabled];
    });
    assert.deepEqual(summary, [
      ["Download via ffmpeg", false, true],
      ["Saved", true, false],
      ["Downloading 3/10...", true, true],
      ["Saved", true, false],
      ["Error: boom", true, true],
    ]);

    // Title only rendered when present.
    assert.equal(items[0].children[0].className, "stream-title");
    assert.equal(items[0].children[0].textContent, "First");
    assert.equal(items[1].children[0].className, "stream-url");
    assert.match(items[0].title, /^Metadata saved in file:/);
  });

  it("'Open page' opens a tab only for known pages", () => {
    const p = loadPopup();
    p.fn.render([{ url: "u1", pageUrl: "https://site/1" }, { url: "u2", pageUrl: "unknown" }]);
    for (const li of p.el.streams.children) li.children.at(-1).children[1].onclick();
    assert.deepEqual(plain(p.chrome.created), [{ url: "https://site/1" }]);
  });

  it("startDownload shows server errors and re-enables the button", () => {
    const p = loadPopup({ onSend: (msg) => (msg.type === "download" ? { error: "server down" } : undefined) });
    const btn = { disabled: false, textContent: "" };
    p.fn.startDownload("u1", btn);
    assert.equal(btn.textContent, "Error: server down");
    assert.equal(btn.disabled, false);
  });

  it("startDownload ignores disabled buttons", () => {
    const p = loadPopup();
    const before = p.chrome.sent.length;
    p.fn.startDownload("u1", { disabled: true });
    assert.equal(p.chrome.sent.length, before);
  });

  it("startDownload starts polling and clears the job once done", () => {
    let progress = { status: "running", current: 1, total: 4 };
    const p = loadPopup({
      onSend: (msg) => {
        if (msg.type === "download") return { job_id: "j1" };
        if (msg.type === "progress") return progress;
        return undefined;
      },
    });
    p.assign("cachedStreams", [{ url: "u1" }]);
    const btn = { disabled: false, textContent: "" };
    p.fn.startDownload("u1", btn);
    assert.equal(p.run("activeJobs.get('u1')"), "j1");
    const intervals = () => p.timers.pending().filter((t) => t.ms === 1000);
    assert.equal(intervals().length, 1);

    p.timers.runAll();
    assert.equal(p.run("jobProgress.get('j1').current"), 1);
    assert.equal(intervals().length, 1);

    progress = { status: "done", output: "C:/x.mp4" };
    p.timers.runAll();
    assert.equal(intervals().length, 0);
    assert.ok(p.chrome.sent.some((m) => m.type === "clearJob" && m.url === "u1"));
  });

  it("restarts polling for jobs stored by the background on load", () => {
    const p = loadPopup({ onSend: (msg) => (msg.type === "getActiveJobs" ? { u9: "j9" } : msg.type === "getStreams" ? [] : undefined) });
    assert.equal(p.run("activeJobs.get('u9')"), "j9");
  });

  it("'Clear' keeps saved streams, finished jobs and (optionally) active downloads", () => {
    const p = loadPopup();
    p.el.protectActive.checked = true;
    p.run(`
      activeJobs.set('u1', 'j1'); jobProgress.set('j1', { status: 'running' });
      activeJobs.set('u2', 'j2'); jobProgress.set('j2', { status: 'done' });
      activeJobs.set('u3', 'j3');
    `);
    p.assign("cachedStreams", [{ url: "u4", saved: true }, { url: "u5" }]);
    p.el.clear.onclick();
    const msg = p.chrome.sent.find((m) => m.type === "clear");
    assert.deepEqual(msg.keepUrls.sort(), ["u1", "u2", "u3", "u4"]);
  });

  it("'Clear' drops active downloads when protection is off", () => {
    const p = loadPopup();
    p.el.protectActive.checked = false;
    p.run(`
      activeJobs.set('u1', 'j1'); jobProgress.set('j1', { status: 'running' });
      activeJobs.set('u2', 'j2'); jobProgress.set('j2', { status: 'error' });
    `);
    p.el.clear.onclick();
    const msg = p.chrome.sent.find((m) => m.type === "clear");
    assert.deepEqual(msg.keepUrls, ["u2"]);
    assert.deepEqual(plain(p.run("[...activeJobs.keys()]")), ["u2"]);
    assert.equal(p.run("jobProgress.has('j1')"), false);
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
  loadScripts([OPTIONS_JS], {
    chrome,
    document,
    console: createFakeConsole(),
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
  });
  return { chrome, timers, el };
}

describe("options.js", () => {
  it("shows the default server URL", () => {
    assert.equal(loadOptions().el.serverUrl.value, "http://127.0.0.1:8765");
  });

  it("shows the stored server URL", () => {
    assert.equal(loadOptions({ storage: { serverUrl: "http://10.0.0.2:9000" } }).el.serverUrl.value, "http://10.0.0.2:9000");
  });

  it("saves the trimmed URL and shows a transient confirmation", () => {
    const o = loadOptions();
    o.el.serverUrl.value = "  http://localhost:1234  ";
    o.el.save.onclick();
    assert.equal(o.chrome.store.serverUrl, "http://localhost:1234");
    assert.equal(o.el.msg.textContent, "Saved.");
    assert.deepEqual(o.timers.pending().map((t) => t.ms), [2000]);
    o.timers.runAll();
    assert.equal(o.el.msg.textContent, "");
  });
});

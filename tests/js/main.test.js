// Unit tests for the Tauri frontend (src/main.js and the ES modules in src/ui/). They are
// evaluated in a node:vm context with stubbed window.__TAURI__, document, localStorage, etc.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  MAIN_JS,
  loadModules,
  plain,
  flush,
  createFakeDocument,
  createFakeTimers,
  createFakeConsole,
  createFakeLocalStorage,
  FakeElement,
} from "./helpers/vm-loader.js";

function loadMain({ storedTheme, throwStorage = false, prefersDark = false, invoke, confirm, prompt, dialog } = {}) {
  const document = createFakeDocument();
  const timers = createFakeTimers();
  const console = createFakeConsole();
  const localStorage = createFakeLocalStorage(storedTheme ? { "mula-theme": storedTheme } : {}, { throwOnAccess: throwStorage });
  const invokeCalls = [];
  const state = {
    invoke: invoke || (() => undefined),
    confirm: confirm || (() => false),
    prompt: prompt || (() => null),
    dialog: dialog || (() => null),
  };
  const confirmCalls = [];
  const mediaListeners = [];
  const windowListeners = {};

  const globals = {
    console,
    document,
    localStorage,
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
    setInterval: timers.setInterval,
    clearInterval: timers.clearInterval,
    __TAURI__: {
      core: {
        invoke(cmd, args) {
          invokeCalls.push({ cmd, args });
          try {
            return Promise.resolve(state.invoke(cmd, args));
          } catch (err) {
            return Promise.reject(err);
          }
        },
      },
      opener: { revealItemInDir: async () => {} },
      dialog: { open: async (opts) => state.dialog(opts) },
      event: { listen: async () => () => {} },
    },
    matchMedia: (media) => ({
      media,
      matches: prefersDark,
      addEventListener: (_type, fn) => mediaListeners.push(fn),
    }),
    addEventListener: (type, fn) => (windowListeners[type] ||= []).push(fn),
    confirm: (msg) => {
      confirmCalls.push(msg);
      return state.confirm(msg);
    },
    prompt: (msg) => state.prompt(msg),
  };

  const { ctx, modules } = loadModules(MAIN_JS, globals);
  return {
    ctx,
    modules,
    // Every module's exports in one object (export names are unique across modules).
    fn: Object.assign({}, ...Object.values(modules)),
    vsd: modules.vsd.state,
    drives: modules.drives.state,
    wc: modules.wallchanger.state,
    document,
    timers,
    console,
    localStorage,
    state,
    confirmCalls,
    mediaListeners,
    windowListeners,
    invokeCalls,
    calls: (cmd) => invokeCalls.filter((c) => c.cmd === cmd),
  };
}

// One shared instance for the pure helpers (they do not touch globals).
const pure = loadMain().fn;

// ── Loading ─────────────────────────────────────────────────────────────────

describe("main.js loading", () => {
  it("evaluates with minimal stubs and registers a DOMContentLoaded handler", () => {
    const m = loadMain();
    assert.equal(m.windowListeners.DOMContentLoaded?.length, 1);
  });

  it("forwards console output to the Rust logger via invoke('log_message')", () => {
    const m = loadMain();
    m.console.calls.length = 0;
    m.ctx.console.warn("disk", { id: 3 }, 7);
    assert.deepEqual(plain(m.calls("log_message").map((c) => c.args)), [{ level: "warn", message: 'disk {"id":3} 7' }]);
    // The original console method is still called.
    assert.equal(m.console.calls.length, 1);
    assert.equal(m.console.calls[0].level, "warn");
  });

  it("maps console.log/info to 'info' and console.error to 'error'", () => {
    const m = loadMain();
    m.ctx.console.log("a");
    m.ctx.console.info("b");
    m.ctx.console.error("c");
    assert.deepEqual(
      m.calls("log_message").map((c) => c.args.level + ":" + c.args.message),
      ["info:a", "info:b", "error:c"]
    );
  });

  it("does not forward empty console messages", () => {
    const m = loadMain();
    m.ctx.console.log("");
    m.ctx.console.log();
    assert.equal(m.calls("log_message").length, 0);
  });
});

// ── Theme ───────────────────────────────────────────────────────────────────

describe("theme helpers", () => {
  it("getStoredTheme returns the stored value or null", () => {
    assert.equal(loadMain({ storedTheme: "dark" }).fn.getStoredTheme(), "dark");
    assert.equal(loadMain().fn.getStoredTheme(), null);
  });

  it("getStoredTheme returns null when localStorage throws", () => {
    assert.equal(loadMain({ throwStorage: true }).fn.getStoredTheme(), null);
  });

  it("applies the effective theme immediately on load", () => {
    assert.equal(loadMain({ storedTheme: "dark" }).document.documentElement.getAttribute("data-theme"), "dark");
    assert.equal(loadMain({ storedTheme: "light", prefersDark: true }).document.documentElement.getAttribute("data-theme"), "light");
    assert.equal(loadMain({ prefersDark: true }).document.documentElement.getAttribute("data-theme"), "dark");
    assert.equal(loadMain().document.documentElement.getAttribute("data-theme"), "light");
    assert.equal(loadMain({ throwStorage: true }).document.documentElement.getAttribute("data-theme"), "light");
  });

  it("applyTheme updates the root attribute, toggle and label", () => {
    const m = loadMain();
    const toggle = m.document.add("app-theme-toggle");
    const label = m.document.add("app-theme-label");
    m.fn.applyTheme("dark");
    assert.equal(m.document.documentElement.getAttribute("data-theme"), "dark");
    assert.equal(toggle.checked, true);
    assert.equal(label.textContent, "Dark mode");
    m.fn.applyTheme("light");
    assert.equal(toggle.checked, false);
    assert.equal(label.textContent, "Light mode");
  });

  it("applyTheme works without the toggle/label elements", () => {
    const m = loadMain();
    m.fn.applyTheme("dark");
    assert.equal(m.document.documentElement.getAttribute("data-theme"), "dark");
  });

  it("initTheme persists the choice when the toggle changes", () => {
    const m = loadMain();
    const toggle = m.document.add("app-theme-toggle");
    m.document.add("app-theme-label");
    m.fn.initTheme();
    toggle.checked = true;
    toggle.dispatch("change");
    assert.equal(m.localStorage.map.get("mula-theme"), "dark");
    assert.equal(m.document.documentElement.getAttribute("data-theme"), "dark");
    toggle.checked = false;
    toggle.dispatch("change");
    assert.equal(m.localStorage.map.get("mula-theme"), "light");
    assert.equal(m.document.documentElement.getAttribute("data-theme"), "light");
  });

  it("initTheme follows OS theme changes only while no explicit choice is stored", () => {
    const m = loadMain();
    m.fn.initTheme();
    assert.equal(m.mediaListeners.length, 1);
    m.mediaListeners[0]({ matches: true });
    assert.equal(m.document.documentElement.getAttribute("data-theme"), "dark");

    m.localStorage.setItem("mula-theme", "light");
    m.fn.applyTheme("light");
    m.mediaListeners[0]({ matches: true });
    assert.equal(m.document.documentElement.getAttribute("data-theme"), "light");
  });
});

// ── Pure helpers ────────────────────────────────────────────────────────────

describe("escapeHtml", () => {
  it("escapes all HTML special characters", () => {
    assert.equal(pure.escapeHtml(`<a href="x">'&'</a>`), "&lt;a href=&quot;x&quot;&gt;&#039;&amp;&#039;&lt;/a&gt;");
  });

  it("escapes & first so entities are not double-decoded", () => {
    assert.equal(pure.escapeHtml("&amp;"), "&amp;amp;");
  });

  it("stringifies non-string input", () => {
    assert.equal(pure.escapeHtml(42), "42");
    assert.equal(pure.escapeHtml(null), "null");
    assert.equal(pure.escapeHtml(undefined), "undefined");
  });

  it("leaves safe text untouched", () => {
    assert.equal(pure.escapeHtml("C:\\Users\\me\\Pictures"), "C:\\Users\\me\\Pictures");
  });
});

describe("ansiToHtml", () => {
  it("passes plain text through", () => {
    assert.equal(pure.ansiToHtml("hello world\n"), "hello world\n");
  });

  it("escapes <, > and &", () => {
    assert.equal(pure.ansiToHtml("<script>&"), "&lt;script&gt;&amp;");
  });

  it("wraps colored text in a span and closes it on reset", () => {
    assert.equal(pure.ansiToHtml("\x1b[31mred\x1b[0m done"), '<span style="color:#e74c3c">red</span> done');
  });

  it("supports bright colors", () => {
    assert.equal(pure.ansiToHtml("\x1b[92mok"), '<span style="color:#55efc4">ok</span>');
  });

  it("renders bold and auto-closes a span left open at the end", () => {
    assert.equal(pure.ansiToHtml("\x1b[1mbold"), '<span style="font-weight:bold">bold</span>');
  });

  it("closes the previous span when a new color starts", () => {
    assert.equal(
      pure.ansiToHtml("\x1b[32ma\x1b[34mb"),
      '<span style="color:#2ecc71">a</span><span style="color:#3498db">b</span>'
    );
  });

  it("ignores unknown codes and empty reset sequences", () => {
    assert.equal(pure.ansiToHtml("\x1b[4mu\x1b[m!"), "u!");
  });

  it("keeps a lone ESC that is not a CSI sequence", () => {
    assert.equal(pure.ansiToHtml("a\x1bb"), "a\x1bb");
  });

  it("does not emit raw escape codes for an unterminated sequence", () => {
    const html = pure.ansiToHtml("abc\x1b[31");
    assert.ok(html.startsWith("abc"));
    assert.ok(!html.includes("\x1b"));
  });

  it(
    "produces balanced spans for combined codes like ESC[1;31m",
    () => {
      const html = pure.ansiToHtml("\x1b[1;31mX\x1b[0m rest");
      const opens = (html.match(/<span/g) || []).length;
      const closes = (html.match(/<\/span>/g) || []).length;
      assert.equal(opens, closes, html);
      assert.ok(html.endsWith(" rest"));
    }
  );
});

describe("wcArgbToHex", () => {
  it("converts unsigned ARGB to #rrggbb", () => {
    assert.equal(pure.wcArgbToHex(0xff112233), "#112233");
  });

  it("converts signed (i32) ARGB values from Rust", () => {
    assert.equal(pure.wcArgbToHex(-15654349), "#112233");
    assert.equal(pure.wcArgbToHex(-1), "#ffffff");
  });

  it("ignores the alpha channel", () => {
    assert.equal(pure.wcArgbToHex(0x00abcdef), "#abcdef");
  });

  it("defaults to black for null/undefined", () => {
    assert.equal(pure.wcArgbToHex(undefined), "#000000");
    assert.equal(pure.wcArgbToHex(null), "#000000");
  });
});

describe("wcHexToArgb", () => {
  it("returns an opaque, signed i32 ARGB value", () => {
    assert.equal(pure.wcHexToArgb("#112233"), -15654349);
    assert.equal(pure.wcHexToArgb("#ffffff"), -1);
    assert.equal(pure.wcHexToArgb("#000000"), -16777216);
  });

  it("accepts input without '#'", () => {
    assert.equal(pure.wcHexToArgb("abcdef"), 0xffabcdef - 0x100000000);
  });

  it("treats unparsable channels as 0", () => {
    assert.equal(pure.wcHexToArgb("#zzzzzz"), -16777216);
  });

  it("round-trips with wcArgbToHex", () => {
    for (const hex of ["#000000", "#ffffff", "#123456", "#fedcba", "#7f8081"]) {
      assert.equal(pure.wcArgbToHex(pure.wcHexToArgb(hex)), hex);
    }
  });
});

describe("errorText", () => {
  it("describes typed command errors", () => {
    assert.equal(pure.errorText({ kind: "smartctl_missing" }), "smartmontools (smartctl) is not installed.");
    assert.equal(pure.errorText({ kind: "elevation_declined" }), "Administrator rights were declined.");
    assert.equal(pure.errorText({ kind: "needs_elevation", message: "Open failed: Access denied" }), "Open failed: Access denied");
    assert.equal(pure.errorText({ kind: "other", message: "boom" }), "boom");
  });

  it("passes plain strings and Error objects through", () => {
    assert.equal(pure.errorText("diskpart failed"), "diskpart failed");
    assert.equal(pure.errorText(new Error("nope")), "nope");
  });
});

describe("formatSelfTestStatus", () => {
  it("lists the status and the log, most recent first", () => {
    const text = pure.formatSelfTestStatus({
      status: "in progress, 90% remaining",
      log: [
        { test: "Short offline", result: "Completed without error", passed: true, hours: 1200 },
        { test: "Extended offline", result: "Aborted by host", passed: null, hours: null },
      ],
    });
    assert.equal(
      text,
      "Self-test status: in progress, 90% remaining\n\n" +
        "Self-test log (most recent first):\n" +
        "  Short offline: Completed without error (at 1200 h)\n" +
        "  Extended offline: Aborted by host"
    );
  });

  it("says so when no self-tests were logged", () => {
    assert.match(pure.formatSelfTestStatus({ status: "No self-test in progress", log: [] }), /No self-tests logged/);
  });
});

describe("wcRenderQueueCard", () => {
  const image = (i, extra = {}) => ({ path: `C:\\pics\\img${i}.jpg`, file_name: `img${i}.jpg`, ...extra });

  it("renders the images it gets plus a '+N more' line for the rest of image_count", () => {
    // The backend sends at most 30 images per queue (QUEUE_PREVIEW_LIMIT) plus the total.
    const images = Array.from({ length: 30 }, (_, i) => image(i));
    const html = pure.wcRenderQueueCard({ label: "Monitor 1", image_count: 35, rotation_mode: "Sequential", images });
    assert.equal((html.match(/<li class="wallchanger-queue-item/g) || []).length, 30);
    assert.match(html, /\+5 more/);
    assert.match(html, /35 image\(s\) &middot; Sequential/);
    assert.ok(!html.includes("chosen randomly"));
  });

  it("adds badges and the random-rotation hint", () => {
    const images = [image(0, { is_next: true }), image(1, { is_last_shown: true }), image(2, { last_resort: true })];
    const html = pure.wcRenderQueueCard({ label: "All monitors", image_count: 3, rotation_mode: "Random", images });
    assert.match(html, /wallchanger-queue-item next"/);
    assert.match(html, /wallchanger-badge next">Next</);
    assert.match(html, /wallchanger-badge last">Last shown</);
    assert.match(html, /wallchanger-queue-item last-resort"/);
    assert.match(html, /Next picture is chosen randomly/);
    assert.ok(!html.includes("more</li>"));
  });

  it(
    "escapes file names and paths",
    () => {
      const html = pure.wcRenderQueueCard({
        label: "Monitor 1",
        image_count: 1,
        rotation_mode: "Sequential",
        images: [{ path: "C:\\pics\\a&lt;b.jpg", file_name: "a&lt;b.jpg" }],
      });
      assert.match(html, /a&amp;lt;b\.jpg/);
    }
  );
});

describe("wcRenderMonitorPreview", () => {
  const base = {
    monitor_index: 2,
    width: 2560,
    height: 1440,
    image_width: 3840,
    image_height: 2160,
    preview_data_url: "data:image/png;base64,AAAA",
    fit: "Fill",
    wallpaper_path: "C:\\pics\\x.jpg",
  };

  it("renders monitor and image dimensions", () => {
    const html = pure.wcRenderMonitorPreview(base);
    assert.match(html, /Monitor 2/);
    assert.match(html, /2560×1440/);
    assert.match(html, /aspect-ratio: 2560 \/ 1440/);
    assert.match(html, /Image: 3840×2160/);
    assert.match(html, /src="data:image\/png;base64,AAAA"/);
  });

  it("uses placeholders when dimensions are missing", () => {
    const html = pure.wcRenderMonitorPreview({ ...base, width: 0, height: 0, image_width: 0 });
    assert.match(html, /aspect-ratio: 16 \/ 10/);
    assert.match(html, /Image: —/);
  });
});

// ── DOM-light behavior ──────────────────────────────────────────────────────

describe("appendLog", () => {
  it("appends converted ANSI output and scrolls to the bottom", () => {
    const m = loadMain();
    const content = m.document.add("vsd-log-content");
    const log = m.document.add("vsd-log", { scrollHeight: 500 });
    m.fn.appendLog("a<b");
    m.fn.appendLog("\x1b[31mX");
    assert.equal(
      content.innerHTML,
      '<span class="vsd-log-chunk">a&lt;b</span>' +
        '<span class="vsd-log-chunk"><span style="color:#e74c3c">X</span></span>'
    );
    assert.equal(log.scrollTop, 500);
  });

  it("keeps only the newest chunks so the log view stays bounded", () => {
    const m = loadMain();
    const content = m.document.add("vsd-log-content");
    m.document.add("vsd-log", { scrollHeight: 0 });
    const max = m.fn.MAX_LOG_CHUNKS;
    for (let i = 0; i < max + 3; i++) m.fn.appendLog(`line ${i}\n`);
    assert.equal(content.childElementCount, max);
    assert.ok(content.innerHTML.startsWith('<span class="vsd-log-chunk">line 3\n</span>'), content.innerHTML.slice(0, 80));
    assert.ok(content.innerHTML.endsWith(`line ${max + 2}\n</span>`));
  });
});

describe("invokeSmart", () => {
  it("passes through results and unrelated errors", async () => {
    const m = loadMain();
    m.state.invoke = (async (cmd) => (cmd === "get_drive_smart" ? "data" : Promise.reject("boom")));
    assert.equal(await m.fn.invokeSmart("get_drive_smart", { id: "x" }), "data");
    await assert.rejects(m.fn.invokeSmart("save_smart_snapshot", {}), (e) => e === "boom");
  });

  it("asks before installing smartmontools and retries after installing", async () => {
    const m = loadMain();
    const calls = [];
    let installed = false;
    m.state.invoke = (async (cmd) => {
      calls.push(cmd);
      if (cmd === "install_smartmontools") {
        installed = true;
        return "C:/Program Files/smartmontools/bin/smartctl.exe";
      }
      if (!installed) throw { kind: "smartctl_missing" };
      return "smart data";
    });
    m.state.confirm = (() => true);
    assert.equal(await m.fn.invokeSmart("get_drive_smart", { id: "x" }), "smart data");
    assert.deepEqual(calls, ["get_drive_smart", "install_smartmontools", "get_drive_smart"]);
  });

  it("never installs when the user declines", async () => {
    const m = loadMain();
    const calls = [];
    m.state.invoke = (async (cmd) => {
      calls.push(cmd);
      throw { kind: "smartctl_missing" };
    });
    m.state.confirm = (() => false);
    await assert.rejects(m.fn.invokeSmart("get_drive_smart", { id: "x" }), /smartmontools is not installed/);
    assert.deepEqual(calls, ["get_drive_smart"]);
  });
});

describe("VSD server panel", () => {
  it("only reflects the autostart setting; the backend starts the server", async () => {
    const m = loadMain({ invoke: (cmd) => (cmd === "vsd_get_autostart" ? true : undefined) });
    const checkbox = m.document.add("vsd-autostart");
    await m.fn.loadAutostart();
    assert.equal(checkbox.checked, true);
    assert.equal(m.calls("vsd_start").length, 0);
  });

  it("shows the backlog, then live lines, without duplicates or gaps", async () => {
    let resolveBacklog;
    const m = loadMain({
      invoke: (cmd) => (cmd === "vsd_get_logs" ? new Promise((r) => (resolveBacklog = r)) : undefined),
    });
    const content = m.document.add("vsd-log-content");
    m.document.add("vsd-log");

    const loading = m.fn.loadVsdLogBacklog();
    // Live lines arrive while the backlog request is still pending; line 2 is also in the backlog.
    m.fn.onVsdLogLine({ seq: 2, text: "Serving on 8765\n" });
    m.fn.onVsdLogLine({ seq: 3, text: "GET /health\n" });
    resolveBacklog([{ seq: 1, text: "[Dependencies OK]\n" }, { seq: 2, text: "Serving on 8765\n" }]);
    await loading;
    m.fn.onVsdLogLine({ seq: 3, text: "GET /health\n" }); // a repeat is ignored
    m.fn.onVsdLogLine({ seq: 4, text: "POST /download\n" });

    const text = content.innerHTML.replace(/<[^>]+>/g, "");
    assert.equal(text, "[Dependencies OK]\nServing on 8765\nGET /health\nPOST /download\n");
  });

  it("re-checks the real state when starting fails", async () => {
    const m = loadMain({
      invoke: (cmd) => {
        if (cmd === "vsd_start") throw "Could not find VSD server.py";
        if (cmd === "vsd_is_running") return false;
      },
    });
    const btn = m.document.add("vsd-toggle", { className: "stopped" });
    m.document.add("vsd-log-content");
    m.document.add("vsd-log");
    await m.fn.toggleVsd();
    await flush();
    assert.equal(m.calls("vsd_is_running").length, 1);
    assert.equal(m.vsd.running, false);
    assert.equal(btn.disabled, false);
  });
});

describe("updateVsdUi / wcUpdateToggleUi / wcShowMessage", () => {
  it("updateVsdUi toggles the VSD button state", () => {
    const m = loadMain();
    const btn = m.document.add("vsd-toggle", { className: "stopped" });
    m.fn.updateVsdUi(true);
    assert.equal(btn.className, "running");
    assert.equal(btn.querySelector(".toggle-label").textContent, "Stop");
    assert.equal(btn.title, "Stop VSD Server");
    assert.equal(m.vsd.running, true);
    m.fn.updateVsdUi(false);
    assert.equal(btn.className, "stopped");
    assert.equal(btn.querySelector(".toggle-label").textContent, "Start");
    assert.equal(m.vsd.running, false);
  });

  it("wcUpdateToggleUi reflects the service state", () => {
    const m = loadMain();
    const btn = m.document.add("wc-toggle");
    const label = m.document.add("wc-toggle-label");
    const status = m.document.add("wc-status");
    m.fn.wcUpdateToggleUi(true);
    assert.deepEqual([btn.className, label.textContent, status.textContent], ["running", "Stop", "running"]);
    m.fn.wcUpdateToggleUi(false);
    assert.deepEqual([btn.className, label.textContent, status.textContent], ["stopped", "Start", "stopped"]);
  });

  it("wcRefreshPreviews waits while the window is hidden and reloads once it is shown", async () => {
    const m = loadMain({ invoke: () => [] });
    m.document.add("wc-monitor-preview");
    m.document.add("wc-queues");
    m.document.hidden = true;
    m.fn.wcRefreshPreviews();
    await flush();
    assert.equal(m.invokeCalls.length, 0);
    assert.equal(m.wc.previewsStale, true);

    m.document.hidden = false;
    m.fn.wcRefreshPreviews();
    await flush();
    assert.deepEqual(m.invokeCalls.map((c) => c.cmd).sort(), ["wc_get_monitor_preview", "wc_get_queue_preview"]);
    assert.equal(m.wc.previewsStale, false);
  });

  it("wcRenderSettings fills every form field from the settings", () => {
    const m = loadMain();
    const interval = m.document.add("wc-interval");
    const color = m.document.add("wc-bg-color");
    const separate = m.document.add("wc-separate-queues");
    const apiKey = m.document.add("wc-wallhaven-api-key");
    m.document.add("wc-scaling");
    m.document.add("wc-overlay-enabled");
    (m.wc.settings = {
      interval_minutes: 15,
      background_color_argb: 0xff102030 | 0,
      use_separate_monitor_queues: true,
      wallhaven_api_key: null,
      scaling_mode: "Fill",
    });
    m.fn.wcRenderSettings();
    assert.equal(interval.value, 15);
    assert.equal(color.value, "#102030");
    assert.equal(separate.checked, true);
    assert.equal(apiKey.value, "");
  });

  it("wcShowMessage sets text and kind class", () => {
    const m = loadMain();
    const el = m.document.add("wc-message");
    m.fn.wcShowMessage("Saved", "success");
    assert.equal(el.textContent, "Saved");
    assert.equal(el.className, "wallchanger-message success");
    m.fn.wcShowMessage("Plain");
    assert.equal(el.className, "wallchanger-message");
  });
});

describe("settings patches", () => {
  function setupInputs(m, overrides = {}) {
    const values = {
      "wc-interval": "15",
      "wc-max-level": "7",
      "wc-wallhaven-api-key": "secret-key",
      "wc-rotation": "Random",
      "wc-scaling": "FitInsideScreen",
      "wc-bg-color": "#112233",
      "wc-overlay-mode": "FileName",
      "wc-overlay-font-size": "30",
      "wc-overlay-color": "#ffffff",
      "wc-overlay-offset-x": "10",
      "wc-overlay-offset-y": "-20",
      "wc-fade-duration": "500",
      "wc-fade-steps": "8",
      ...overrides,
    };
    for (const [id, value] of Object.entries(values)) m.document.add(id, { value });
    for (const id of [
      "wc-separate-queues",
      "wc-unique-queues",
      "wc-stop-slideshow",
      "wc-one-monitor",
      "wc-overlay-enabled",
      "wc-overlay-backdrop",
      "wc-fade-enabled",
    ]) {
      m.document.add(id, { checked: id === "wc-overlay-enabled" || id === "wc-fade-enabled" });
    }
    for (const id of ["wc-sources", "wc-remove-source", "wc-move-up", "wc-move-down", "wc-message"]) m.document.add(id);
  }

  const versioned = (revision, settings) => ({ revision, settings });

  it("turns one changed form field into a patch with only that setting", () => {
    const m = loadMain();
    setupInputs(m, { "wc-interval": "99999", "wc-overlay-offset-x": "abc" });
    assert.deepEqual(plain(m.fn.wcFieldPatch("wc-interval")), { interval_minutes: 99999 }, "no clamping in the UI");
    assert.deepEqual(plain(m.fn.wcFieldPatch("wc-bg-color")), { background_color_argb: -15654349 });
    assert.deepEqual(plain(m.fn.wcFieldPatch("wc-overlay-enabled")), { show_picture_name_overlay: true });
    assert.deepEqual(plain(m.fn.wcFieldPatch("wc-wallhaven-api-key")), { wallhaven_api_key: "secret-key" });
    assert.equal(m.fn.wcFieldPatch("wc-overlay-offset-x"), null, "an unusable number sends nothing");
  });

  it("does nothing while settings are not loaded", () => {
    const m = loadMain();
    m.fn.wcQueuePatch({ interval_minutes: 5 });
    assert.equal(m.wc.settings, null);
    assert.equal(m.timers.pending().length, 0);
  });

  it("batches edits into one debounced wc_update_settings call and adopts the normalized result", async () => {
    const m = loadMain({
      invoke: (cmd, args) =>
        cmd === "wc_update_settings" ? versioned(1, { source_folders: [], ...args.patch, interval_minutes: 1440 }) : undefined,
    });
    setupInputs(m);
    m.fn.wcApplySettings(versioned(0, { source_folders: [], interval_minutes: 30, maximum_source_level: 10 }));
    m.fn.wcQueuePatch({ interval_minutes: 99999 });
    m.fn.wcQueuePatch({ maximum_source_level: 3 });
    assert.deepEqual(m.timers.pending().map((t) => t.ms), [300]);
    assert.equal(m.wc.settings.interval_minutes, 99999, "edits show immediately");

    m.timers.runAll();
    await flush();
    const updates = m.calls("wc_update_settings");
    assert.equal(updates.length, 1);
    assert.deepEqual(plain(updates[0].args.patch), { interval_minutes: 99999, maximum_source_level: 3 });
    assert.equal(m.wc.settings.interval_minutes, 1440, "the backend's normalized value wins");
    assert.equal(m.document.getElementById("wc-interval").value, 1440, "and is rendered");
  });

  it("keeps unsent edits when settings arrive from the backend", () => {
    const m = loadMain();
    setupInputs(m);
    m.fn.wcApplySettings(versioned(0, { source_folders: [], interval_minutes: 30, maximum_source_level: 10 }));
    m.fn.wcQueuePatch({ interval_minutes: 45 });
    // e.g. the tray changed the max level while the interval edit is still pending
    m.fn.wcApplySettings(versioned(1, { source_folders: [], interval_minutes: 30, maximum_source_level: 2 }));
    const s = plain(m.wc.settings);
    assert.equal(s.interval_minutes, 45);
    assert.equal(s.maximum_source_level, 2);
  });

  it("a pending edit of one field never sends back other fields (no reverting tray changes)", async () => {
    const m = loadMain({ invoke: (cmd, args) => (cmd === "wc_update_settings" ? versioned(2, { source_folders: [], ...args.patch }) : undefined) });
    setupInputs(m);
    m.fn.wcApplySettings(versioned(0, { source_folders: [], interval_minutes: 30, maximum_source_level: 10 }));
    // The user edits the interval; the change handler queues that field's patch.
    m.document.getElementById("wc-interval").value = "45";
    m.fn.wcQueuePatch(m.fn.wcFieldPatch("wc-interval"));
    m.fn.wcApplySettings(versioned(1, { source_folders: [], interval_minutes: 30, maximum_source_level: 3 }));
    m.timers.runAll();
    await flush();
    const [update] = m.calls("wc_update_settings");
    assert.deepEqual(plain(update.args.patch), { interval_minutes: 45 });
  });

  it("ignores responses and events older than the settings shown", () => {
    const m = loadMain();
    setupInputs(m);
    m.fn.wcApplySettings(versioned(5, { source_folders: [], interval_minutes: 30, maximum_source_level: 3 }));
    m.fn.wcApplySettings(versioned(4, { source_folders: [], interval_minutes: 30, maximum_source_level: 10 }));
    assert.equal(m.wc.settings.maximum_source_level, 3);
    assert.equal(m.wc.revision, 5);
  });

  it("flushes pending edits before changing the wallpaper", async () => {
    const m = loadMain({
      invoke: (cmd, args) =>
        cmd === "wc_update_settings" ? versioned(1, { source_folders: [], ...args.patch }) : "Applied 1 wallpaper(s): a.jpg",
    });
    setupInputs(m);
    m.fn.wcApplySettings(versioned(0, { source_folders: [], interval_minutes: 30 }));
    m.fn.wcQueuePatch({ interval_minutes: 5 });
    await m.fn.wcChangeNow();
    assert.deepEqual(m.invokeCalls.map((c) => c.cmd), ["wc_update_settings", "wc_change_now"]);
    assert.equal(m.timers.pending().length, 0);
  });

  it("reloads the settings when an update is rejected", async () => {
    const m = loadMain({
      invoke: (cmd) => {
        if (cmd === "wc_update_settings") throw "Unknown setting: x";
        if (cmd === "wc_get_settings") return versioned(0, { source_folders: [], interval_minutes: 30 });
      },
    });
    setupInputs(m);
    m.fn.wcApplySettings(versioned(0, { source_folders: [], interval_minutes: 30 }));
    m.fn.wcQueuePatch({ x: 1 });
    await m.fn.wcFlushPatch();
    assert.match(m.document.getElementById("wc-message").textContent, /Unknown setting: x/);
    assert.equal(m.calls("wc_get_settings").length, 1);
  });
});

describe("wcRenderSources", () => {
  function setup(sources, selected = -1) {
    const m = loadMain();
    const container = m.document.add("wc-sources");
    const remove = m.document.add("wc-remove-source");
    const up = m.document.add("wc-move-up");
    const down = m.document.add("wc-move-down");
    (m.wc.settings = { source_folders: sources });
    (m.wc.selectedSourceIndex = selected);
    m.fn.wcRenderSources();
    return { m, container, remove, up, down, settings: () => plain(m.wc.settings) };
  }

  const folder = { path: 'C:\\Pics "x" <y> & z', enabled: true, level: 5, kind: "folder", include_subfolders: true };
  const url = {
    path: "https://wallhaven.cc/search?q=a&b=1",
    enabled: false,
    level: 3,
    kind: "wallhaven",
    page_limit: 0,
    purity: { sfw: false, sketchy: true, nsfw: true },
  };

  it("renders one row per source with escaped paths", () => {
    const { container } = setup([folder, url]);
    assert.equal(container.children.length, 2);
    const [row0, row1] = container.children;
    assert.ok(row0.innerHTML.includes('value="C:\\Pics &quot;x&quot; &lt;y&gt; &amp; z"'), row0.innerHTML);
    assert.ok(!row0.innerHTML.includes('"x"'));
    assert.ok(row1.innerHTML.includes('value="https://wallhaven.cc/search?q=a&amp;b=1"'), row1.innerHTML);
  });

  it("renders folder rows read-only with a subfolder checkbox", () => {
    const { container } = setup([folder]);
    const row = container.children[0];
    assert.match(row.innerHTML, /readonly/);
    assert.match(row.innerHTML, /Subfolders/);
    assert.ok(!row.innerHTML.includes("wallchanger-purity"));
    assert.equal(row.style.gridTemplateColumns, "24px 1fr 60px 110px");
    assert.match(row.innerHTML, /<input type="checkbox" checked title="Enabled">/);
  });

  it("renders URL rows with page limit and purity checkboxes", () => {
    const { container } = setup([url]);
    const row = container.children[0];
    assert.ok(!row.innerHTML.includes("readonly"));
    assert.match(row.innerHTML, /value="0" title="Page limit/);
    assert.match(row.innerHTML, /data-purity="sfw" >/);
    assert.match(row.innerHTML, /data-purity="sketchy" checked/);
    assert.match(row.innerHTML, /data-purity="nsfw" checked/);
    assert.match(row.innerHTML, /<input type="checkbox"  title="Enabled">/);
    assert.equal(row.style.gridTemplateColumns, "24px 1fr 60px 80px 180px");
  });

  it("marks the selected row and updates the toolbar", () => {
    const { container, remove, up, down } = setup([folder, url], 1);
    assert.ok(!container.children[0].classList.contains("selected"));
    assert.ok(container.children[1].classList.contains("selected"));
    assert.equal(remove.disabled, false);
    assert.equal(up.disabled, false);
    assert.equal(down.disabled, true);
  });

  it("disables the toolbar without a selection", () => {
    const { remove, up, down } = setup([folder, url], -1);
    assert.deepEqual([remove.disabled, up.disabled, down.disabled], [true, true, true]);
  });

  it("selects a row on click, but not when an input was clicked", () => {
    const { m, container } = setup([folder, url]);
    container.children[1].dispatch("click", { target: { tagName: "INPUT" } });
    assert.equal(m.wc.selectedSourceIndex, -1);
    container.children[1].dispatch("click", { target: { tagName: "DIV" } });
    assert.equal(m.wc.selectedSourceIndex, 1);
    assert.ok(container.children[1].classList.contains("selected"));
  });

  it("editing a Wallhaven path sends it as a source_folders patch", async () => {
    const { m, container, settings } = setup([{ ...url }]);
    m.state.invoke = () => new Promise(() => {}); // only the request matters here
    const pathInput = container.children[0].querySelectorAll('input[type="text"], input[type="number"]')[0];
    pathInput.value = "  https://wallhaven.cc/latest  ";
    pathInput.dispatch("change");
    assert.equal(settings().source_folders[0].path, "https://wallhaven.cc/latest");
    assert.equal(settings().source_folders[0].kind, "wallhaven");

    assert.deepEqual(m.timers.pending().map((t) => t.ms), [300]);
    m.timers.runAll();
    await flush();
    const updates = m.calls("wc_update_settings");
    assert.equal(updates.length, 1);
    assert.equal(updates[0].args.patch.source_folders[0].path, "https://wallhaven.cc/latest");
  });

  it("debounces autosave to a single pending timer", () => {
    const { m, container } = setup([{ ...folder }]);
    const checkbox = container.children[0].querySelector('input[type="checkbox"]');
    checkbox.checked = false;
    checkbox.dispatch("change");
    checkbox.checked = true;
    checkbox.dispatch("change");
    assert.equal(m.timers.pending().length, 1);
    assert.equal(plain(m.wc.settings).source_folders[0].enabled, true);
  });

  it("sends numeric edits as entered and ignores non-numbers (the backend clamps)", () => {
    const { container, settings } = setup([{ ...url }]);
    const [, levelInput, pageInput] = container.children[0].querySelectorAll('input[type="text"], input[type="number"]');
    for (const [input, expected] of [["99", 99], ["abc", 99], ["7", 7]]) {
      levelInput.value = input;
      levelInput.dispatch("change");
      assert.equal(settings().source_folders[0].level, expected, input);
    }
    for (const [input, expected] of [["5", 5], ["-2", -2], ["abc", -2]]) {
      pageInput.value = input;
      pageInput.dispatch("change");
      assert.equal(settings().source_folders[0].page_limit, expected, input);
    }
  });

  it("stores the purity checkboxes as an object", () => {
    const { container, settings } = setup([{ ...url }]);
    const purity = container.children[0].querySelectorAll('.wallchanger-purity input[type="checkbox"]');
    ["sfw", "sketchy", "nsfw"].forEach((name, i) => (purity[i].dataset.purity = name));
    [true, false, true].forEach((v, i) => (purity[i].checked = v));
    purity[0].dispatch("change");
    assert.deepEqual(settings().source_folders[0].purity, { sfw: true, sketchy: false, nsfw: true });
  });

  it("toggles include_subfolders for folder rows", () => {
    const { container, settings } = setup([{ ...folder }]);
    const sub = container.children[0].querySelector('label.wallchanger-option input[type="checkbox"]');
    sub.checked = false;
    sub.dispatch("change");
    assert.equal(settings().source_folders[0].include_subfolders, false);
  });
});

describe("wcRemoveSource / wcMoveSource", () => {
  function setup(paths, selected) {
    const m = loadMain();
    for (const id of ["wc-sources", "wc-remove-source", "wc-move-up", "wc-move-down"]) m.document.add(id);
    (m.wc.settings = { source_folders: paths.map((path) => ({ path, enabled: true, level: 5, kind: "folder", include_subfolders: true })) });
    (m.wc.selectedSourceIndex = selected);
    return {
      m,
      paths: () => plain(m.wc.settings).source_folders.map((s) => s.path),
      selected: () => m.wc.selectedSourceIndex,
    };
  }

  it("removes the selected source and keeps a valid selection", () => {
    const t = setup(["a", "b", "c"], 2);
    t.m.fn.wcRemoveSource();
    assert.deepEqual(t.paths(), ["a", "b"]);
    assert.equal(t.selected(), 1);
    assert.equal(t.m.timers.pending().length, 1);
  });

  it("clears the selection when the last source is removed", () => {
    const t = setup(["only"], 0);
    t.m.fn.wcRemoveSource();
    assert.deepEqual(t.paths(), []);
    assert.equal(t.selected(), -1);
  });

  it("does nothing without a selection", () => {
    const t = setup(["a", "b"], -1);
    t.m.fn.wcRemoveSource();
    t.m.fn.wcMoveSource(1);
    assert.deepEqual(t.paths(), ["a", "b"]);
    assert.equal(t.m.timers.pending().length, 0);
  });

  it("moves the selected source and follows it", () => {
    const t = setup(["a", "b", "c"], 0);
    t.m.fn.wcMoveSource(1);
    assert.deepEqual(t.paths(), ["b", "a", "c"]);
    assert.equal(t.selected(), 1);
    t.m.fn.wcMoveSource(1);
    assert.deepEqual(t.paths(), ["b", "c", "a"]);
    assert.equal(t.selected(), 2);
  });

  it("does not move past either end", () => {
    const t = setup(["a", "b"], 0);
    t.m.fn.wcMoveSource(-1);
    assert.deepEqual(t.paths(), ["a", "b"]);
    (t.m.wc.selectedSourceIndex = 1);
    t.m.fn.wcMoveSource(1);
    assert.deepEqual(t.paths(), ["a", "b"]);
  });
});

describe("wcAddWallhavenUrl / wcAddFolder", () => {
  function setup(opts) {
    const m = loadMain(opts);
    for (const id of ["wc-sources", "wc-remove-source", "wc-move-up", "wc-move-down", "wc-message"]) m.document.add(id);
    (m.wc.settings = { source_folders: [] });
    return m;
  }

  it("rejects non-Wallhaven URLs with an error message", async () => {
    const m = setup({ prompt: () => "https://example.com/x" });
    await m.fn.wcAddWallhavenUrl();
    assert.deepEqual(plain(m.wc.settings).source_folders, []);
    const msg = m.document.getElementById("wc-message");
    assert.equal(msg.textContent, "Please enter a valid Wallhaven URL.");
    assert.equal(msg.className, "wallchanger-message error");
  });

  it("ignores a cancelled prompt", async () => {
    const m = setup({ prompt: () => null });
    await m.fn.wcAddWallhavenUrl();
    assert.deepEqual(plain(m.wc.settings).source_folders, []);
    assert.equal(m.document.getElementById("wc-message").textContent, "");
  });

  it("adds a trimmed Wallhaven URL source with URL defaults", async () => {
    const m = setup({ prompt: () => "  https://wallhaven.cc/search?q=cats  " });
    await m.fn.wcAddWallhavenUrl();
    assert.deepEqual(plain(m.wc.settings).source_folders, [
      {
        path: "https://wallhaven.cc/search?q=cats",
        enabled: true,
        level: 5,
        kind: "wallhaven",
        page_limit: 1,
        purity: { sfw: true, sketchy: true, nsfw: false },
      },
    ]);
  });

  it("adds a picked folder with subfolders enabled", async () => {
    const m = setup({ dialog: () => "D:\\Wallpapers" });
    await m.fn.wcAddFolder();
    const [s] = plain(m.wc.settings).source_folders;
    assert.equal(s.path, "D:\\Wallpapers");
    assert.equal(s.kind, "folder");
    assert.equal(s.include_subfolders, true);
    assert.equal(s.level, 5);
  });

  it("does nothing when the folder dialog is cancelled", async () => {
    const m = setup({ dialog: () => null });
    await m.fn.wcAddFolder();
    assert.deepEqual(plain(m.wc.settings).source_folders, []);
  });
});

describe("Drive Test", () => {
  const DRIVE = {
    device_id: "PD1",
    vendor: "Samsung",
    model: "SSD <980>",
    serial: "S123",
    bus_type: "NVMe",
    connection_speed: "PCIe 3.0 x4",
    media_type: "SSD",
    interface_type: "SCSI",
    size_text: "1 TB",
    size: 1000204886016,
    partitions: 2,
    drive_letters: ["D:", "E:"],
    mount_points: [],
    health_status: "Healthy",
    smart_capable: "Yes",
    trim_capable: "Yes",
    status: "OK",
    firmware: "1B2QGXA7",
    pnp_device_id: "",
    is_system: false,
  };

  function setup(drives, selectedId, opts) {
    const m = loadMain(opts);
    const el = {};
    for (const id of [
      "drive-select",
      "drive-empty",
      "drive-details",
      "smart-toggle",
      "smart-content",
      "smart-data",
      "smart-admin",
      "smart-test",
      "smart-run-test",
      "smart-check-status",
      "smart-test-status",
      "drive-format",
      "drive-format-status",
      "drive-format-btn",
    ]) {
      el[id] = m.document.add(id);
    }
    el.tbody = m.document.setQuery("#drive-info-table tbody", new FakeElement());
    el["drive-select"].value = selectedId;
    (m.drives.drives = drives);
    return { m, el };
  }

  it("onDriveSelect shows escaped drive details and enables formatting for data disks", () => {
    const { m, el } = setup([DRIVE], "PD1");
    m.fn.onDriveSelect();
    assert.ok(el["drive-empty"].classList.contains("hidden"));
    assert.ok(!el["drive-details"].classList.contains("hidden"));
    assert.match(el.tbody.innerHTML, /<td class="label">Model<\/td><td class="value">SSD &lt;980&gt;<\/td>/);
    assert.match(el.tbody.innerHTML, /Drive letters<\/td><td class="value">D:, E:</);
    assert.match(el.tbody.innerHTML, /Mount points<\/td><td class="value">None</);
    assert.match(el.tbody.innerHTML, /PNP ID<\/td><td class="value">N\/A</);
    assert.match(el.tbody.innerHTML, /Partitions<\/td><td class="value">2</);
    assert.equal(el["drive-format-btn"].disabled, false);
    assert.equal(el["drive-format-status"].textContent, "");
    assert.ok(!el["smart-test"].classList.contains("hidden"));
    assert.ok(!el["drive-format"].classList.contains("hidden"));
    assert.equal(el["smart-data"].textContent, "Select a drive and expand to load SMART data.");
    assert.equal(el["smart-toggle"].getAttribute("aria-expanded"), "false");
  });

  it("onDriveSelect disables the format button for the system disk", () => {
    const { m, el } = setup([{ ...DRIVE, is_system: true }], "PD1");
    m.fn.onDriveSelect();
    assert.equal(el["drive-format-btn"].disabled, true);
    assert.match(el["drive-format-status"].textContent, /cannot be formatted/);
  });

  it("onDriveSelect with no selection hides details and the format section", () => {
    const { m, el } = setup([DRIVE], "");
    el.tbody.innerHTML = "<tr></tr>";
    el["drive-format-status"].textContent = "old";
    m.fn.onDriveSelect();
    assert.ok(!el["drive-empty"].classList.contains("hidden"));
    assert.ok(el["drive-details"].classList.contains("hidden"));
    assert.ok(el["smart-test"].classList.contains("hidden"));
    assert.ok(el["drive-format"].classList.contains("hidden"));
    assert.equal(el["drive-format-status"].textContent, "");
    assert.equal(el.tbody.innerHTML, "");
  });

  it("onDriveSelect with an unknown id shows the empty state", () => {
    const { m, el } = setup([DRIVE], "PD9");
    m.fn.onDriveSelect();
    assert.ok(!el["drive-empty"].classList.contains("hidden"));
    assert.ok(el["drive-details"].classList.contains("hidden"));
  });

  it("onFormatDrive refuses the system disk without asking or invoking", async () => {
    const { m } = setup([{ ...DRIVE, is_system: true }], "PD1", { confirm: () => true });
    await m.fn.onFormatDrive();
    assert.equal(m.confirmCalls.length, 0);
    assert.equal(m.calls("format_drive").length, 0);
  });

  it("onFormatDrive asks for confirmation and aborts when declined", async () => {
    const { m } = setup([DRIVE], "PD1", { confirm: () => false });
    await m.fn.onFormatDrive();
    assert.equal(m.confirmCalls.length, 1);
    assert.match(m.confirmCalls[0], /Drive: Samsung SSD <980> S123/);
    assert.equal(m.calls("format_drive").length, 0);
  });

  it("onFormatDrive reports backend errors", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      confirm: () => true,
      invoke: (cmd) => {
        if (cmd === "format_drive") throw "diskpart failed";
      },
    });
    await m.fn.onFormatDrive();
    assert.equal(m.calls("format_drive").length, 1);
    assert.equal(el["drive-format-status"].textContent, "Error: diskpart failed");
    assert.equal(el["drive-format-status"].className, "drive-format-status error");
    assert.equal(el["drive-format-btn"].disabled, false);
    assert.equal(m.drives.formatTimer, null);
  });

  it("offers the admin retry only when smartctl needs elevation", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: (cmd, args) => {
        if (cmd === "get_drive_smart" && !args.elevated) throw { kind: "needs_elevation", message: "Open failed: Access denied" };
        if (cmd === "get_drive_smart") return "SMART overall-health: PASSED";
      },
    });
    m.document.add("smart-status");
    el["smart-admin"].classList.add("hidden");
    await m.fn.loadSmartData("PD1", false);
    assert.equal(el["smart-data"].textContent, "Error: Open failed: Access denied");
    assert.ok(!el["smart-admin"].classList.contains("hidden"));

    await m.fn.onSmartRetryAdmin();
    assert.deepEqual(plain(m.calls("get_drive_smart").map((c) => c.args)), [
      { id: "PD1", elevated: false },
      { id: "PD1", elevated: true },
    ]);
    assert.equal(el["smart-data"].textContent, "SMART overall-health: PASSED");
    assert.ok(el["smart-admin"].classList.contains("hidden"));
  });

  it("other SMART errors don't offer the admin retry", async () => {
    const { m, el } = setup([DRIVE], "PD1", { invoke: () => { throw { kind: "other", message: "boom" }; } });
    m.document.add("smart-status");
    el["smart-admin"].classList.add("hidden");
    await m.fn.loadSmartData("PD1", false);
    assert.ok(el["smart-admin"].classList.contains("hidden"));
  });

  it("pollTestStatus stops when the selected drive changed", async () => {
    const { m } = setup([DRIVE], "PD2");
    await m.fn.pollTestStatus("PD1");
    assert.equal(m.calls("get_drive_self_test_status").length, 0);
  });

  it("pollTestStatus shows progress while a test runs", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: () => ({ in_progress: true, percent_remaining: 40, status: "Extended self-test in progress", failed: false, log: [] }),
    });
    await m.fn.pollTestStatus("PD1");
    assert.match(el["smart-test-status"].textContent, /^Monitoring self-test progress\.\.\. \(40% remaining, updates every 10s\)/);
    assert.equal(el["smart-test-status"].className, "smart-test-status");
  });

  it("pollTestStatus marks success after two polls without a running test", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: () => ({ in_progress: false, percent_remaining: null, status: "No self-test in progress", failed: false, log: [] }),
    });
    await m.fn.pollTestStatus("PD1");
    assert.match(el["smart-test-status"].textContent, /^Self-test finished/);
    assert.equal(el["smart-test-status"].className, "smart-test-status");
    await m.fn.pollTestStatus("PD1");
    assert.equal(el["smart-test-status"].className, "smart-test-status success");
    assert.equal(m.drives.testPollNoProgressCount, 0);
  });

  it("pollTestStatus reports failed tests as errors", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: () => ({ in_progress: false, percent_remaining: null, status: "completed: read failure", failed: true, log: [] }),
    });
    await m.fn.pollTestStatus("PD1");
    assert.match(el["smart-test-status"].textContent, /^Self-test failed/);
    assert.equal(el["smart-test-status"].className, "smart-test-status error");
  });

  it("onRunDriveTest requires at least one test type", async () => {
    const { m, el } = setup([DRIVE], "PD1");
    await m.fn.onRunDriveTest();
    assert.equal(el["smart-test-status"].textContent, "Select at least one test type to run");
    assert.equal(el["smart-test-status"].className, "smart-test-status error");
    assert.equal(m.calls("run_drive_test").length, 0);
  });

  it("onRunDriveTest runs each selected test and flags failures", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: (cmd, args) => {
        if (cmd === "run_drive_test") {
          if (args.testType === "long") throw { kind: "other", message: "Device busy" };
          return "Testing has begun.";
        }
      },
    });
    m.document.setQueryAll('input[name="smart-test-type"]:checked', [{ value: "short" }, { value: "long" }]);
    await m.fn.onRunDriveTest();
    assert.deepEqual(
      m.calls("run_drive_test").map((c) => c.args.testType),
      ["short", "long"]
    );
    assert.match(el["smart-test-status"].textContent, /short: started\n {2}Testing has begun\./);
    assert.match(el["smart-test-status"].textContent, /long: error - Device busy/);
    assert.equal(el["smart-test-status"].className, "smart-test-status error");
    assert.equal(el["smart-run-test"].disabled, false);
  });
});

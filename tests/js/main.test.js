// Unit tests for src/main.js (the Tauri frontend). The file is a plain browser script, so it
// is evaluated in a node:vm context with stubbed window.__TAURI__, document, localStorage, etc.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  MAIN_JS,
  loadScripts,
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

  const loaded = loadScripts([MAIN_JS], globals);
  return {
    ...loaded,
    fn: loaded.ctx,
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

describe("isWallhavenUrl", () => {
  it("accepts http(s) wallhaven URLs (case/whitespace-insensitive)", () => {
    assert.equal(pure.isWallhavenUrl("https://wallhaven.cc/search?q=cats"), true);
    assert.equal(pure.isWallhavenUrl("  HTTP://WALLHAVEN.CC/api/v1/search  "), true);
  });

  it("rejects folders, other schemes, other hosts and empty values", () => {
    assert.equal(pure.isWallhavenUrl("C:\\wallhaven.cc\\pics"), false);
    assert.equal(pure.isWallhavenUrl("ftp://wallhaven.cc/x"), false);
    assert.equal(pure.isWallhavenUrl("https://example.com/"), false);
    assert.equal(pure.isWallhavenUrl(""), false);
    assert.equal(pure.isWallhavenUrl(null), false);
    assert.equal(pure.isWallhavenUrl(undefined), false);
  });
});

describe("wcParsePurity / wcBuildPurity", () => {
  const parse = (v) => plain(pure.wcParsePurity(v));

  it("defaults to SFW + Sketchy", () => {
    assert.deepEqual(parse(undefined), [true, true, false]);
    assert.deepEqual(parse(""), [true, true, false]);
  });

  it("parses valid bit strings", () => {
    assert.deepEqual(parse("100"), [true, false, false]);
    assert.deepEqual(parse("011"), [false, true, true]);
    assert.deepEqual(parse("111"), [true, true, true]);
    assert.deepEqual(parse(" 101 "), [true, false, true]);
    assert.deepEqual(parse(101), [true, false, true]);
  });

  it("pads short strings with zeros", () => {
    assert.deepEqual(parse("1"), [true, false, false]);
    assert.deepEqual(parse("01"), [false, true, false]);
  });

  it("falls back to the default for invalid strings", () => {
    assert.deepEqual(parse("abc"), [true, true, false]);
    assert.deepEqual(parse("1111"), [true, true, false]);
    assert.deepEqual(parse("102"), [true, true, false]);
  });

  it("never yields 'nothing selected' (000 becomes SFW only)", () => {
    assert.deepEqual(parse("000"), [true, false, false]);
    assert.equal(pure.wcBuildPurity([false, false, false]), "100");
  });

  it("builds bit strings", () => {
    assert.equal(pure.wcBuildPurity([true, false, true]), "101");
    assert.equal(pure.wcBuildPurity([false, true, false]), "010");
  });

  it("round-trips", () => {
    for (const s of ["100", "010", "001", "110", "101", "011", "111"]) {
      assert.equal(pure.wcBuildPurity(pure.wcParsePurity(s)), s);
    }
  });
});

describe("needsAdminRights", () => {
  it("detects access/permission problems", () => {
    for (const msg of [
      "OpenFile failed, Error=5",
      "Access is denied.",
      "ACCESS DENIED",
      "This command requires admin rights",
      "Requires administrator privileges",
      "Elevation required",
      "Permission denied",
    ]) {
      assert.equal(pure.needsAdminRights(msg), true, msg);
    }
  });

  it("accepts non-string input such as errors", () => {
    assert.equal(pure.needsAdminRights(new Error("Access denied")), true);
    assert.equal(pure.needsAdminRights(null), false);
  });

  it("returns false for unrelated output", () => {
    assert.equal(pure.needsAdminRights("SMART overall-health self-assessment test result: PASSED"), false);
    assert.equal(pure.needsAdminRights("/dev/pd1: Unable to detect device type"), false);
  });
});

describe("extractSelfTestProgress", () => {
  const ATA_IN_PROGRESS = [
    "=== START OF READ SMART DATA SECTION ===",
    "SMART overall-health self-assessment test result: PASSED",
    "",
    "General SMART Values:",
    "Offline data collection status:  (0x00)\tOffline data collection activity",
    "\t\t\t\t\twas never started.",
    "Self-test execution status:      ( 249)\tSelf-test routine in progress...",
    "\t\t\t\t\t90% of test remaining.",
    "Total time to complete Offline ",
    "data collection: \t\t(    0) seconds.",
    "",
    "SMART Self-test log structure revision number 1",
    "Num  Test_Description    Status                  Remaining  LifeTime(hours)  LBA_of_first_error",
    "# 1  Short offline       Completed without error       00%      1234         -",
    "",
    "SMART Selective self-test log data structure revision number 1",
  ].join("\n");

  const ATA_DONE = [
    "Self-test execution status:      (   0)\tThe previous self-test routine completed",
    "\t\t\t\t\twithout error or no self-test has ever ",
    "\t\t\t\t\tbeen run.",
    "Total time to complete Offline ",
    "",
    "SMART Self-test log structure revision number 1",
    "# 1  Extended offline    Completed without error       00%      1300         -",
  ].join("\n");

  const ATA_FAILED = [
    "Self-test execution status:      ( 121)\tThe previous self-test completed having",
    "\t\t\t\t\tthe read element of the test failed.",
    "Total time to complete Offline ",
    "SMART Self-test log structure revision number 1",
    "# 1  Short offline       Completed: read failure       90%      1301         123456",
  ].join("\n");

  it("extracts status and log sections of an in-progress ATA test", () => {
    const r = pure.extractSelfTestProgress(ATA_IN_PROGRESS);
    assert.match(r.status, /^Self-test execution status:/);
    assert.match(r.status, /Self-test routine in progress/);
    assert.ok(!r.status.includes("Total time"), r.status);
    assert.match(r.log, /^SMART Self-test log structure revision number 1/);
    assert.match(r.log, /Completed without error/);
    assert.ok(!r.log.includes("Selective"), r.log);
    assert.equal(r.inProgress, true);
    assert.equal(r.hasFailed, false);
    assert.equal(r.text, `${r.status}\n\n${r.log}`);
  });

  it(
    "reports remaining percentage for smartctl's 'NN% of test remaining.' format",
    () => {
      assert.equal(pure.extractSelfTestProgress(ATA_IN_PROGRESS).progress, "90% remaining");
    }
  );

  it("parses 'NN% remaining' progress", () => {
    const r = pure.extractSelfTestProgress("Self-test status: Extended self-test in progress, 40% remaining\n");
    assert.equal(r.inProgress, true);
    assert.equal(r.progress, "40% remaining");
  });

  it("parses 'NN% left' progress", () => {
    assert.equal(pure.extractSelfTestProgress("Self-test status: in progress 15% left").progress, "15% remaining");
  });

  it("recognizes a completed test", () => {
    const r = pure.extractSelfTestProgress(ATA_DONE);
    assert.equal(r.inProgress, false);
    assert.equal(r.hasFailed, false);
    assert.equal(r.progress, null);
    assert.match(r.log, /Extended offline/);
  });

  it("recognizes a failed test", () => {
    const r = pure.extractSelfTestProgress(ATA_FAILED);
    assert.equal(r.hasFailed, true);
    assert.equal(r.inProgress, false);
  });

  it("treats 'No self-test in progress' as not in progress", () => {
    assert.equal(pure.extractSelfTestProgress("Self-test status: No self-test in progress\n").inProgress, false);
    assert.equal(pure.extractSelfTestProgress("Self-test status: No selftest in progress\n").inProgress, false);
  });

  it("handles NVMe-style output", () => {
    const r = pure.extractSelfTestProgress(
      "Self-test Log (NVMe Log 0x06)\nSelf-test status: Short self-test in progress (12% completed)\nNo Self-tests Logged"
    );
    assert.equal(r.status, "Self-test status: Short self-test in progress (12% completed)");
    assert.equal(r.inProgress, true);
    assert.match(r.log, /^Self-test Log \(NVMe Log 0x06\)/);
  });

  it("falls back to placeholder texts when nothing is found", () => {
    const r = pure.extractSelfTestProgress("");
    assert.equal(r.status, "Self-test status not available");
    assert.equal(r.log, "No self-test log available");
    assert.equal(r.inProgress, false);
    assert.equal(r.hasFailed, false);
    assert.equal(r.progress, null);
  });
});

describe("wcRenderQueueCard", () => {
  const image = (i, extra = {}) => ({ path: `C:\\pics\\img${i}.jpg`, file_name: `img${i}.jpg`, ...extra });

  it("renders at most 30 images plus a '+N more' line", () => {
    const images = Array.from({ length: 35 }, (_, i) => image(i));
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
    m.fn.clearLog();
    assert.equal(content.innerHTML, "");
  });

  it("keeps only the newest chunks so the log view stays bounded", () => {
    const m = loadMain();
    const content = m.document.add("vsd-log-content");
    m.document.add("vsd-log", { scrollHeight: 0 });
    const max = m.run("MAX_LOG_CHUNKS");
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
      if (!installed) throw "SMARTCTL_MISSING: smartmontools (smartctl) is not installed.";
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
      throw "SMARTCTL_MISSING: smartmontools (smartctl) is not installed.";
    });
    m.state.confirm = (() => false);
    await assert.rejects(m.fn.invokeSmart("get_drive_smart", { id: "x" }), /smartmontools is not installed/);
    assert.deepEqual(calls, ["get_drive_smart"]);
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
    assert.equal(m.run("vsdRunning"), true);
    m.fn.updateVsdUi(false);
    assert.equal(btn.className, "stopped");
    assert.equal(btn.querySelector(".toggle-label").textContent, "Start");
    assert.equal(m.run("vsdRunning"), false);
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

describe("wcUpdateModelFromUi", () => {
  function setupInputs(m, overrides = {}) {
    const values = {
      "wc-interval": "15",
      "wc-max-level": "7",
      "wc-wallhaven-api-key": "",
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
  }

  it("does nothing while settings are not loaded", () => {
    const m = loadMain();
    assert.doesNotThrow(() => m.fn.wcUpdateModelFromUi());
    assert.equal(m.run("wcSettings"), null);
  });

  it("copies UI values into the settings model", () => {
    const m = loadMain();
    setupInputs(m, { "wc-wallhaven-api-key": "  secret-key  " });
    m.assign("wcSettings", { source_folders: [], extra: "kept" });
    m.fn.wcUpdateModelFromUi();
    assert.deepEqual(plain(m.run("wcSettings")), {
      source_folders: [],
      extra: "kept",
      interval_minutes: 15,
      maximum_source_level: 7,
      wallhaven_api_key: "secret-key",
      use_wallhaven_api_key: true,
      rotation_mode: "Random",
      scaling_mode: "FitInsideScreen",
      background_color_argb: -15654349,
      use_separate_monitor_queues: false,
      keep_image_in_single_monitor_queue: false,
      disable_windows_slideshow_when_running: false,
      change_one_monitor_per_interval: false,
      show_picture_name_overlay: true,
      picture_name_overlay_text_mode: "FileName",
      picture_name_overlay_font_size: 30,
      picture_name_overlay_text_color_argb: -1,
      picture_name_overlay_offset_x: 10,
      picture_name_overlay_offset_y: -20,
      picture_name_overlay_use_backdrop: false,
      enable_wallpaper_fade_transition: true,
      wallpaper_fade_duration_ms: 500,
      wallpaper_fade_steps: 8,
    });
  });

  it("clamps numbers and applies defaults for invalid input", () => {
    const m = loadMain();
    setupInputs(m, {
      "wc-interval": "99999",
      "wc-max-level": "-4",
      "wc-overlay-font-size": "",
      "wc-overlay-offset-x": "abc",
      "wc-fade-duration": "-50",
      "wc-fade-steps": "0",
    });
    m.assign("wcSettings", {});
    m.fn.wcUpdateModelFromUi();
    const s = plain(m.run("wcSettings"));
    assert.equal(s.interval_minutes, 1440);
    assert.equal(s.maximum_source_level, 1);
    assert.equal(s.picture_name_overlay_font_size, 28);
    assert.equal(s.picture_name_overlay_offset_x, 0);
    assert.equal(s.wallpaper_fade_duration_ms, 0);
    assert.equal(s.wallpaper_fade_steps, 12);
    assert.equal(s.use_wallhaven_api_key, false);
  });

  it("uses defaults for empty interval and level", () => {
    const m = loadMain();
    setupInputs(m, { "wc-interval": "", "wc-max-level": "" });
    m.assign("wcSettings", {});
    m.fn.wcUpdateModelFromUi();
    const s = plain(m.run("wcSettings"));
    assert.equal(s.interval_minutes, 30);
    assert.equal(s.maximum_source_level, 10);
  });
});

describe("wcRenderSources", () => {
  function setup(sources, selected = -1) {
    const m = loadMain();
    const container = m.document.add("wc-sources");
    const remove = m.document.add("wc-remove-source");
    const up = m.document.add("wc-move-up");
    const down = m.document.add("wc-move-down");
    m.assign("wcSettings", { source_folders: sources });
    m.assign("wcSelectedSourceIndex", selected);
    m.fn.wcRenderSources();
    return { m, container, remove, up, down, settings: () => plain(m.run("wcSettings")) };
  }

  const folder = { path: 'C:\\Pics "x" <y> & z', enabled: true, include_subfolders: true, level: 5 };
  const url = { path: "https://wallhaven.cc/search?q=a&b=1", enabled: false, level: 3, wallhaven_page_limit: 0, wallhaven_purity: "011" };

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
    assert.match(row.innerHTML, /data-purity="0" >/);
    assert.match(row.innerHTML, /data-purity="1" checked/);
    assert.match(row.innerHTML, /data-purity="2" checked/);
    assert.match(row.innerHTML, /<input type="checkbox"  title="Enabled">/);
    assert.equal(row.style.gridTemplateColumns, "24px 1fr 60px 80px 180px");
  });

  it("defaults the page limit to 1 when missing", () => {
    const { container } = setup([{ ...url, wallhaven_page_limit: undefined }]);
    assert.match(container.children[0].innerHTML, /value="1" title="Page limit/);
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
    assert.equal(m.run("wcSelectedSourceIndex"), -1);
    container.children[1].dispatch("click", { target: { tagName: "DIV" } });
    assert.equal(m.run("wcSelectedSourceIndex"), 1);
    assert.ok(container.children[1].classList.contains("selected"));
  });

  it("switching a folder path to a Wallhaven URL sets URL defaults and autosaves", async () => {
    const { m, container, settings } = setup([{ ...folder }]);
    const pathInput = container.children[0].querySelectorAll('input[type="text"], input[type="number"]')[0];
    pathInput.value = "  https://wallhaven.cc/latest  ";
    pathInput.dispatch("change");
    const s = settings().source_folders[0];
    assert.equal(s.path, "https://wallhaven.cc/latest");
    assert.equal(s.include_subfolders, false);
    assert.equal(s.wallhaven_page_limit, 1);
    assert.equal(s.wallhaven_purity, "110");
    // Re-rendered as a URL row.
    assert.equal(container.children.length, 1);
    assert.match(container.children[0].innerHTML, /wallchanger-purity/);

    // Debounced save.
    assert.deepEqual(m.timers.pending().map((t) => t.ms), [300]);
    m.timers.runAll();
    await flush();
    const saves = m.calls("wc_save_settings");
    assert.equal(saves.length, 1);
    assert.equal(saves[0].args.settings.source_folders[0].path, "https://wallhaven.cc/latest");
  });

  it("debounces autosave to a single pending timer", () => {
    const { m, container } = setup([{ ...folder }]);
    const checkbox = container.children[0].querySelector('input[type="checkbox"]');
    checkbox.checked = false;
    checkbox.dispatch("change");
    checkbox.checked = true;
    checkbox.dispatch("change");
    assert.equal(m.timers.pending().length, 1);
    assert.equal(plain(m.run("wcSettings")).source_folders[0].enabled, true);
  });

  it("clamps the level to 1..10", () => {
    const { container, settings } = setup([{ ...folder }]);
    const levelInput = container.children[0].querySelectorAll('input[type="text"], input[type="number"]')[1];
    for (const [input, expected] of [["99", 10], ["0", 1], ["-3", 1], ["abc", 1], ["7", 7]]) {
      levelInput.value = input;
      levelInput.dispatch("change");
      assert.equal(settings().source_folders[0].level, expected, input);
    }
  });

  it("parses the page limit (0 = unlimited, negatives clamp to 0, invalid = 1)", () => {
    const { container, settings } = setup([{ ...url }]);
    const pageInput = container.children[0].querySelectorAll('input[type="text"], input[type="number"]')[2];
    for (const [input, expected] of [["5", 5], ["0", 0], ["-2", 0], ["abc", 1]]) {
      pageInput.value = input;
      pageInput.dispatch("change");
      assert.equal(settings().source_folders[0].wallhaven_page_limit, expected, input);
    }
  });

  it("rebuilds the purity string from the checkboxes", () => {
    const { container, settings } = setup([{ ...url }]);
    const purity = container.children[0].querySelectorAll('.wallchanger-purity input[type="checkbox"]');
    [true, false, true].forEach((v, i) => (purity[i].checked = v));
    purity[0].dispatch("change");
    assert.equal(settings().source_folders[0].wallhaven_purity, "101");
    [false, false, false].forEach((v, i) => (purity[i].checked = v));
    purity[2].dispatch("change");
    assert.equal(settings().source_folders[0].wallhaven_purity, "100");
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
    m.assign("wcSettings", { source_folders: paths.map((path) => ({ path, enabled: true, level: 5 })) });
    m.assign("wcSelectedSourceIndex", selected);
    return {
      m,
      paths: () => plain(m.run("wcSettings")).source_folders.map((s) => s.path),
      selected: () => m.run("wcSelectedSourceIndex"),
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
    t.m.assign("wcSelectedSourceIndex", 1);
    t.m.fn.wcMoveSource(1);
    assert.deepEqual(t.paths(), ["a", "b"]);
  });
});

describe("wcAddWallhavenUrl / wcAddFolder", () => {
  function setup(opts) {
    const m = loadMain(opts);
    for (const id of ["wc-sources", "wc-remove-source", "wc-move-up", "wc-move-down", "wc-message"]) m.document.add(id);
    m.assign("wcSettings", { source_folders: [] });
    return m;
  }

  it("rejects non-Wallhaven URLs with an error message", async () => {
    const m = setup({ prompt: () => "https://example.com/x" });
    await m.fn.wcAddWallhavenUrl();
    assert.deepEqual(plain(m.run("wcSettings")).source_folders, []);
    const msg = m.document.getElementById("wc-message");
    assert.equal(msg.textContent, "Please enter a valid Wallhaven URL.");
    assert.equal(msg.className, "wallchanger-message error");
  });

  it("ignores a cancelled prompt", async () => {
    const m = setup({ prompt: () => null });
    await m.fn.wcAddWallhavenUrl();
    assert.deepEqual(plain(m.run("wcSettings")).source_folders, []);
    assert.equal(m.document.getElementById("wc-message").textContent, "");
  });

  it("adds a trimmed Wallhaven URL source with URL defaults", async () => {
    const m = setup({ prompt: () => "  https://wallhaven.cc/search?q=cats  " });
    await m.fn.wcAddWallhavenUrl();
    assert.deepEqual(plain(m.run("wcSettings")).source_folders, [
      {
        path: "https://wallhaven.cc/search?q=cats",
        enabled: true,
        include_subfolders: false,
        level: 5,
        wallhaven_page_limit: 1,
        wallhaven_purity: "110",
      },
    ]);
  });

  it("adds a picked folder with subfolders enabled", async () => {
    const m = setup({ dialog: () => "D:\\Wallpapers" });
    await m.fn.wcAddFolder();
    const [s] = plain(m.run("wcSettings")).source_folders;
    assert.equal(s.path, "D:\\Wallpapers");
    assert.equal(s.include_subfolders, true);
    assert.equal(s.level, 5);
  });

  it("does nothing when the folder dialog is cancelled", async () => {
    const m = setup({ dialog: () => null });
    await m.fn.wcAddFolder();
    assert.deepEqual(plain(m.run("wcSettings")).source_folders, []);
  });
});

describe("Drive Test", () => {
  const DRIVE = {
    id: "PD1",
    device_id: "\\\\.\\PHYSICALDRIVE1",
    vendor: "Samsung",
    model: "SSD <980>",
    serial: "S123",
    type: "SSD",
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
    m.assign("cachedDrives", drives);
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
    assert.equal(m.run("formatTimer"), null);
  });

  it("pollTestStatus stops when the selected drive changed", async () => {
    const { m } = setup([DRIVE], "PD2");
    await m.fn.pollTestStatus("PD1");
    assert.equal(m.calls("get_drive_smart_elevated").length, 0);
  });

  it("pollTestStatus shows progress while a test runs", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: () => "Self-test status: Extended self-test in progress, 40% remaining\n",
    });
    await m.fn.pollTestStatus("PD1");
    assert.match(el["smart-test-status"].textContent, /^Monitoring self-test progress\.\.\. \(40% remaining, updates every 10s\)/);
    assert.equal(el["smart-test-status"].className, "smart-test-status");
  });

  it("pollTestStatus marks success after two polls without a running test", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: () => "Self-test status: No self-test in progress\n",
    });
    await m.fn.pollTestStatus("PD1");
    assert.match(el["smart-test-status"].textContent, /^Self-test finished/);
    assert.equal(el["smart-test-status"].className, "smart-test-status");
    await m.fn.pollTestStatus("PD1");
    assert.equal(el["smart-test-status"].className, "smart-test-status success");
    assert.equal(m.run("testPollNoProgressCount"), 0);
  });

  it("pollTestStatus reports failed tests as errors", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: () => "Self-test status: Short self-test failed\n",
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
    assert.equal(m.calls("run_drive_test_elevated").length, 0);
  });

  it("onRunDriveTest runs each selected test and flags failures", async () => {
    const { m, el } = setup([DRIVE], "PD1", {
      invoke: (cmd, args) => {
        if (cmd === "run_drive_test_elevated") {
          if (args.testType === "long") return "Testing failed: device busy";
          return "Testing has begun.";
        }
      },
    });
    m.document.setQueryAll('input[name="smart-test-type"]:checked', [{ value: "short" }, { value: "long" }]);
    await m.fn.onRunDriveTest();
    assert.deepEqual(
      m.calls("run_drive_test_elevated").map((c) => c.args.testType),
      ["short", "long"]
    );
    assert.match(el["smart-test-status"].textContent, /short: started\n {2}Testing has begun\./);
    assert.match(el["smart-test-status"].textContent, /long: failed/);
    assert.equal(el["smart-test-status"].className, "smart-test-status error");
    assert.equal(el["smart-run-test"].disabled, false);
  });
});

'use strict';

// ── Metadata Extraction ─────────────────────────────────────────────────────

function extractText(selectors) {
  for (const selector of selectors) {
    const el = document.querySelector(selector);
    if (el && el.textContent.trim()) return el.textContent.trim();
  }
  return '';
}

function extractList(selectors) {
  for (const selector of selectors) {
    const seen = new Set();
    const values = [];
    document.querySelectorAll(selector).forEach(el => {
      const text = el.textContent.trim();
      if (text && !seen.has(text)) { seen.add(text); values.push(text); }
    });
    if (values.length) return values;
  }
  return [];
}

function extractMetadata() {
  return {
    title: document.title || '',
    channel: extractText([
      '.videoUserInfo .username', '.videoUploader .username',
      '.channel-name', '.uploader a', '.author a',
      '.user-name', '.username', '.video-info-row.userRow a',
    ]),
    categories: extractList([
      '.categoriesWrapper a[data-label="category"]', '.categoriesWrapper a',
      '.categories a', '.category-list a', '.videoCategories a', '.categories-list a',
    ]),
    tags: extractList([
      '.tagsWrapper a[data-label="tag"]', '.tagsWrapper a',
      '.tags a', '.tag-list a', '.videoTags a', '.tags-list a',
    ]),
    actors: extractList([
      '.pornstarsWrapper a[data-label="pornstar"]', '.pornstarsWrapper a',
      '.pornstars a', '.pornstar-list a', '.pornstar-list-btn',
    ]),
    uploadDate: extractText([
      '.videoAdded', '.videoInfo .videoAdded',
      '.upload-date', '.video-uploaded', '.date-added', '.published',
    ]),
  };
}

// ── State ───────────────────────────────────────────────────────────────────

const SCAN_THROTTLE_MS = 1000;
// A port that disconnects sooner than this after connecting is treated as a dead context.
const MIN_PORT_LIFETIME_MS = 1000;

const reportedUrls = new Set();
let lastMetadataKey = '';
let observer = null;
let scanTimer = null;
let contextValid = true;

// ── Context Validation ──────────────────────────────────────────────────────

function stopAll() {
  contextValid = false;
  if (observer) {
    try { observer.disconnect(); } catch (e) {}
    observer = null;
  }
  if (scanTimer !== null) {
    clearTimeout(scanTimer);
    scanTimer = null;
  }
}

function runtimeAlive() {
  try { return !!chrome.runtime.id; }
  catch (e) { return false; }
}

// The open port lets the background notice this tab; its disconnect tells us the
// extension context went away (extension reloaded/removed). If the runtime is still
// alive, only the background worker restarted: reconnect and re-send everything,
// since the new worker has lost its in-memory page metadata.
function connectPort() {
  const connectedAt = Date.now();
  try {
    const port = chrome.runtime.connect({ name: 'content' });
    port.onDisconnect.addListener(() => {
      try { void chrome.runtime.lastError; } catch (e) {}
      if (contextValid && runtimeAlive() && Date.now() - connectedAt >= MIN_PORT_LIFETIME_MS) {
        lastMetadataKey = '';
        reportedUrls.clear();
        connectPort();
        scheduleScan();
      } else {
        stopAll();
      }
    });
  } catch (e) {
    stopAll();
  }
}

connectPort();

function extensionContextValid() {
  if (!contextValid) return false;
  if (runtimeAlive()) return true;
  stopAll();
  return false;
}

function sendMessageSafe(msg) {
  if (!extensionContextValid()) return;
  try {
    chrome.runtime.sendMessage(msg, () => {
      try {
        const err = chrome.runtime.lastError;
        if (err && !err.message.includes('Could not establish connection') && !err.message.includes('Extension context invalidated')) {
          console.error('sendMessage error:', err.message);
        }
      } catch (e) {}
    });
  } catch (e) {
    stopAll();
  }
}

// ── Detection ───────────────────────────────────────────────────────────────

/** Send page metadata, but only when it (or the page URL) changed since the last send. */
function sendMetadata() {
  if (!extensionContextValid()) return;
  const metadata = extractMetadata();
  const key = JSON.stringify([location.href, metadata]);
  if (key === lastMetadataKey) return;
  lastMetadataKey = key;
  sendMessageSafe({ type: 'metadata', pageUrl: location.href, metadata });
}

/** Report HLS <video> sources that were not reported yet. */
function scan() {
  if (!extensionContextValid()) return;
  sendMetadata();
  const streams = [];
  document.querySelectorAll('video').forEach(v => {
    const src = v.src;
    if (src && /\.m3u8?(\?|$)/i.test(src) && !reportedUrls.has(src)) {
      reportedUrls.add(src);
      streams.push({ url: src, type: 'hls' });
    }
  });
  if (streams.length) {
    sendMessageSafe({ type: 'detected', streams, title: document.title || '', metadata: extractMetadata() });
  }
}

/** Throttled scan, triggered by DOM mutations. */
function scheduleScan() {
  if (!contextValid || scanTimer !== null) return;
  scanTimer = setTimeout(() => {
    scanTimer = null;
    scan();
  }, SCAN_THROTTLE_MS);
}

// Re-report everything when detection is switched back on (reports made while it
// was off were ignored by the background).
try {
  chrome.storage.onChanged.addListener((changes) => {
    if (changes.detectionEnabled && changes.detectionEnabled.newValue && contextValid) {
      reportedUrls.clear();
      scheduleScan();
    }
  });
} catch (e) {}

// ── Init ────────────────────────────────────────────────────────────────────

scan();

if (contextValid && typeof MutationObserver === 'function' && document.documentElement) {
  observer = new MutationObserver(scheduleScan);
  observer.observe(document.documentElement, {
    childList: true, subtree: true, attributes: true, attributeFilter: ['src']
  });
}

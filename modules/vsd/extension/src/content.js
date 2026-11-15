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

const reportedUrls = new Set();
// Stored streams this page already sent its metadata for (so a refused answer isn't repeated).
const answeredStreams = new Set();
let observer = null;
let scanTimer = null;
let contextValid = true;

// ── Context Validation ──────────────────────────────────────────────────────
// There is deliberately no long-lived port to the background: every open tab reconnecting
// whenever the service worker stops would keep restarting it. A reloaded/removed extension is
// noticed lazily instead, the next time this script wants to send something (`chrome.runtime.id`
// is gone, or sendMessage throws), and then all work stops. Everything the background knows is
// in storage (the stream list), so nothing has to be re-sent after it restarts.

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

/** Whether the extension is still there; stops all work the first time it is not. */
function extensionContextValid() {
  if (!contextValid) return false;
  try {
    if (chrome.runtime.id) return true;
  } catch (e) {}
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

/**
 * Sends this page's metadata when the background has stored a stream for this page that has
 * none yet (streams found through network requests arrive without). `streams` is the stored
 * stream list. Pages without such a stream send nothing, so ordinary browsing never wakes the
 * background.
 */
function offerMetadata(streams) {
  if (!contextValid || !Array.isArray(streams)) return;
  const pageUrl = location.href;
  const waiting = streams.filter(s => s && s.pageUrl === pageUrl && !s.metadata && !answeredStreams.has(s.url));
  if (!waiting.length) return;
  waiting.forEach(s => answeredStreams.add(s.url));
  sendMessageSafe({ type: 'metadata', pageUrl, metadata: extractMetadata() });
}

/** Report HLS <video> sources that were not reported yet. */
function scan() {
  if (!extensionContextValid()) return;
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

try {
  chrome.storage.onChanged.addListener((changes) => {
    // Re-report everything when detection is switched back on (reports made while it was off
    // were ignored by the background).
    if (changes.detectionEnabled && changes.detectionEnabled.newValue) {
      reportedUrls.clear();
      scheduleScan();
    }
    if (changes.streams) offerMetadata(changes.streams.newValue);
  });
  // A stream may have been stored before this script started (requests made while the page
  // was still loading).
  chrome.storage.local.get({ streams: [] }, (data) => {
    try { void chrome.runtime.lastError; } catch (e) {}
    offerMetadata(data && data.streams);
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

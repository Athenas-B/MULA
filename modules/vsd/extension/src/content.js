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

// ── Context Validation ──────────────────────────────────────────────────────

let metadataInterval = null;
let scanInterval = null;
let contextValid = true;

try {
  const port = chrome.runtime.connect({ name: 'content' });
  port.onDisconnect.addListener(() => {
    contextValid = false;
    if (metadataInterval) clearInterval(metadataInterval);
    if (scanInterval) clearInterval(scanInterval);
  });
} catch (e) {
  contextValid = false;
}

function extensionContextValid() {
  if (!contextValid) return false;
  try { return !!chrome.runtime.id; }
  catch (e) { contextValid = false; return false; }
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
    contextValid = false;
  }
}

// ── Periodic Tasks ──────────────────────────────────────────────────────────

function sendMetadata() {
  if (!extensionContextValid()) return;
  sendMessageSafe({ type: 'metadata', pageUrl: location.href, metadata: extractMetadata() });
}

function scan() {
  if (!extensionContextValid()) return;
  const streams = [];
  document.querySelectorAll('video').forEach(v => {
    if (v.src && /\.m3u8?(\?|$)/i.test(v.src)) {
      streams.push({ url: v.src, type: 'hls' });
    }
  });
  if (streams.length) {
    sendMessageSafe({ type: 'detected', streams, title: document.title || '', metadata: extractMetadata() });
  }
}

sendMetadata();
setTimeout(sendMetadata, 2000);
metadataInterval = setInterval(sendMetadata, 5000);

scan();
scanInterval = setInterval(scan, 3000);

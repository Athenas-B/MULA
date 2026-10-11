'use strict';

if (typeof importScripts === 'function') {
  importScripts('errorlog.js');
}
vsdInstallErrorCapture('background');

// ── Configuration ───────────────────────────────────────────────────────────

const DEFAULT_SERVER = 'http://127.0.0.1:8765';
const MAX_STREAMS = 50;
const MAX_CAPTURED_HEADERS = 100;

// ── State ───────────────────────────────────────────────────────────────────

const capturedHeaders = new Map();
const pageMetadata = new Map();

const actionApi = chrome.action || chrome.browserAction;
const ICON_SIZES = [16, 32, 48, 128];
const iconImageDataCache = {};

let detectionEnabled = true;

// ── Icon Management ─────────────────────────────────────────────────────────

async function getGrayscaleIconImageData() {
  if (iconImageDataCache.gray) return iconImageDataCache.gray;
  const result = {};
  for (const size of ICON_SIZES) {
    const res = await fetch(chrome.runtime.getURL(`icons/icon${size}.png`));
    const blob = await res.blob();
    const bitmap = await createImageBitmap(blob);
    const canvas = new OffscreenCanvas(size, size);
    const ctx = canvas.getContext('2d');
    ctx.drawImage(bitmap, 0, 0, size, size);
    const imageData = ctx.getImageData(0, 0, size, size);
    const data = imageData.data;
    for (let i = 0; i < data.length; i += 4) {
      const gray = 0.3 * data[i] + 0.59 * data[i + 1] + 0.11 * data[i + 2];
      data[i] = data[i + 1] = data[i + 2] = gray;
    }
    result[size] = imageData;
  }
  iconImageDataCache.gray = result;
  return result;
}

async function updateActionIcon(enabled) {
  if (!actionApi) return;
  try {
    if (enabled) {
      actionApi.setIcon({
        path: { 16: 'icons/icon16.png', 32: 'icons/icon32.png', 48: 'icons/icon48.png', 128: 'icons/icon128.png' }
      });
    } else {
      actionApi.setIcon({ imageData: await getGrayscaleIconImageData() });
    }
  } catch (e) {
    console.error('Failed to update action icon:', e);
  }
}

// ── Detection State ─────────────────────────────────────────────────────────

chrome.storage.local.get({ detectionEnabled: true }, (result) => {
  detectionEnabled = result.detectionEnabled;
  updateActionIcon(detectionEnabled);
});

chrome.storage.onChanged.addListener((changes) => {
  if (changes.detectionEnabled) {
    detectionEnabled = changes.detectionEnabled.newValue;
    updateActionIcon(detectionEnabled);
  }
});

// ── Helpers ─────────────────────────────────────────────────────────────────

function isM3u8(url) {
  return /\.m3u8?(\?|$)/i.test(url);
}

function isMasterPlaylistUrl(url) {
  try {
    const path = new URL(url).pathname.toLowerCase();
    return /(^|\/)master(?:_[\w-]+)?\.m3u8$/.test(path);
  } catch (e) {
    return false;
  }
}

function isExtensionUrl(url) {
  return url && (url.startsWith('chrome-extension://') || url.startsWith('moz-extension://'));
}

function makePageHeaders(pageUrl) {
  if (!pageUrl || pageUrl === 'unknown' || !pageUrl.startsWith('http')) return null;
  try {
    return { Referer: pageUrl, Origin: new URL(pageUrl).origin, Accept: 'application/vnd.apple.mpegurl, video/*, */*' };
  } catch (e) {
    return null;
  }
}

async function hasTsSegments(url, headers) {
  try {
    const res = await fetch(url, { headers });
    if (!res.ok) return true;
    const text = await res.text();
    return /(^|\n)[^#].*\.ts(\?|$)/m.test(text);
  } catch (e) {
    return true;
  }
}

function queryTabs() {
  return new Promise((resolve, reject) => {
    chrome.tabs.query({}, (tabs) => {
      if (chrome.runtime.lastError) return reject(new Error(chrome.runtime.lastError.message));
      resolve(tabs || []);
    });
  });
}

// The tab showing `pageUrl` (or another page of the same site). Throws for invalid URLs.
function matchTab(tabs, pageUrl) {
  const origin = new URL(pageUrl).origin;
  return tabs.find(t => t.url && !isExtensionUrl(t.url) && (t.url === pageUrl || t.url.startsWith(origin + '/')));
}

async function findTabTitle(pageUrl) {
  if (isExtensionUrl(pageUrl)) return '';
  try {
    return matchTab(await queryTabs(), pageUrl)?.title || '';
  } catch (e) {
    return '';
  }
}

async function enrichTitles(streams) {
  try {
    const tabs = await queryTabs();
    return streams.map(s => {
      if (s.title || !s.pageUrl || s.pageUrl === 'unknown' || isExtensionUrl(s.pageUrl)) return s;
      try {
        const tab = matchTab(tabs, s.pageUrl);
        if (tab?.title) return { ...s, title: tab.title };
      } catch (e) {}
      return s;
    });
  } catch (e) {
    return streams;
  }
}

async function getServerUrl() {
  const result = await new Promise(r => chrome.storage.local.get({ serverUrl: DEFAULT_SERVER }, r));
  return result.serverUrl || DEFAULT_SERVER;
}

// ── Stream Storage ──────────────────────────────────────────────────────────

function addStream(url, pageUrl, type = 'hls', title = '', headers = null) {
  if (!url || !isM3u8(url)) return;
  const metadata = (pageUrl && pageUrl !== 'unknown') ? (pageMetadata.get(pageUrl) || null) : null;
  const finalHeaders = headers || capturedHeaders.get(url) || makePageHeaders(pageUrl);

  chrome.storage.local.get({ streams: [] }, (result) => {
    const streams = result.streams;
    const existing = streams.find(s => s.url === url);
    if (existing) {
      if (title && !existing.title) existing.title = title;
      if (finalHeaders) existing.headers = finalHeaders;
      if (metadata && !existing.metadata) existing.metadata = metadata;
      chrome.storage.local.set({ streams });
      return;
    }
    streams.unshift({ url, pageUrl, type, title, headers: finalHeaders, metadata, detectedAt: Date.now() });
    chrome.storage.local.set({ streams: streams.slice(0, MAX_STREAMS) });
  });
}

function removeStream(url) {
  chrome.storage.local.get({ streams: [] }, (result) => {
    chrome.storage.local.set({ streams: result.streams.filter(s => s.url !== url) });
  });
}

// Records a detected HLS playlist, then drops it again if it lists no .ts segments.
function trackStream(url, pageUrl, title = '') {
  const headers = makePageHeaders(pageUrl);
  addStream(url, pageUrl, 'hls', title, headers);
  hasTsSegments(url, headers).then(ok => { if (!ok) removeStream(url); });
}

// Keeps only the streams (and their active jobs) whose URL passes `keep`.
function pruneByUrl({ streams, activeJobs }, keep) {
  return {
    streams: streams.filter(s => keep(s.url)),
    activeJobs: Object.fromEntries(Object.entries(activeJobs || {}).filter(([url]) => keep(url))),
  };
}

// ── Network Interception ────────────────────────────────────────────────────

function handleRequest(details) {
  if (!detectionEnabled || !isM3u8(details.url) || isMasterPlaylistUrl(details.url)) return;

  const fallbackPageUrl = details.documentUrl || details.initiator || 'unknown';
  if (isExtensionUrl(fallbackPageUrl)) return;

  if (details.tabId > 0) {
    chrome.tabs.get(details.tabId, (tab) => {
      if (chrome.runtime.lastError) return trackStream(details.url, fallbackPageUrl);
      trackStream(details.url, tab?.url || fallbackPageUrl, tab?.title || '');
    });
  } else {
    trackStream(details.url, fallbackPageUrl);
  }
}

chrome.webRequest.onBeforeRequest.addListener(handleRequest, { urls: ['<all_urls>'] }, []);

const headerExtraInfo = typeof browser !== 'undefined' ? ['requestHeaders'] : ['requestHeaders', 'extraHeaders'];

chrome.webRequest.onBeforeSendHeaders.addListener(
  (details) => {
    if (!isM3u8(details.url)) return;
    const pageUrl = details.initiator || details.documentUrl || 'unknown';
    if (isExtensionUrl(pageUrl)) return;

    const h = {};
    details.requestHeaders.forEach(({ name, value }) => {
      if (['referer', 'origin', 'user-agent', 'accept'].includes(name.toLowerCase())) {
        h[name] = value;
      }
    });
    if (Object.keys(h).length) {
      capturedHeaders.set(details.url, h);
      if (capturedHeaders.size > MAX_CAPTURED_HEADERS) {
        capturedHeaders.delete(capturedHeaders.keys().next().value);
      }
    }
  },
  { urls: ['<all_urls>'] },
  headerExtraInfo
);

// ── Server Communication ────────────────────────────────────────────────────

async function downloadStream(m3u8Url, title = '', headers = null, pageUrl = '', metadata = null) {
  if (!title && pageUrl) {
    title = await findTabTitle(pageUrl) || title;
  }
  const base = await getServerUrl();
  const res = await fetch(`${base}/download`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ url: m3u8Url, title, headers, pageUrl, metadata })
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `Server error: ${res.status}`);
  return data;
}

async function getProgress(jobId) {
  const base = await getServerUrl();
  const res = await fetch(`${base}/progress/${jobId}`);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `Server error: ${res.status}`);
  return data;
}

// ── Message Handling ────────────────────────────────────────────────────────

chrome.runtime.onConnect.addListener((port) => {
  if (port.name === 'content') {
    port.onDisconnect.addListener(() => {});
  }
});

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  switch (message.type) {
    case 'metadata': {
      const pageUrl = message.pageUrl || sender.tab?.url;
      if (pageUrl && message.metadata) {
        pageMetadata.set(pageUrl, message.metadata);
        chrome.storage.local.get({ streams: [] }, (result) => {
          let changed = false;
          result.streams.forEach(s => {
            if (s.pageUrl === pageUrl && !s.metadata) { s.metadata = message.metadata; changed = true; }
          });
          if (changed) chrome.storage.local.set({ streams: result.streams });
        });
      }
      sendResponse({ ok: true });
      break;
    }

    case 'detected': {
      const pageUrl = sender.tab?.url || 'unknown';
      if (message.metadata && pageUrl !== 'unknown') pageMetadata.set(pageUrl, message.metadata);
      if (detectionEnabled) {
        for (const s of message.streams || []) {
          if (s.type === 'hls') trackStream(s.url, pageUrl, message.title || '');
          else addStream(s.url, pageUrl, s.type, message.title || '', makePageHeaders(pageUrl));
        }
      }
      sendResponse({ ok: true });
      break;
    }

    case 'download': {
      chrome.storage.local.get({ streams: [], activeJobs: {} }, (result) => {
        const stream = result.streams.find(s => s.url === message.url);
        const title = stream?.title || '';
        const headers = stream?.headers || makePageHeaders(stream?.pageUrl || 'unknown');
        const metadata = stream?.metadata || null;
        downloadStream(message.url, title, headers, stream?.pageUrl || '', metadata)
          .then(r => {
            if (r?.job_id) {
              const jobs = result.activeJobs || {};
              jobs[message.url] = r.job_id;
              chrome.storage.local.set({ activeJobs: jobs });
            }
            sendResponse(r);
          })
          .catch(err => sendResponse({ error: err.message }));
      });
      return true;
    }

    case 'clearJob': {
      chrome.storage.local.get({ activeJobs: {}, streams: [] }, (result) => {
        const jobs = result.activeJobs || {};
        delete jobs[message.url];
        const stream = result.streams.find(s => s.url === message.url);
        if (stream) stream.saved = true;
        chrome.storage.local.set({ activeJobs: jobs, streams: result.streams });
        sendResponse({ ok: true });
      });
      return true;
    }

    case 'getActiveJobs': {
      chrome.storage.local.get({ activeJobs: {} }, (result) => sendResponse(result.activeJobs || {}));
      return true;
    }

    case 'progress': {
      getProgress(message.job_id)
        .then(r => sendResponse(r))
        .catch(err => sendResponse({ status: 'error', error: err.message }));
      return true;
    }

    case 'getStreams': {
      chrome.storage.local.get({ streams: [] }, (result) => {
        enrichTitles(result.streams || [])
          .then(enriched => sendResponse(enriched))
          .catch(() => sendResponse(result.streams));
      });
      return true;
    }

    case 'remove': {
      if (message.url) removeStream(message.url);
      sendResponse({ ok: true });
      break;
    }

    case 'clear': {
      chrome.storage.local.get({ streams: [], activeJobs: {} }, (result) => {
        const keepUrls = new Set(message.keepUrls || []);
        chrome.storage.local.set(pruneByUrl(result, url => keepUrls.has(url)), () => sendResponse({ ok: true }));
      });
      return true;
    }

    case 'removeUrls': {
      const removeUrls = new Set(message.urls || []);
      chrome.storage.local.get({ streams: [], activeJobs: {} }, (result) => {
        chrome.storage.local.set(pruneByUrl(result, url => !removeUrls.has(url)), () => sendResponse({ ok: true }));
      });
      return true;
    }

    case 'clearSaved': {
      chrome.storage.local.get({ streams: [], activeJobs: {} }, (result) => {
        const { streams, activeJobs: jobs } = result;
        const savedUrls = new Set();
        streams.forEach(s => { if (s.saved) savedUrls.add(s.url); });
        for (const [url, jobId] of Object.entries(jobs)) {
          const p = message.jobProgress?.[jobId];
          if (p && p.status === 'done') savedUrls.add(url);
        }
        chrome.storage.local.set(pruneByUrl(result, url => !savedUrls.has(url)), () => sendResponse({ ok: true }));
      });
      return true;
    }

    case 'clearAllData': {
      chrome.storage.local.set({ streams: [], activeJobs: {}, errorLog: [] }, () => sendResponse({ ok: true }));
      return true;
    }
  }
  return true;
});

'use strict';

if (typeof importScripts === 'function') {
  importScripts('errorlog.js', 'config.js');
}
vsdInstallErrorCapture('background');

// ── Configuration ───────────────────────────────────────────────────────────

const MAX_STREAMS = 50;
const MAX_CAPTURED_HEADERS = 100;
const MAX_PAGE_METADATA = 100;
const MAX_VERIFIED_URLS = 500;
const POLL_INTERVAL_MS = 1000;
const POLL_RETRY_MS = 5000;
const POLL_TIMEOUT_MS = 10000;
// Network failures (server unreachable) tolerated before a job is marked as failed.
const MAX_POLL_FAILURES = 10;

// ── State ───────────────────────────────────────────────────────────────────

const capturedHeaders = new Map();
const pageMetadata = new Map();
// Playlist URL -> true (media playlist with .ts segments, or unverifiable) / false (no segments).
const verifiedUrls = new Map();
// Playlist URL -> in-flight verification promise, shared by concurrent detections.
const pendingVerifications = new Map();
// Stream URLs with a /download request in flight (guards against double starts).
const startingDownloads = new Set();

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

// ── Storage ─────────────────────────────────────────────────────────────────

function storageGet(defaults) {
  return new Promise((resolve, reject) => {
    chrome.storage.local.get(defaults, (result) => {
      const err = chrome.runtime.lastError;
      if (err) reject(new Error(err.message));
      else resolve(result || {});
    });
  });
}

function storageSet(items) {
  return new Promise((resolve, reject) => {
    chrome.storage.local.set(items, () => {
      const err = chrome.runtime.lastError;
      if (err) reject(new Error(err.message));
      else resolve();
    });
  });
}

const STORE_DEFAULTS = { streams: [] };
let storeQueue = Promise.resolve();

/**
 * The single read-modify-write path for the store. Updates are serialized on a promise
 * chain so concurrent callers never overwrite each other. `fn(data)` receives
 * { streams } and may mutate or replace its values; only keys whose JSON changed are
 * written. Resolves with fn's return value.
 */
function updateStore(fn) {
  const run = storeQueue.then(async () => {
    const data = await storageGet(STORE_DEFAULTS);
    if (!Array.isArray(data.streams)) data.streams = [];
    const before = {};
    for (const key of Object.keys(data)) before[key] = JSON.stringify(data[key]);
    const result = fn(data);
    const changed = {};
    for (const key of Object.keys(data)) {
      if (JSON.stringify(data[key]) !== before[key]) changed[key] = data[key];
    }
    if (Object.keys(changed).length) await storageSet(changed);
    return result;
  });
  storeQueue = run.catch(() => {});
  return run;
}

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

function setBounded(map, key, value, max) {
  map.delete(key);
  map.set(key, value);
  while (map.size > max) map.delete(map.keys().next().value);
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

/** hasTsSegments, fetched at most once per URL (results are remembered in verifiedUrls). */
function verifyPlaylist(url, headers) {
  if (verifiedUrls.has(url)) return Promise.resolve(verifiedUrls.get(url));
  let pending = pendingVerifications.get(url);
  if (!pending) {
    pending = hasTsSegments(url, headers).then((ok) => {
      pendingVerifications.delete(url);
      setBounded(verifiedUrls, url, ok, MAX_VERIFIED_URLS);
      return ok;
    });
    pendingVerifications.set(url, pending);
  }
  return pending;
}

function queryTabs() {
  return new Promise((resolve, reject) => {
    chrome.tabs.query({}, (tabs) => {
      if (chrome.runtime.lastError) return reject(new Error(chrome.runtime.lastError.message));
      resolve(tabs || []);
    });
  });
}

/** Title of an open tab showing pageUrl (or another page of the same origin). */
function titleFromTabs(tabs, pageUrl) {
  if (!pageUrl || pageUrl === 'unknown' || isExtensionUrl(pageUrl)) return '';
  try {
    const origin = new URL(pageUrl).origin;
    const tab = tabs.find(t => t.url && !isExtensionUrl(t.url) && (t.url === pageUrl || t.url.startsWith(origin + '/')));
    return tab?.title || '';
  } catch (e) {
    return '';
  }
}

async function findTabTitle(pageUrl) {
  try {
    return titleFromTabs(await queryTabs(), pageUrl);
  } catch (e) {
    return '';
  }
}

/** Fill missing stream titles from open tabs (persisted, so the popup just renders storage). */
async function fillMissingTitles() {
  let tabs;
  try {
    tabs = await queryTabs();
  } catch (e) {
    return;
  }
  if (!tabs.length) return;
  await updateStore(({ streams }) => {
    for (const s of streams) {
      if (s.title) continue;
      const title = titleFromTabs(tabs, s.pageUrl);
      if (title) s.title = title;
    }
  });
}

// ── Stream Storage ──────────────────────────────────────────────────────────

function addStream(url, pageUrl, type = 'hls', title = '', headers = null) {
  if (!url || !isM3u8(url)) return Promise.resolve();
  const metadata = (pageUrl && pageUrl !== 'unknown') ? (pageMetadata.get(pageUrl) || null) : null;
  const finalHeaders = headers || capturedHeaders.get(url) || makePageHeaders(pageUrl);

  return updateStore((data) => {
    const existing = data.streams.find(s => s.url === url);
    if (existing) {
      if (title && !existing.title) existing.title = title;
      if (finalHeaders) existing.headers = finalHeaders;
      if (metadata && !existing.metadata) existing.metadata = metadata;
      return;
    }
    data.streams.unshift({ url, pageUrl, type, title, headers: finalHeaders, metadata, detectedAt: Date.now() });
    data.streams = data.streams.slice(0, MAX_STREAMS);
  });
}

function removeStreams(urls) {
  const remove = new Set(urls);
  return updateStore((data) => {
    data.streams = data.streams.filter(s => !remove.has(s.url));
  });
}

/**
 * Store a detected stream. HLS playlists are verified first (once per URL): playlists
 * without .ts segments (e.g. master playlists) are never stored, and repeated
 * detections of an already verified URL neither refetch it nor rewrite unchanged data.
 */
async function detectStream(url, pageUrl, type = 'hls', title = '', headers = null) {
  if (!url || !isM3u8(url)) return false;
  if (type === 'hls') {
    const ok = await verifyPlaylist(url, headers || makePageHeaders(pageUrl));
    if (!ok) return false;
  }
  await addStream(url, pageUrl, type, title, headers);
  return true;
}

function detectStreamSafe(...args) {
  detectStream(...args).catch(err => console.error('Failed to store detected stream:', err));
}

// ── Network Interception ────────────────────────────────────────────────────

function handleRequest(details) {
  if (!detectionEnabled || !isM3u8(details.url) || isMasterPlaylistUrl(details.url)) return;
  if (verifiedUrls.get(details.url) === false) return;

  const fallbackPageUrl = details.documentUrl || details.initiator || 'unknown';
  if (isExtensionUrl(fallbackPageUrl)) return;

  if (details.tabId && details.tabId > 0) {
    chrome.tabs.get(details.tabId, (tab) => {
      if (chrome.runtime.lastError || !tab) {
        detectStreamSafe(details.url, fallbackPageUrl, 'hls', '', makePageHeaders(fallbackPageUrl));
        return;
      }
      const pageUrl = tab.url || fallbackPageUrl;
      detectStreamSafe(details.url, pageUrl, 'hls', tab.title || '', makePageHeaders(pageUrl));
    });
  } else {
    detectStreamSafe(details.url, fallbackPageUrl, 'hls', '', makePageHeaders(fallbackPageUrl));
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

// ── Downloads & Job Tracking ────────────────────────────────────────────────

/**
 * Send a stored stream to the server and record the job on the stream record
 * (stream.job). Progress is then tracked by the poll loop below.
 */
async function startDownload(url) {
  const { streams } = await storageGet(STORE_DEFAULTS);
  const stream = (streams || []).find(s => s.url === url);
  if (!stream) throw new Error('Stream not found');
  if (stream.job?.status === 'running') return { ok: true, job_id: stream.job.id };
  if (startingDownloads.has(url)) throw new Error('Download is already starting');

  startingDownloads.add(url);
  try {
    let title = stream.title || '';
    if (!title && stream.pageUrl) title = await findTabTitle(stream.pageUrl);
    const pageUrl = stream.pageUrl || '';
    const headers = stream.headers || makePageHeaders(pageUrl || 'unknown');
    const data = await vsdServerJson('/download', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ url, title, headers, pageUrl, metadata: stream.metadata || null })
    });
    if (!data.job_id) throw new Error(data.error || 'Server did not return a job id');

    await updateStore((store) => {
      const s = store.streams.find(x => x.url === url);
      if (!s) return;
      if (title && !s.title) s.title = title;
      s.job = {
        id: data.job_id, status: 'running', stage: 'starting',
        current: 0, total: 0, output: null, error: null, startedAt: Date.now()
      };
    });
    schedulePoll();
    return data;
  } finally {
    startingDownloads.delete(url);
  }
}

let pollTimer = null;
let pollInFlight = false;
let pollRequested = false;

/** Schedule one run of the shared poll loop (no-op if one is already pending). */
function schedulePoll(delay = POLL_INTERVAL_MS) {
  if (pollInFlight) {
    pollRequested = true;
    return;
  }
  if (pollTimer !== null) return;
  pollTimer = setTimeout(() => {
    pollTimer = null;
    pollJobs();
  }, delay);
}

function applyProgress(job, progress) {
  const status = ['running', 'done', 'error'].includes(progress.status) ? progress.status : 'running';
  job.status = status;
  if (progress.stage !== undefined) job.stage = progress.stage;
  if (progress.current !== undefined) job.current = progress.current;
  if (progress.total !== undefined) job.total = progress.total;
  if (progress.output !== undefined) job.output = progress.output;
  job.error = status === 'error' ? (progress.error || 'failed') : null;
  delete job.failures;
}

function applyPollFailure(job, err) {
  // The server answered but does not know the job (e.g. it restarted): give up now.
  // Network failures are retried a few times before the job is marked as failed.
  const failures = (job.failures || 0) + 1;
  if (err.status || failures >= MAX_POLL_FAILURES) {
    job.status = 'error';
    job.error = err.message || 'failed';
    delete job.failures;
  } else {
    job.failures = failures;
  }
}

/** One iteration of the poll loop: fetch /progress for every running job and store it. */
async function pollJobs() {
  if (pollInFlight) return;
  pollInFlight = true;
  pollRequested = false;
  let nextDelay = null;
  try {
    const { streams } = await storageGet(STORE_DEFAULTS);
    const active = (streams || []).filter(s => s.job && s.job.status === 'running' && s.job.id);
    if (active.length) {
      const results = await Promise.all(active.map(async (s) => {
        try {
          const progress = await vsdServerJson(`/progress/${encodeURIComponent(s.job.id)}`, {}, POLL_TIMEOUT_MS);
          return { url: s.url, id: s.job.id, progress };
        } catch (err) {
          return { url: s.url, id: s.job.id, err };
        }
      }));
      const stillRunning = await updateStore((data) => {
        for (const r of results) {
          const s = data.streams.find(x => x.url === r.url);
          // Skip streams removed (or restarted with a new job) while the request was in flight.
          if (!s || !s.job || s.job.id !== r.id || s.job.status !== 'running') continue;
          if (r.err) applyPollFailure(s.job, r.err);
          else applyProgress(s.job, r.progress);
        }
        return data.streams.some(s => s.job && s.job.status === 'running');
      });
      if (stillRunning) nextDelay = POLL_INTERVAL_MS;
    }
  } catch (e) {
    console.error('Job polling failed:', e);
    nextDelay = POLL_RETRY_MS;
  } finally {
    pollInFlight = false;
    if (pollRequested && nextDelay === null) nextDelay = POLL_INTERVAL_MS;
    pollRequested = false;
    if (nextDelay !== null) schedulePoll(nextDelay);
  }
}

// ── Message Handling ────────────────────────────────────────────────────────

chrome.runtime.onConnect.addListener((port) => {
  if (port.name === 'content') {
    port.onDisconnect.addListener(() => {});
  }
});

function respondWith(promise, sendResponse) {
  promise
    .then(() => sendResponse({ ok: true }))
    .catch(err => sendResponse({ error: err.message }));
  return true;
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  switch (message.type) {
    case 'metadata': {
      const pageUrl = message.pageUrl || sender.tab?.url;
      if (pageUrl && message.metadata) {
        setBounded(pageMetadata, pageUrl, message.metadata, MAX_PAGE_METADATA);
        updateStore(({ streams }) => {
          streams.forEach(s => {
            if (s.pageUrl === pageUrl && !s.metadata) s.metadata = message.metadata;
          });
        }).catch(err => console.error('Failed to store metadata:', err));
      }
      sendResponse({ ok: true });
      break;
    }

    case 'detected': {
      const pageUrl = sender.tab?.url || 'unknown';
      if (message.metadata && pageUrl !== 'unknown') setBounded(pageMetadata, pageUrl, message.metadata, MAX_PAGE_METADATA);
      if (detectionEnabled) {
        const headers = makePageHeaders(pageUrl);
        for (const s of message.streams || []) {
          detectStreamSafe(s.url, pageUrl, s.type, message.title || '', headers);
        }
      }
      sendResponse({ ok: true });
      break;
    }

    case 'download': {
      startDownload(message.url)
        .then(r => sendResponse(r))
        .catch(err => sendResponse({ error: err.message }));
      return true;
    }

    case 'refresh': {
      // Sent by the popup when it opens: wakes the worker, resumes job polling and
      // fills missing titles from open tabs.
      schedulePoll(0);
      return respondWith(fillMissingTitles(), sendResponse);
    }

    case 'remove': {
      return respondWith(removeStreams(message.url ? [message.url] : []), sendResponse);
    }

    case 'clear': {
      // Finished jobs (saved or failed) stay; running ones only when protected.
      const protectActive = !!message.protectActive;
      return respondWith(updateStore((data) => {
        data.streams = data.streams.filter(s =>
          s.job && (s.job.status !== 'running' || protectActive));
      }), sendResponse);
    }

    case 'clearSaved': {
      return respondWith(updateStore((data) => {
        data.streams = data.streams.filter(s => s.job?.status !== 'done');
      }), sendResponse);
    }

    case 'clearAllData': {
      return respondWith(
        updateStore((data) => { data.streams = []; }).then(() => storageSet({ errorLog: [] })),
        sendResponse
      );
    }

    default:
      return false;
  }
  return false;
});

// ── Startup ─────────────────────────────────────────────────────────────────

// The MV3 service worker may have been suspended while jobs were running:
// resume polling for any job still marked as running in storage.
schedulePoll(0);

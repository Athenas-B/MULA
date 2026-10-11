'use strict';

vsdInstallErrorCapture('popup');

// Mark the layout as a pop-out window so the CSS can use all available space.
if (new URLSearchParams(window.location.search).has('popout')) {
  document.documentElement.classList.add('popout');
}

// ── DOM Elements ────────────────────────────────────────────────────────────

const streamsList = document.getElementById('streams');
const statusDiv = document.getElementById('status');
const clearBtn = document.getElementById('clear');
const clearSavedBtn = document.getElementById('clearSaved');
const copyLogsBtn = document.getElementById('copyLogs');
const clearLogsBtn = document.getElementById('clearLogs');
const popoutBtn = document.getElementById('popout');
const protectActive = document.getElementById('protectActive');
const detectionToggle = document.getElementById('detectionEnabled');

// ── State ───────────────────────────────────────────────────────────────────
// Everything persistent (streams and their jobs) lives in chrome.storage.local and is
// owned by the background; the popup only renders it and re-renders on changes.

let streams = [];
let detectionEnabled = true;
const rows = new Map();              // stream URL -> rendered row
const pendingDownloads = new Set();  // URLs with a 'download' request in flight
const startErrors = new Map();       // URL -> error from a failed download start

// ── Helpers ─────────────────────────────────────────────────────────────────

function formatProgress(p) {
  if (!p) return 'Starting...';
  if (p.status === 'error') return 'Error: ' + (p.error || 'failed');
  if (p.status === 'done') return 'Saved';
  if (p.stage === 'muxing') return 'Muxing...';
  if (p.total > 0) return `Downloading ${p.current}/${p.total}...`;
  return 'Starting...';
}

function buildTooltip(s) {
  const meta = s.metadata || {};
  const lines = [];
  if (s.title || meta.title) lines.push(`title: ${s.title || meta.title}`);
  if (meta.channel) lines.push(`artist: ${meta.channel}`);
  if (meta.actors?.length) lines.push(`album_artist: ${meta.actors.join(', ')}`);
  if (meta.categories?.length) lines.push(`genre: ${meta.categories.join(', ')}`);
  if (meta.tags?.length) lines.push(`keywords: ${meta.tags.join(', ')}`);
  if (s.pageUrl && s.pageUrl !== 'unknown') lines.push(`comment: ${s.pageUrl}`);
  if (s.url) lines.push(`description: ${s.url}`);
  if (meta.uploadDate) lines.push(`date: ${meta.uploadDate}`);
  return lines.length ? 'Metadata saved in file:\n' + lines.join('\n') : '';
}

/** [label, disabled] of a stream's download button. */
function downloadButtonState(s) {
  const job = s.job;
  if (pendingDownloads.has(s.url)) return ['Starting...', true];
  if (job) return [formatProgress(job), true];
  if (startErrors.has(s.url)) return ['Error: ' + startErrors.get(s.url), false];
  return ['Download via ffmpeg', false];
}

function sendMessage(message) {
  return new Promise((resolve, reject) => {
    try {
      chrome.runtime.sendMessage(message, (res) => {
        const err = chrome.runtime.lastError;
        if (err) reject(new Error(err.message));
        else resolve(res);
      });
    } catch (e) {
      reject(e);
    }
  });
}

async function copyToClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (e) {
    // Fallback for contexts where clipboard API is unavailable
    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.style.cssText = 'position:fixed;top:-9999px;left:-9999px';
    document.body.appendChild(textarea);
    textarea.select();
    let ok = false;
    try { ok = document.execCommand('copy'); } catch (e2) {}
    document.body.removeChild(textarea);
    return ok;
  }
}

function setText(el, text) {
  if (el.textContent !== text) el.textContent = text;
}

function setChildren(parent, children) {
  parent.replaceChildren(...children);
}

// ── Rendering ───────────────────────────────────────────────────────────────

function createElement(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text) el.textContent = text;
  return el;
}

function createRow() {
  const row = {
    li: createElement('li', 'stream-item'),
    titleDiv: createElement('div', 'stream-title'),
    urlDiv: createElement('div', 'stream-url'),
    actions: createElement('div', 'stream-actions'),
    dlBtn: createElement('button'),
    openBtn: createElement('button', 'open-btn', 'Open page'),
    openFileBtn: createElement('button', 'open-file-btn', 'Open file'),
    removeBtn: createElement('button', 'remove-btn', 'Remove'),
    hasTitle: null,
    stream: null,
  };
  row.actions.append(row.dlBtn, row.openBtn, row.openFileBtn, row.removeBtn);
  row.dlBtn.onclick = () => startDownload(row.stream.url);
  row.openBtn.onclick = () => openPage(row.stream);
  row.openFileBtn.onclick = () => openFile(row.stream);
  row.removeBtn.onclick = () => removeStream(row.stream.url);
  return row;
}

function updateRow(row, s) {
  row.stream = s;
  const tooltip = buildTooltip(s);
  if (row.li.title !== tooltip) row.li.title = tooltip;

  const hasTitle = !!s.title;
  setText(row.titleDiv, s.title || '');
  setText(row.urlDiv, s.url);
  if (row.hasTitle !== hasTitle) {
    row.hasTitle = hasTitle;
    setChildren(row.li, hasTitle ? [row.titleDiv, row.urlDiv, row.actions] : [row.urlDiv, row.actions]);
  }

  const [label, disabled] = downloadButtonState(s);
  setText(row.dlBtn, label);
  row.dlBtn.disabled = disabled;
  row.openFileBtn.disabled = !(s.job?.status === 'done' && s.job.output);
}

function updateStatus() {
  const n = streams.length;
  if (!n) {
    statusDiv.textContent = detectionEnabled ? 'No HLS streams detected yet.' : 'Detection paused';
  } else {
    statusDiv.textContent = `${n} stream(s) detected` + (detectionEnabled ? '' : ' (detection paused)');
  }
}

/** Render `streams`, reusing the existing row of each URL and only reordering when needed. */
function render() {
  updateStatus();
  const seen = new Set();
  const ordered = [];
  for (const s of streams) {
    if (!s || !s.url || seen.has(s.url)) continue;
    seen.add(s.url);
    let row = rows.get(s.url);
    if (!row) {
      row = createRow();
      rows.set(s.url, row);
    }
    updateRow(row, s);
    ordered.push(row.li);
  }
  for (const url of [...rows.keys()]) {
    if (!seen.has(url)) rows.delete(url);
  }
  for (const url of [...startErrors.keys()]) {
    if (!seen.has(url)) startErrors.delete(url);
  }
  const current = streamsList.children;
  if (current.length !== ordered.length || ordered.some((li, i) => current[i] !== li)) {
    setChildren(streamsList, ordered);
  }
}

function setStreams(value) {
  streams = Array.isArray(value) ? value : [];
  render();
}

// ── Actions ─────────────────────────────────────────────────────────────────

async function startDownload(url) {
  const s = streams.find(x => x.url === url);
  if (pendingDownloads.has(url) || s?.job) return;
  pendingDownloads.add(url);
  startErrors.delete(url);
  render();
  try {
    const res = await sendMessage({ type: 'download', url });
    if (res?.error) startErrors.set(url, res.error);
    else if (!res?.job_id) startErrors.set(url, 'no response');
  } catch (e) {
    startErrors.set(url, e.message);
  } finally {
    pendingDownloads.delete(url);
    render();
  }
}

function openPage(s) {
  if (s?.pageUrl && s.pageUrl !== 'unknown') chrome.tabs.create({ url: s.pageUrl });
}

function openFile(s) {
  const output = s?.job?.status === 'done' ? s.job.output : null;
  if (!output) return Promise.resolve();
  return vsdServerJson('/open', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path: output })
  }).catch(err => console.error('Open file failed:', err));
}

function removeStream(url) {
  return sendMessage({ type: 'remove', url }).catch(() => {});
}

// ── Button Handlers ─────────────────────────────────────────────────────────

clearBtn.onclick = () => sendMessage({ type: 'clear', protectActive: protectActive.checked }).catch(() => {});

clearSavedBtn.onclick = () => sendMessage({ type: 'clearSaved' }).catch(() => {});

clearLogsBtn.onclick = () => {
  startErrors.clear();
  return sendMessage({ type: 'clearAllData' }).catch(() => {});
};

copyLogsBtn.onclick = async () => {
  const originalText = copyLogsBtn.textContent;
  copyLogsBtn.textContent = 'Copying...';
  copyLogsBtn.disabled = true;
  try {
    const storageDump = await new Promise(r => chrome.storage.local.get(null, r));
    const errorLog = storageDump.errorLog || [];
    delete storageDump.errorLog;

    const errorLogText = errorLog.length
      ? errorLog.map(e => `[${e.timestamp}] (${e.context}/${e.level}) ${e.message}${e.stack ? '\n' + e.stack : ''}`).join('\n')
      : '(none)';

    let serverLog = '';
    let serverAvailable = true;
    try {
      serverLog = (await vsdServerJson('/logs?lines=200', {}, 1500)).log || '';
    } catch (e) {
      serverAvailable = false;
    }

    const report = [
      '=== VSD Diagnostics ===',
      `Generated: ${new Date().toISOString()}`,
      `Server: ${await vsdGetServerUrl()}`,
      '', '--- Extension Storage ---', JSON.stringify(storageDump, null, 2),
      '', '--- Error Log ---', errorLogText,
      '', serverAvailable ? '--- Server Log ---' : '--- Server ---',
      serverAvailable ? serverLog : 'Server unavailable.',
    ].join('\n');

    const ok = await copyToClipboard(report);
    copyLogsBtn.textContent = ok ? 'Copied!' : 'Copy failed';
  } catch (e) {
    copyLogsBtn.textContent = 'Copy failed';
  } finally {
    setTimeout(() => { copyLogsBtn.textContent = originalText; copyLogsBtn.disabled = false; }, 1500);
  }
};

popoutBtn.onclick = () => {
  const api = typeof browser !== 'undefined' && browser.windows ? browser : chrome;
  api.windows.create({
    url: chrome.runtime.getURL('popup.html?popout=1'),
    type: 'popup',
    width: 420, height: 600,
    left: Math.max(0, window.screen.availLeft + window.screen.availWidth - 420),
    top: window.screen.availTop || 0
  });
};

// ── Toggles ─────────────────────────────────────────────────────────────────

detectionToggle.onchange = () => {
  detectionEnabled = detectionToggle.checked;
  chrome.storage.local.set({ detectionEnabled });
  updateStatus();
};

protectActive.onchange = () => chrome.storage.local.set({ protectActiveDownloads: protectActive.checked });

// ── Storage Sync ────────────────────────────────────────────────────────────

chrome.storage.onChanged.addListener((changes, area) => {
  if (area && area !== 'local') return;
  if (changes.detectionEnabled) {
    detectionEnabled = changes.detectionEnabled.newValue !== false;
    detectionToggle.checked = detectionEnabled;
    if (!changes.streams) updateStatus();
  }
  if (changes.protectActiveDownloads) protectActive.checked = changes.protectActiveDownloads.newValue !== false;
  if (changes.streams) setStreams(changes.streams.newValue);
});

// ── Init ────────────────────────────────────────────────────────────────────

chrome.storage.local.get({ streams: [], detectionEnabled: true, protectActiveDownloads: true }, (result) => {
  detectionEnabled = result.detectionEnabled !== false;
  detectionToggle.checked = detectionEnabled;
  protectActive.checked = result.protectActiveDownloads !== false;
  setStreams(result.streams);
});

// Wake the background so it resumes job polling and fills missing titles.
sendMessage({ type: 'refresh' }).catch(() => {});

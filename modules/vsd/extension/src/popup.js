'use strict';

vsdInstallErrorCapture('popup');

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

const SERVER_URL = 'http://127.0.0.1:8765';
const activeJobs = new Map();
const jobProgress = new Map();
let cachedStreams = [];

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

function isJobActive(jobId) {
  const p = jobProgress.get(jobId);
  return !p || p.status === 'running';
}

function fetchWithTimeout(url, ms) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), ms);
  return fetch(url, { signal: controller.signal }).finally(() => clearTimeout(timer));
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

// ── Rendering ───────────────────────────────────────────────────────────────

function render(streams) {
  streamsList.innerHTML = '';
  if (!streams?.length) {
    statusDiv.textContent = 'No HLS streams detected yet.';
    return;
  }
  statusDiv.textContent = `${streams.length} stream(s) detected`;

  for (const s of streams) {
    const li = document.createElement('li');
    li.className = 'stream-item';
    li.title = buildTooltip(s);

    if (s.title) {
      const titleDiv = document.createElement('div');
      titleDiv.className = 'stream-title';
      titleDiv.textContent = s.title;
      li.appendChild(titleDiv);
    }

    const urlDiv = document.createElement('div');
    urlDiv.className = 'stream-url';
    urlDiv.textContent = s.url;

    const jobId = activeJobs.get(s.url);
    const p = jobId ? jobProgress.get(jobId) : null;

    const dlBtn = document.createElement('button');
    if (s.saved && !jobId) {
      dlBtn.textContent = 'Saved';
      dlBtn.disabled = true;
    } else if (jobId && p && (p.status === 'done' || p.status === 'error')) {
      dlBtn.textContent = p.status === 'done' ? 'Saved' : ('Error: ' + (p.error || 'failed'));
      dlBtn.disabled = true;
    } else if (jobId) {
      dlBtn.textContent = formatProgress(p);
      dlBtn.disabled = true;
    } else {
      dlBtn.textContent = 'Download via ffmpeg';
      dlBtn.disabled = false;
    }
    dlBtn.onclick = () => startDownload(s.url, dlBtn);

    const openBtn = document.createElement('button');
    openBtn.textContent = 'Open page';
    openBtn.className = 'open-btn';
    openBtn.onclick = () => {
      if (s.pageUrl && s.pageUrl !== 'unknown') chrome.tabs.create({ url: s.pageUrl });
    };

    const openFileBtn = document.createElement('button');
    openFileBtn.textContent = 'Open file';
    openFileBtn.className = 'open-file-btn';
    openFileBtn.disabled = !(p?.status === 'done' && p.output) && !s.saved;
    openFileBtn.onclick = () => {
      if (p?.output) {
        fetch(`${SERVER_URL}/open`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ path: p.output })
        }).catch(err => console.error('Open file failed:', err));
      }
    };

    const removeBtn = document.createElement('button');
    removeBtn.textContent = 'Remove';
    removeBtn.className = 'remove-btn';
    removeBtn.onclick = () => {
      chrome.runtime.sendMessage({ type: 'remove', url: s.url }, () => {
        if (!chrome.runtime.lastError) { activeJobs.delete(s.url); load(); }
      });
    };

    const actions = document.createElement('div');
    actions.className = 'stream-actions';
    actions.append(dlBtn, openBtn, openFileBtn, removeBtn);
    li.append(urlDiv, actions);
    streamsList.appendChild(li);
  }
}

function renderFromCache() { render(cachedStreams); }

// ── Download & Progress ─────────────────────────────────────────────────────

function startDownload(url, btn) {
  if (btn.disabled) return;
  btn.disabled = true;
  btn.textContent = 'Starting...';

  chrome.runtime.sendMessage({ type: 'download', url }, (res) => {
    if (chrome.runtime.lastError) {
      btn.textContent = 'Error: ' + chrome.runtime.lastError.message;
      btn.disabled = false;
      return;
    }
    if (res?.error) {
      btn.textContent = 'Error: ' + res.error;
      btn.disabled = false;
      return;
    }
    if (res?.job_id) startProgressPolling(url, res.job_id);
  });
}

function startProgressPolling(url, jobId) {
  activeJobs.set(url, jobId);
  jobProgress.set(jobId, { status: 'running', stage: 'starting', current: 0, total: 0 });
  renderFromCache();

  const interval = setInterval(() => {
    chrome.runtime.sendMessage({ type: 'progress', job_id: jobId }, (res) => {
      if (chrome.runtime.lastError) return;
      jobProgress.set(jobId, res);
      if (res.status === 'done' || res.status === 'error') {
        clearInterval(interval);
        if (res.status === 'done') {
          const doneUrl = [...activeJobs.entries()].find(([, jid]) => jid === jobId)?.[0];
          if (doneUrl) chrome.runtime.sendMessage({ type: 'clearJob', url: doneUrl }, () => {});
        } else {
          const errUrl = [...activeJobs.entries()].find(([, jid]) => jid === jobId)?.[0];
          if (errUrl && !cachedStreams.some(s => s.url === errUrl)) {
            chrome.runtime.sendMessage({ type: 'removeUrls', urls: [errUrl] }, () => {});
            activeJobs.delete(errUrl);
            jobProgress.delete(jobId);
          }
        }
      }
      renderFromCache();
    });
  }, 1000);
}

// ── Data Loading ────────────────────────────────────────────────────────────

function load() {
  chrome.runtime.sendMessage({ type: 'getActiveJobs' }, (storedJobs) => {
    if (!chrome.runtime.lastError) {
      for (const [url, jobId] of Object.entries(storedJobs || {})) {
        if (!activeJobs.has(url)) startProgressPolling(url, jobId);
      }
    }
    chrome.runtime.sendMessage({ type: 'getStreams' }, (streams) => {
      if (chrome.runtime.lastError) return;
      cachedStreams = streams || [];
      render(cachedStreams);
    });
  });
}

// ── Button Handlers ─────────────────────────────────────────────────────────

clearBtn.onclick = () => {
  const keepUrls = protectActive.checked
    ? [...activeJobs.entries()].filter(([, jobId]) => isJobActive(jobId)).map(([url]) => url)
    : [];
  chrome.runtime.sendMessage({ type: 'clear', keepUrls }, () => {
    if (chrome.runtime.lastError) return;
    for (const [url, jobId] of activeJobs.entries()) {
      if (!keepUrls.includes(url)) { activeJobs.delete(url); jobProgress.delete(jobId); }
    }
    load();
  });
};

clearSavedBtn.onclick = () => {
  chrome.runtime.sendMessage({ type: 'clearSaved', jobProgress: Object.fromEntries(jobProgress) }, () => {
    if (chrome.runtime.lastError) return;
    for (const [url, jobId] of activeJobs.entries()) {
      if (jobProgress.get(jobId)?.status === 'done') { activeJobs.delete(url); jobProgress.delete(jobId); }
    }
    for (const s of cachedStreams) {
      if (s.saved) { const jid = activeJobs.get(s.url); activeJobs.delete(s.url); if (jid) jobProgress.delete(jid); }
    }
    load();
  });
};

clearLogsBtn.onclick = () => {
  chrome.runtime.sendMessage({ type: 'clearAllData' }, () => {
    if (chrome.runtime.lastError) return;
    activeJobs.clear();
    jobProgress.clear();
    cachedStreams = [];
    load();
  });
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
      const res = await fetchWithTimeout(`${SERVER_URL}/logs?lines=200`, 1500);
      if (res.ok) { serverLog = (await res.json()).log || ''; }
      else serverAvailable = false;
    } catch (e) { serverAvailable = false; }

    const report = [
      '=== VSD Diagnostics ===',
      `Generated: ${new Date().toISOString()}`,
      '', '--- Extension Storage ---', JSON.stringify(storageDump, null, 2),
      '', '--- Job Progress ---', JSON.stringify(Object.fromEntries(jobProgress), null, 2),
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
    url: chrome.runtime.getURL('popup.html'),
    type: 'popup',
    width: 420, height: 600,
    left: Math.max(0, window.screen.availLeft + window.screen.availWidth - 420),
    top: window.screen.availTop || 0
  });
};

// ── Detection Toggle ────────────────────────────────────────────────────────

chrome.storage.local.get({ detectionEnabled: true }, (result) => {
  detectionToggle.checked = result.detectionEnabled;
  statusDiv.textContent = result.detectionEnabled ? 'Scanning...' : 'Detection paused';
});

detectionToggle.onchange = () => {
  const enabled = detectionToggle.checked;
  chrome.storage.local.set({ detectionEnabled: enabled });
  statusDiv.textContent = enabled ? 'Scanning...' : 'Detection paused';
};

chrome.storage.local.get({ protectActiveDownloads: true }, (result) => {
  protectActive.checked = result.protectActiveDownloads;
});
protectActive.onchange = () => chrome.storage.local.set({ protectActiveDownloads: protectActive.checked });

chrome.storage.onChanged.addListener((changes) => {
  if (changes.detectionEnabled) {
    detectionToggle.checked = changes.detectionEnabled.newValue;
    statusDiv.textContent = changes.detectionEnabled.newValue ? 'Scanning...' : 'Detection paused';
  }
  if (changes.protectActiveDownloads) protectActive.checked = changes.protectActiveDownloads.newValue;
});

// ── Init ────────────────────────────────────────────────────────────────────

load();
setInterval(load, 2000);

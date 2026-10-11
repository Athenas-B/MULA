'use strict';

// Shared configuration and server helpers.
// Loaded as a classic script (importScripts / background scripts list / <script>),
// so everything defined here is a global, like errorlog.js.

const VSD_DEFAULT_SERVER = 'http://127.0.0.1:8765';

/** The companion server base URL from storage (without trailing slashes), or the default. */
function vsdGetServerUrl() {
  return new Promise((resolve) => {
    try {
      chrome.storage.local.get({ serverUrl: VSD_DEFAULT_SERVER }, (result) => {
        const url = String((result && result.serverUrl) || '').trim().replace(/\/+$/, '');
        resolve(url || VSD_DEFAULT_SERVER);
      });
    } catch (e) {
      resolve(VSD_DEFAULT_SERVER);
    }
  });
}

/**
 * fetch() `${server}${path}` and return the parsed JSON body ({} when it is not JSON).
 * Throws Error(body.error || `Server error: <status>`) for non-2xx responses (with
 * `err.status` set). An optional `timeoutMs` aborts requests that take too long.
 */
async function vsdServerJson(path, init = {}, timeoutMs = 0) {
  const base = await vsdGetServerUrl();
  let options = init;
  let timer = null;
  if (timeoutMs > 0 && typeof AbortController === 'function') {
    const controller = new AbortController();
    timer = setTimeout(() => controller.abort(), timeoutMs);
    options = { ...init, signal: controller.signal };
  }
  try {
    const res = await fetch(`${base}${path}`, options);
    let data = await res.json().catch(() => ({}));
    if (!data || typeof data !== 'object') data = {};
    if (!res.ok) {
      const err = new Error(data.error || `Server error: ${res.status}`);
      err.status = res.status;
      throw err;
    }
    return data;
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}

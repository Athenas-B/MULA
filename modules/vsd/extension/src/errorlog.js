'use strict';

// Shared error capture utility.
// Persists errors/warnings to chrome.storage.local for diagnostics.

const VSD_ERROR_LOG_KEY = 'errorLog';
const VSD_ERROR_LOG_MAX = 100;

function vsdAppendErrorLog(entry) {
  try {
    chrome.storage.local.get({ [VSD_ERROR_LOG_KEY]: [] }, (result) => {
      const log = result[VSD_ERROR_LOG_KEY] || [];
      log.push(entry);
      while (log.length > VSD_ERROR_LOG_MAX) log.shift();
      chrome.storage.local.set({ [VSD_ERROR_LOG_KEY]: log });
    });
  } catch (e) {}
}

function vsdInstallErrorCapture(context) {
  const originalError = console.error.bind(console);
  const originalWarn = console.warn.bind(console);

  console.error = (...args) => {
    vsdAppendErrorLog({
      timestamp: new Date().toISOString(), context, level: 'error',
      message: args.map(a => (a instanceof Error ? (a.stack || a.message) : String(a))).join(' ')
    });
    originalError(...args);
  };

  console.warn = (...args) => {
    vsdAppendErrorLog({
      timestamp: new Date().toISOString(), context, level: 'warn',
      message: args.map(a => (a instanceof Error ? (a.stack || a.message) : String(a))).join(' ')
    });
    originalWarn(...args);
  };

  const globalTarget = typeof window !== 'undefined' ? window : self;

  globalTarget.addEventListener('error', (event) => {
    vsdAppendErrorLog({
      timestamp: new Date().toISOString(), context, level: 'error',
      message: event.message || String(event.error),
      stack: event.error?.stack
    });
  });

  globalTarget.addEventListener('unhandledrejection', (event) => {
    const reason = event.reason;
    vsdAppendErrorLog({
      timestamp: new Date().toISOString(), context, level: 'error',
      message: 'Unhandled rejection: ' + (reason instanceof Error ? reason.message : String(reason)),
      stack: reason instanceof Error ? reason.stack : undefined
    });
  });
}

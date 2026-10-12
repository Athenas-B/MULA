// The Tauri APIs MULA uses (exposed as `window.__TAURI__` by `withGlobalTauri`).

const tauri = window.__TAURI__;

export const { invoke } = tauri.core;
export const { revealItemInDir } = tauri.opener;
export const { open: openDialog } = tauri.dialog;
export const { listen } = tauri.event;

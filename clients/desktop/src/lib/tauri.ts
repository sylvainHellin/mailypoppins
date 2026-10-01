// The one module that touches @tauri-apps/api and the Tauri plugins'
// JavaScript. Everything else imports from here, so a test mocks this single
// module (src/test/tauri-mock.ts).

export { invoke, Channel } from "@tauri-apps/api/core";
export { listen } from "@tauri-apps/api/event";
export { getCurrentWindow } from "@tauri-apps/api/window";
export { homeDir } from "@tauri-apps/api/path";
export type { UnlistenFn } from "@tauri-apps/api/event";
// The native file and folder picker (tauri-plugin-dialog, `dialog:allow-open`).
export { open as openPicker } from "@tauri-apps/plugin-dialog";

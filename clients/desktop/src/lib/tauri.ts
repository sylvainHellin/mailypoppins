// The one module that touches @tauri-apps/api and the Tauri plugins'
// JavaScript. Everything else imports from here, so the fixture backend
// stands in for this single module: in a test (src/test/tauri-mock.ts) and in
// the browser preview (src/fixture/preview.ts).

export { invoke, Channel } from "@tauri-apps/api/core";
export { listen } from "@tauri-apps/api/event";
export { getCurrentWindow } from "@tauri-apps/api/window";
export { homeDir } from "@tauri-apps/api/path";
export type { UnlistenFn } from "@tauri-apps/api/event";
// The native file and folder picker (tauri-plugin-dialog, `dialog:allow-open`).
export { open as openPicker } from "@tauri-apps/plugin-dialog";

/** The URL the reader frame loads for an `mpmsg` document: the scheme the Rust layer serves itself. */
export const readerSrc = (url: string): string => url;

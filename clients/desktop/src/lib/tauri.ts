// The one module that touches @tauri-apps/api. Everything else imports from
// here, so a test mocks this single module (src/test/tauri-mock.ts).

export { invoke, Channel } from "@tauri-apps/api/core";
export { listen } from "@tauri-apps/api/event";
export type { UnlistenFn } from "@tauri-apps/api/event";

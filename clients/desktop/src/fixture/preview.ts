// The browser preview's stand-in for src/lib/tauri.ts: `pnpm preview:browser`
// runs the frontend in an ordinary browser, and vite.config.ts aliases
// `@/lib/tauri` to this module in its `browser` mode only, a mode that refuses
// to build, so no bundle carries it (scripts/check-bundle.ts checks).
// Every command is answered by the fixture backend the tests use, the reader
// frame loads the fixture's renditions, and `window.mpFixture` is the backend
// itself, for whoever drives the page to push events and settle operations
// (clients/desktop/docs/browser-preview.md).

import readerRs from "../../src-tauri/src/reader.rs?raw";
import * as backend from "@/fixture/backend";

export { Channel, getCurrentWindow, homeDir, invoke, listen, openPicker } from "@/fixture/backend";
export type { UnlistenFn } from "@/fixture/backend";

declare global {
  interface Window {
    mpFixture: typeof backend;
  }
}

backend.resetMock();
// As the Rust layer's fixture mode: an editor open is journaled, nothing launched.
backend.mock.editorFixture = true;
window.mpFixture = backend;

/** A string constant of src-tauri/src/reader.rs, read from its source so the preview never holds a copy. */
function readerConstant(name: string): string {
  const m = new RegExp(`pub const ${name}: &str = (?:r#"(.*?)"#|"(.*?)");`).exec(readerRs);
  if (!m) throw new Error(`src-tauri/src/reader.rs has no \`${name}\` constant`);
  return m[1] ?? m[2];
}

const DAEMON_CSP = readerConstant("DAEMON_CSP");
const BRIDGE = readerConstant("BRIDGE");

const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/**
 * The document the frame gets, as reader.rs serves it: the reader CSP with a
 * nonce as the only script source and the bridge under it, ahead of the
 * rendition, or the text document for a message without markup.
 */
function served(doc: backend.ReaderDocument): string {
  // getRandomValues, since a page served over plain http on the tailnet is no secure context and has no randomUUID.
  const nonce = Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");
  const head = `<meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${DAEMON_CSP}; script-src 'nonce-${nonce}'"><script nonce="${nonce}">${BRIDGE}</script>`;
  // After a leading doctype, which keeps the document out of quirks mode.
  if (doc.rendition === "html") return doc.html.replace(/^(\s*<!doctype html>)?/i, (doctype) => `${doctype}${head}`);
  const text = doc.body === null ? "(This message has no stored body.)" : escapeHtml(doc.body);
  return (
    `<!doctype html><html><head>${head}<style>body{margin:16px;font:14px/1.5 -apple-system,system-ui,sans-serif}` +
    `pre{white-space:pre-wrap;word-wrap:break-word;font:inherit;margin:0}</style></head><body><pre>${text}</pre></body></html>`
  );
}

/** The blob URL of each document served so far, so a re-render keeps the frame's URL. */
const blobs = new Map<string, string>();

/**
 * The reader frame's URL for an `mpmsg` document: a blob of what the
 * fixture backend serves for it now, since a browser knows no `mpmsg` scheme.
 */
export function readerSrc(url: string): string {
  const doc = backend.readerDocument(url);
  if (doc === null) return "about:blank";
  const key = JSON.stringify(doc);
  let blob = blobs.get(key);
  if (blob === undefined) {
    blob = URL.createObjectURL(new Blob([served(doc)], { type: "text/html" }));
    blobs.set(key, blob);
  }
  return blob;
}

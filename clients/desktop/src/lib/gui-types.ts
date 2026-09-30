// The Tauri layer's own result, argument and event types, as clients/desktop/docs/rust-layer.md
// defines them. Those with a Rust counterpart in src-tauri are generated from it with ts-rs
// into src/protocol/generated/gui (`pnpm gen:types`) and re-exported here; the protocol
// shapes they embed come from src/protocol/types.ts.

import type { GuiError } from "@/protocol/generated/gui";

export type * from "@/protocol/generated/gui";

/** What `fixture_simulate` takes; the command reads it as a plain string. */
export type FixtureSimulation =
  | "disconnect"
  | "reconnect"
  | "restart"
  | "resync"
  | "new_mail"
  | "shutdown";

/** Narrow an unknown rejection to a GuiError, or wrap it as `internal`. */
export function asGuiError(e: unknown): GuiError {
  if (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    "message" in e &&
    typeof (e as { kind: unknown }).kind === "string"
  ) {
    return e as GuiError;
  }
  return { kind: "internal", message: e instanceof Error ? e.message : String(e) };
}

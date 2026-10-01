// The Tauri layer's own result, argument and event types, as clients/desktop/docs/rust-layer.md
// defines them. Those with a Rust counterpart in src-tauri are generated from it with ts-rs
// into src/protocol/generated/gui (`pnpm gen:types`) and re-exported here; the protocol
// shapes they embed come from src/protocol/types.ts.

import type { EditorLaunch, GuiError } from "@/protocol/generated/gui";

export type * from "@/protocol/generated/gui";

/**
 * What `fixture_simulate` takes; the command reads it as a plain string.
 * `rollback` reverts every fixture mutation, `rollback:<n>` the last n.
 * `editor_save` and `editor_invalid` save the draft `editor_open` last named.
 * `send_fail`, `send_partial` and `send_pending_append` decide the next send;
 * `send_hold:<secs>` is the fixture's `email.send_hold_secs`.
 * `invite_update` and `invite_cancel` change `work`'s steering committee
 * (row 1008) the way a new version or a cancellation of it arriving would;
 * `rsvp_fail` fails the next RSVP with an SMTP error; `rebuild_refused`
 * makes the next contact index rebuild settle `refused_shrunk`;
 * `signature_changed` edits the signature `work` as another window would;
 * `config_invalid` appends a line to config.toml the next reload refuses;
 * `config_absent` restarts the daemon with no config.toml and no account;
 * `oauth_approve` and `oauth_deny` end every waiting device-code sign-in.
 */
export type FixtureSimulation =
  | "disconnect"
  | "reconnect"
  | "restart"
  | "resync"
  | "new_mail"
  | "shutdown"
  | "rollback"
  | `rollback:${number}`
  | "hold"
  | "editor_save"
  | "editor_invalid"
  | "send_fail"
  | "send_partial"
  | "send_pending_append"
  | `send_hold:${number}`
  | "invite_update"
  | "invite_cancel"
  | "rsvp_fail"
  | "rebuild_refused"
  | "signature_changed"
  | "config_invalid"
  | "config_absent"
  | "oauth_approve"
  | "oauth_deny";

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

/**
 * The notice of an editor handoff the Rust layer only journaled: in fixture
 * mode (`MP_DESKTOP_FIXTURE`) it launches nothing and answers the command
 * it would have run. Null for an editor that started.
 */
export function fixtureNotice(launch: EditorLaunch): string | null {
  return launch.fixture ? `Fixture mode: the editor was not launched; the command would have been ${launch.editor}.` : null;
}

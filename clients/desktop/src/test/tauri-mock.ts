// The fixture-backed stand-in for src/lib/tauri.ts. It answers every command
// from clients/desktop/fixtures/*.json the way src-tauri/src/fixture.rs does,
// holds the one event Channel, and lets a test push GuiEvents and menu items.

import { vi } from "vitest";
import accountsFx from "../../fixtures/accounts.json";
import bootstrapFx from "../../fixtures/bootstrap.json";
import calendarFx from "../../fixtures/calendar.json";
import configFx from "../../fixtures/config.json";
import contactsFx from "../../fixtures/contacts.json";
import draftBodiesFx from "../../fixtures/draft-bodies.json";
import draftsFx from "../../fixtures/drafts.json";
import htmlFx from "../../fixtures/html.json";
import messagesFx from "../../fixtures/messages.json";
import signaturesFx from "../../fixtures/signatures.json";
import type {
  AgendaEvent,
  Bootstrap,
  DraftCreated,
  DraftEntry,
  DraftInvalid,
  DraftListing,
  DraftMessage,
  DraftPreview,
  EventFrontmatter,
  HoldStatus,
  MessageListRow,
  OutboxListing,
  OutboxRow,
} from "@/protocol/types";
import type {
  AccountDraft,
  AccountInfo,
  ConfigSnapshot,
  ConnectionStatus,
  ContactRow,
  GuiError,
  GuiEvent,
  InterceptedUrl,
  LocalSearchHit,
  MailboxListing,
  MessageList,
  MessageMeta,
  MessageText,
  MutationAck,
  MutationBatch,
  VersionInfo,
} from "@/lib/gui-types";

type FixtureRow = MessageListRow & { body?: string; attachments?: { name: string; size: number }[] };

export const fixtures = {
  bootstrap: bootstrapFx as unknown as Bootstrap,
  accounts: accountsFx.accounts as { name: string; default: boolean; backend: "imap" | "graph"; state: string }[],
  messages: messagesFx as unknown as Record<string, Record<string, FixtureRow[]>>,
  drafts: draftsFx as unknown as Record<string, DraftListing>,
  draftBodies: draftBodiesFx as Record<string, string>,
  signatures: signaturesFx as { signatures: Record<string, string>; defaults: Record<string, string> },
  calendar: calendarFx as unknown as { events: Record<string, AgendaEvent[]>; ics: Record<string, string> },
  contacts: contactsFx as unknown as Record<string, Omit<ContactRow, "recipient">[]>,
  config: configFx as unknown as ConfigSnapshot["config"],
};

export class Channel<T> {
  onmessage: (message: T) => void = () => {};
}

const clone = <T,>(v: T): T => structuredClone(v);

/**
 * The outbox rows the fixture seeds from the bootstrap's counts: `home`
 * has one message queued and never submitted, row 1.
 */
function seedOutbox(): Record<string, { ever_used: boolean; rows: OutboxRow[] }> {
  const out: Record<string, { ever_used: boolean; rows: OutboxRow[] }> = {};
  let id = 1;
  for (const [account, counts] of Object.entries(fixtures.bootstrap.snapshot.outbox)) {
    if (counts.queued === 0) continue;
    out[account] = {
      ever_used: true,
      rows: Array.from({ length: counts.queued }, (_, n) => ({
        id: id++,
        state: "pending_send",
        partial: false,
        never_submitted: true,
        message_id: `<queued-${n}@${account}.fixture.example>`,
        target_mailbox: null,
        updated: 1_790_000_000,
        last_error: null,
        rejected: [],
        outstanding: ["friend@example.com"],
      })),
    };
  }
  return out;
}

/** A listing as the fixture's `send.outbox_list` answers it, counts from the rows. */
export function outboxListing(account: string): OutboxListing {
  const o = mock.outbox[account];
  const rows = clone(o?.rows ?? []).filter((r) => r.state !== "done" || r.partial);
  return {
    account,
    ever_used: o?.ever_used ?? false,
    rows,
    counts: {
      open: rows.filter((r) => r.state === "pending_send" || r.state === "sent_pending_append").length,
      failed: rows.filter((r) => r.state === "failed").length,
      partial: rows.filter((r) => r.partial).length,
    },
  };
}

export const mock = {
  connection: { state: "connected", instance_id: "fixture-instance-1", daemon_version: "0.0.0-fixture", protocol: 1, fixture: true } as ConnectionStatus,
  channel: null as Channel<GuiEvent> | null,
  menu: null as ((e: { payload: string }) => void) | null,
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  /** Fail a command with a GuiError-shaped rejection. */
  failing: new Map<string, unknown>(),
  /** Added to every row_id, as a daemon restart that rebuilt the store would. */
  rowShift: 0,
  /** What `intercepted_urls` drains. */
  interceptLog: [] as InterceptedUrl[],
  /** The next server search's operation id counter. */
  nextOp: 1,
  /** What `search_server_cancel` answers. */
  cancelOutcome: "cancelled" as "cancelled" | "already_settled",
  /** The rows the commands read and the mutations change, fresh per test. */
  rows: clone(fixtures.messages),
  drafts: clone(fixtures.drafts),
  /** The holds `send_hold_status` lists and `send_cancel_hold` stops. */
  holds: clone(fixtures.bootstrap.snapshot.holds) as HoldStatus[],
  /** The revision the mock's own events carry. */
  revision: 1000,
  /** The next sync's operation id counter. */
  nextSync: 1,
  /**
   * Hold the next answer of a command: it is computed at the call, from the
   * rows as they are then, and delivered once the promise settles, as a read
   * that overtook a later write would be.
   */
  gates: new Map<string, Promise<unknown>>(),
  /** Every path `editor_open` was asked to open, in order. */
  editorOpens: [] as string[],
  /** What `editor_open` rejects with instead of opening, once. */
  editorFailure: null as GuiError | null,
  /** Whether the editor commands answer as the Rust layer's fixture mode does: journaled, nothing launched. */
  editorFixture: false,
  /** A draft's fields the listing does not carry, by `<account>/<id>`. */
  draftExtra: {} as Record<string, { bcc: string; body: string }>,
  /** The next minted draft id's counter. */
  nextDraft: 1,
  /** `desktop.json`'s keys, as `setting_get|set` and `editor_setting_get|set` read and write them. */
  settings: new Map<string, string>(),
  /**
   * Whether `send_draft` and `send_approved` arm a hold, as a daemon with
   * `email.send_hold_secs` above 0 does; the hold's first event is emitted
   * before the command answers, as the Rust layer can deliver it.
   */
  sendHeld: true,
  /** The next send's operation id counter. */
  nextSend: 1,
  /** Each account's outbox rows; `outbox_retry` leaves them as they are, a test settles it. */
  outbox: {} as Record<string, { ever_used: boolean; rows: OutboxRow[] }>,
  /** The next outbox retry's operation id counter. */
  nextRetry: 1,
  /** Every file the system opener was asked to open, in order. */
  opened: [] as string[],
  /** The files on the mock's disk, which `draft_attach` checks; `~` is {@link MOCK_HOME}. */
  files: new Set<string>(),
  /** Each draft's `attachments:` entries, by `<account>/<id>`. */
  draftAttachments: {} as Record<string, string[]>,
  /** The names each save directory holds, for the `_1` rule. */
  savedIn: {} as Record<string, string[]>,
  /** The next materialised handle's counter. */
  nextHandle: 1,
  /** Each account's agenda, what `calendar_events` answers once sorted. */
  calendar: clone(fixtures.calendar.events),
  /** The `invite.ics` of a row, by row id. */
  ics: clone(fixtures.calendar.ics),
  /** The event an email no agenda row stands for carries, by row id (`simulateInvite`'s emails). */
  invites: {} as Record<string, EventFrontmatter>,
  /** The RSVPs `calendar_rsvp` started and no test settled yet, oldest first. */
  rsvps: [] as { operation_id: string; account: string; row_id: number; response: string }[],
  /** The next RSVP's operation id counter. */
  nextRsvp: 1,
  /** The invitations `send_invite` started and no test settled yet, oldest first. */
  invitesSent: [] as { operation_id: string; account: string; subject: string; to: string[] }[],
  /** The next invitation's operation id counter. */
  nextInvite: 1,
  /** Each account's contact index, ranked, as `contact.search` rows. */
  contacts: clone(fixtures.contacts),
  /** The rebuilds `contact_rebuild` started and no test settled yet, oldest first. */
  rebuilds: [] as { operation_id: string; account: string }[],
  /** The next rebuild's operation id counter. */
  nextRebuild: 1,
  /** The vCard drafts `contact_vcard_draft` wrote, oldest first. */
  vcards: [] as { account: string; id: string; vcf: string }[],
  /** The signatures and each account's default, what the `signature_*` commands read and change. */
  signatures: clone(fixtures.signatures),
  /** `config.get`'s `state`: `absent` makes `config_open` refuse with fixture.rs's `NO_CONFIG`. */
  configState: "ok" as "ok" | "absent" | "invalid",
  /** Whether the daemon's log file exists, which `log_open` checks. */
  logExists: true,
  /** `config.get`'s `revision`, which every accepted `config_reload` moves. */
  configRevision: 0,
  /** fixture.rs's `config_invalid`: config.toml holds a line the next `config_reload` refuses. */
  configInvalid: false,
  /** Every `config_set_password`, as fixture.rs journals it: the value redacted. */
  passwords: [] as { account: string; kind: string; value: string }[],
  /** What `subscribe_events` and `bootstrap` answer; `config_add_account` and `config_init` add to it. */
  bootstrap: clone(fixtures.bootstrap),
  /** What `account_list` lists. */
  accounts: clone(fixtures.accounts),
  /** The effective configuration `config_get` answers. */
  config: clone(fixtures.config),
  /** The sign-ins `config_oauth2_login` started and no test ended yet, oldest first. */
  signIns: [] as { operation_id: string; account: string; kind: "oauth2" | "graph" }[],
  /** The next sign-in's operation id counter. */
  nextSignIn: 1,
};

/** fixture.rs's `DEVICE_CODE_MESSAGE`, the device-code progress's message. */
export const DEVICE_CODE_MESSAGE = "https://microsoft.com/devicelogin FXTR-CODE";

/** fixture.rs's `OAUTH_DENIED`, why an `oauth_deny` sign-in failed. */
export const OAUTH_DENIED = "Authorization was declined by the user.";

/**
 * fixture.rs's `config_absent`, before the app starts: the daemon has no
 * config.toml and serves no account.
 */
export function simulateConfigAbsent(): void {
  mock.configState = "absent";
  mock.accounts = [];
  mock.config = { ...clone(fixtures.config), accounts: [] };
  mock.bootstrap = {
    ...clone(fixtures.bootstrap),
    snapshot: { ...clone(fixtures.bootstrap.snapshot), accounts: [], mailboxes: {}, drafts: {}, outbox: {}, holds: [], diagnostics: [] },
  };
  mock.holds = [];
  mock.outbox = {};
}

/** The oldest waiting sign-in reports its device code, as fixture.rs does after `DEVICE_CODE_DELAY`. */
export function reportDeviceCode(): string {
  const run = mock.signIns[0];
  if (!run) throw new Error("no sign-in is waiting for its code");
  emitEnvelope("operation.progress", { operation_id: run.operation_id, phase: "device_code", done: 0, total: null, message: DEVICE_CODE_MESSAGE });
  return run.operation_id;
}

/** fixture.rs's `oauth_approve` (stored) or `oauth_deny` (failed): the oldest waiting sign-in ends. */
export function settleSignIn(opts: { deny?: boolean } = {}): string {
  const run = mock.signIns.shift();
  if (!run) throw new Error("no sign-in is waiting to settle");
  if (opts.deny) {
    emitEnvelope("operation.finished", { operation_id: run.operation_id, state: "failed", error: { code: -32603, message: OAUTH_DENIED } });
  } else {
    emitEnvelope("operation.finished", {
      operation_id: run.operation_id,
      state: "succeeded",
      result: { stored: true, account: run.account, kind: run.kind, key: `oauth2-token-${run.account}` },
    });
  }
  return run.operation_id;
}

/** fixture.rs's `configure_account`: the account is served at once, ready, with Inbox, Archive and Sent. */
function configureAccount(account: AccountDraft): string {
  const name = account.name;
  const auth = account.auth_method ?? "password";
  mock.accounts.push({ name, default: mock.accounts.length === 0, backend: auth === "graph" ? "graph" : "imap", state: "ready" });
  mock.bootstrap.snapshot.accounts.push({ name, state: "ready", sync_health: { state: "ok" } } as Bootstrap["snapshot"]["accounts"][number]);
  mock.bootstrap.snapshot.mailboxes[name] = [
    { role: "inbox", slug: "inbox", label: "Inbox", total: 0, unread: 0, badge: 0 },
    { role: "archive", slug: "archive", label: "Archive", total: 0, unread: 0, badge: 0 },
    { role: "sent", slug: "sent", label: "Sent", total: 0, unread: 0, badge: 0 },
  ] as Bootstrap["snapshot"]["mailboxes"][string];
  mock.rows[name] = { inbox: [], archive: [], sent: [] };
  const server = (s: AccountDraft["smtp"], port: number) => ({ host: s?.host ?? "", port: s?.port ?? port, username: s?.username ?? "" });
  mock.config.accounts.push({
    name,
    default_from: account.default_from ?? "",
    auth_method: auth,
    smtp: server(account.smtp, 465),
    imap: server(account.imap, 993),
    oauth2: account.oauth2 ?? null,
  } as ConfigSnapshot["config"]["accounts"][number]);
  mock.configState = "ok";
  mock.configRevision += 1;
  emitEnvelope("config.changed", { added: [name], updated: [], removed: [], config_revision: mock.configRevision });
  return name;
}

/** fixture.rs's `CONFIG_INVALID_MESSAGE`, why `config_reload` refuses after `config_invalid`. */
export const CONFIG_INVALID_MESSAGE = "key with no value, expected `=`";

/** The line of config.toml the mock's `config_invalid` breaks: the template's 29 lines, then the appended one. */
export const CONFIG_INVALID_AT = 30;

/** Where `config_open` opens config.toml in the mock, `config.get`'s `path`. */
export const MOCK_CONFIG_PATH = "/fixture/config.toml";

/** Where `log_open` opens the daemon log in the mock, `diagnostic.log_path`'s answer. */
export const MOCK_LOG_PATH = "/fixture/logs/mailypoppins-2026-09-30.log";

/** daemon_files.rs's `NO_CONFIG`. */
export const NO_CONFIG = "There is no config.toml yet; add an account first";

/** fixture.rs's `GRAPH_INVITE_REFUSAL`, the daemon's `send.invite` refusal of a Graph account. */
export const GRAPH_INVITE_REFUSAL =
  "`mp send --invite` is not supported for Graph accounts yet (Graph calendar send is tracked by #0036, blocked on #0035). Use an SMTP-configured account.";

/** fixture.rs's `GRAPH_RSVP_REFUSAL`, what the probe answers for `home`. */
export const GRAPH_RSVP_REFUSAL = "RSVP is not supported for Graph accounts yet (#0036, blocked on #0035)";

/** fixture.rs's `SEND_FAIL_REASON`, what `rsvp_fail` fails an RSVP with. */
export const SEND_FAIL_REASON = "421 4.7.0 fixture: the server closed the connection";

/** The home directory `~` expands to in the mock. */
export const MOCK_HOME = "/home/fixture";

/** The files the mock's disk starts with. */
export const MOCK_FILES = [`${MOCK_HOME}/Documents/report.pdf`, `${MOCK_HOME}/Documents/plan.pdf`];

export function resetMock(): void {
  mock.connection = { state: "connected", instance_id: "fixture-instance-1", daemon_version: "0.0.0-fixture", protocol: 1, fixture: true };
  mock.channel = null;
  mock.menu = null;
  mock.calls = [];
  mock.failing.clear();
  mock.rowShift = 0;
  mock.interceptLog = [];
  mock.nextOp = 1;
  mock.cancelOutcome = "cancelled";
  mock.rows = clone(fixtures.messages);
  mock.drafts = clone(fixtures.drafts);
  mock.holds = clone(fixtures.bootstrap.snapshot.holds);
  mock.revision = 1000;
  mock.nextSync = 1;
  mock.gates.clear();
  mock.editorOpens = [];
  mock.editorFailure = null;
  mock.editorFixture = false;
  mock.draftExtra = {};
  mock.nextDraft = 1;
  mock.settings.clear();
  mock.sendHeld = true;
  mock.nextSend = 1;
  mock.outbox = seedOutbox();
  mock.nextRetry = 1;
  mock.opened = [];
  mock.files = new Set(MOCK_FILES);
  mock.draftAttachments = {};
  mock.savedIn = {};
  mock.nextHandle = 1;
  mock.calendar = clone(fixtures.calendar.events);
  mock.ics = clone(fixtures.calendar.ics);
  mock.invites = {};
  mock.rsvps = [];
  mock.nextRsvp = 1;
  mock.invitesSent = [];
  mock.nextInvite = 1;
  mock.contacts = clone(fixtures.contacts);
  mock.rebuilds = [];
  mock.nextRebuild = 1;
  mock.vcards = [];
  mock.signatures = clone(fixtures.signatures);
  mock.configState = "ok";
  mock.logExists = true;
  mock.configRevision = 0;
  mock.configInvalid = false;
  mock.passwords = [];
  mock.bootstrap = clone(fixtures.bootstrap);
  mock.accounts = clone(fixtures.accounts);
  mock.config = clone(fixtures.config);
  mock.signIns = [];
  mock.nextSignIn = 1;
}

/** `mp_core::addresses::format_recipient`: the name quoted when it holds a character outside atext and spaces. */
export function formatRecipient(name: string, address: string): string {
  const n = name.trim();
  if (!n) return address;
  const atext = /^[A-Za-z0-9!#$%&'*+\-/=?^_`{|}~. \t]*$/.test(n);
  const quoted = atext ? n : `"${n.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
  return `${quoted} <${address}>`;
}

/**
 * fixture.rs's rebuild end: the oldest rebuild not settled yet settles
 * `written` with the index's size, or with `saved` the cache guard's
 * refusal (`refused_shrunk`: 3 found; `refused_empty`: none; the index
 * kept either way), or with `fail` fails with that message.
 */
export function settleRebuild(opts: { saved?: "refused_empty" | "refused_shrunk"; fail?: string } = {}): string {
  const run = mock.rebuilds.shift();
  if (!run) throw new Error("no rebuild is waiting to settle");
  if (opts.fail) {
    emitEnvelope("operation.finished", { operation_id: run.operation_id, state: "failed", error: { code: -32603, message: opts.fail } });
    return run.operation_id;
  }
  const indexed = (mock.contacts[run.account] ?? []).length;
  const saved = opts.saved ?? "written";
  const found = saved === "refused_shrunk" ? 3 : saved === "refused_empty" ? 0 : indexed;
  emitEnvelope("operation.finished", {
    operation_id: run.operation_id,
    state: "succeeded",
    result: {
      account: run.account,
      contacts: found,
      kept: saved === "written" ? 0 : indexed,
      saved,
      cache_path: `/fixture/accounts/${run.account}/contacts-cache.json`,
    },
  });
  return run.operation_id;
}

/** fixture.rs's invitation end: the oldest invitation settles with every recipient delivered, or fails with `fail`. */
export function settleInvite(opts: { fail?: boolean; refused?: number } = {}): string {
  const run = mock.invitesSent.shift();
  if (!run) throw new Error("no invitation is waiting to settle");
  if (opts.fail) {
    emitEnvelope("operation.finished", { operation_id: run.operation_id, state: "failed", error: { code: -32603, message: SEND_FAIL_REASON } });
    return run.operation_id;
  }
  const refused = opts.refused ?? 0;
  emitEnvelope("state.invalidate", { resource: `mailbox:${run.account}/sent`, scope: { query: "counts" } });
  emitEnvelope("operation.finished", {
    operation_id: run.operation_id,
    state: "succeeded",
    result: {
      account: run.account,
      selector: null,
      message_id: `<${run.operation_id}@fixture.example>`,
      status_line: "sent + saved",
      recipients: run.to.map((address, i) => ({
        address,
        role: "To",
        delivered: i < run.to.length - refused,
        error: i < run.to.length - refused ? null : "550 5.1.1 fixture: no such mailbox",
      })),
      sent_copy: "filed",
      settle_error: null,
    },
  });
  return run.operation_id;
}

/** The event row `rowId` of `account` carries: its agenda row's, else the version an email delivered. */
function inviteEvent(account: string, rowId: number): EventFrontmatter | null {
  const row = (mock.calendar[account] ?? []).find((e) => e.row_id === rowId);
  if (row) return clone(row.event);
  return findRow(account, rowId) ? clone(mock.invites[String(rowId)] ?? null) : null;
}

/**
 * fixture.rs's RSVP end: the oldest RSVP not settled yet (or `operationId`)
 * settles with the agenda row's reply changed and `state.invalidate` of the
 * account's Sent mailbox, or with `fail`, fails with an SMTP error after a
 * `failed` outbox row, as after `rsvp_fail`. `delivered: false` settles a
 * reply no recipient took yet.
 */
export function settleRsvp(opts: { fail?: boolean; delivered?: boolean; operationId?: string } = {}): string {
  const at = opts.operationId ? mock.rsvps.findIndex((r) => r.operation_id === opts.operationId) : 0;
  const [run] = mock.rsvps.splice(at, 1);
  if (!run) throw new Error("no RSVP is waiting to settle");
  const e = (mock.calendar[run.account] ?? []).find((x) => x.row_id === run.row_id)?.event ?? mock.invites[String(run.row_id)];
  if (opts.fail) {
    const o = (mock.outbox[run.account] ??= { ever_used: true, rows: [] });
    o.rows.push({
      id: 100 + mock.nextRsvp,
      state: "failed",
      partial: false,
      never_submitted: false,
      message_id: `<fixture-rsvp-${run.operation_id}@fixture.example>`,
      target_mailbox: "Sent",
      updated: 1_790_000_000,
      last_error: SEND_FAIL_REASON,
      rejected: [],
      outstanding: [e?.organizer ?? ""],
    });
    emitEnvelope("state.invalidate", { resource: `outbox:${run.account}`, scope: { query: "counts" } });
    emitEnvelope("operation.finished", { operation_id: run.operation_id, state: "failed", error: { code: -32603, message: SEND_FAIL_REASON } });
    return run.operation_id;
  }
  const status = run.response === "accept" ? "accepted" : run.response === "decline" ? "declined" : "tentative";
  if (e) {
    e.rsvp = status;
    for (const a of e.attendees ?? []) if (a.address === "me@example.com") a.status = status;
  }
  emitEnvelope("state.invalidate", { resource: `mailbox:${run.account}/sent`, scope: { query: "counts" } });
  const verb = status === "accepted" ? "Accepted" : status === "declined" ? "Declined" : "Tentative";
  emitEnvelope("operation.finished", {
    operation_id: run.operation_id,
    state: "succeeded",
    result: {
      account: run.account,
      selector: findRow(run.account, run.row_id)?.[1].selector ?? `mp://${run.account}/inbox/fixture-row-${run.row_id}`,
      response: run.response,
      subject: `${verb}: ${e?.summary ?? ""}`,
      organizer: e?.organizer ?? "",
      message_id: `<fixture-rsvp-${run.operation_id}@fixture.example>`,
      delivered: opts.delivered ?? true,
    },
  });
  return run.operation_id;
}

/** The agenda row `invite_update` and `invite_cancel` change, as fixture.rs's `INVITE_ROW`. */
export const INVITE_ROW = 1008;

/** `account`'s agenda in the daemon's order: by start, undated last. */
function agendaOf(account: string): AgendaEvent[] {
  const key = (e: AgendaEvent) => `${e.start_sort === "" ? 1 : 0}${e.start_sort}`;
  return clone(mock.calendar[account] ?? []).sort((a, b) => (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0));
}

/**
 * fixture.rs's `invite_update` and `invite_cancel`: row 1008 of `work`'s
 * agenda changes in place, the email that carried the change lands at the
 * top of `work`'s inbox, and `state.invalidate` of `mailbox:work/inbox` says so.
 */
export function simulateInvite(cancel: boolean): void {
  const e = mock.calendar.work.find((x) => x.row_id === INVITE_ROW)!;
  if (cancel) {
    e.cancelled = true;
    e.event.cancelled = true;
  } else {
    e.event.sequence += 1;
    const day = String(Math.min(14 + e.event.sequence, 28)).padStart(2, "0");
    e.event.start = `2099-10-${day}T10:00:00+02:00`;
    e.event.end = `2099-10-${day}T11:00:00+02:00`;
    e.start_sort = `2099-10-${day}T08:00:00`;
    e.end_sort = `2099-10-${day}T09:00:00`;
    e.start_display = `2099-10-${day} 10:00`;
  }
  const id = Math.max(...Object.values(mock.rows).flatMap((b) => Object.values(b).flatMap((rs) => rs.map((r) => r.id)))) + 1;
  const tag = cancel ? "invite-cancel" : "invite-update";
  const subject = `${cancel ? "Cancelled" : "Updated invitation"}: ${e.event.summary}`;
  mock.ics[String(id)] = `BEGIN:VCALENDAR\r\nMETHOD:${cancel ? "CANCEL" : "REQUEST"}\r\nEND:VCALENDAR\r\n`;
  mock.invites[String(id)] = { ...clone(e.event), method: cancel ? "CANCEL" : "REQUEST" };
  const row = {
    id,
    uid: id,
    message_id: `<${tag}-${id}@fixture.example>`,
    from: "Calendar <calendar@example.com>",
    to: "me@example.com",
    cc: null,
    reply_to: null,
    bcc: null,
    subject,
    date_sort: "2026-09-30T12:00:00",
    date_display: "Wed, 30 Sep 2026 12:00:00 +0200",
    flags: { seen: false, answered: false, forwarded: false, flagged: false },
    has_attachments: true,
    is_invite: true,
    selector: `mp://work/inbox/${tag}-${id}@fixture.example`,
    body: `${subject}.`,
    attachments: [{ name: "invite.ics", size: 400 }],
  } as unknown as FixtureRow;
  mock.rows.work.inbox.unshift(row);
  emitEnvelope("state.invalidate", { resource: "mailbox:work/inbox", scope: { query: "counts" } });
}

const expandHome = (p: string) => (p === "~" ? MOCK_HOME : p.startsWith("~/") ? `${MOCK_HOME}/${p.slice(2)}` : p);

/** A draft's attachments as `draft_attachments` answers them. */
function draftAttachmentsOf(account: string, id: string) {
  const d = findDraft("draft.path", account, id);
  const entries = mock.draftAttachments[`${account}/${id}`] ?? [];
  return {
    account,
    id,
    path: d.path,
    attachments: entries.map((entry, index) => {
      const path = expandHome(entry);
      return { index, entry, path, exists: mock.files.has(path) };
    }),
  };
}

/** The `_1` rule of `mp_core::parse::save_attachment`. */
function saveName(dir: string, name: string): string {
  const taken = (mock.savedIn[dir] ??= []);
  const dot = name.lastIndexOf(".");
  const [stem, ext] = dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, ""];
  let candidate = name;
  for (let n = 1; taken.includes(candidate); n++) candidate = `${stem}_${n}${ext}`;
  taken.push(candidate);
  return `${dir}/${candidate}`;
}

/** Part `part` of a stored row, or the daemon's -32602. */
function partOf(method: string, account: string, rowId: number, part: number): string {
  const hit = findRow(account, rowId);
  if (!hit) throw refusedRow(method, `no message has row_id ${rowId}`);
  const parts = hit[1].attachments ?? [];
  if (part >= parts.length) throw refusedRow(method, `row ${rowId} has ${parts.length} attachments, no part ${part}`);
  return parts[part].name;
}

/** The hold the mock arms for a send, 20 s as the daemon's default window. */
function armSendHold(operation_id: string, account: string, d: DraftEntry): HoldStatus {
  const hold: HoldStatus = {
    operation_id,
    account,
    draft_id: d.id,
    subject: d.subject ?? "",
    hold_secs: 20,
    remaining_secs: 20,
    fires_at: "2026-09-30T12:00:20Z",
    origin: "gui",
  };
  mock.holds.push(hold);
  emitEnvelope("send.hold_started", hold);
  return hold;
}

/** `send_draft` as the Rust layer runs it: validate, approve a `draft` status, then `send.draft`. */
function sendDraft(account: string, id: string): unknown {
  knownAccount("send_draft", account);
  const invalid = invalidDraft(account, id);
  if (invalid) throw { ...draftInvalidError("draft.approve", invalid), invalid };
  const d = findDraft("draft.approve", account, id);
  const error = draftPreview(account, id).error;
  if (error) throw { kind: "protocol", code: null, message: `${d.selector} does not validate: ${error}` };
  const approved = d.status !== "approved";
  if (approved) {
    d.status = "approved";
    draftChanged(account, d);
  }
  const operation_id = `fixture-send-${mock.nextSend++}`;
  if (mock.sendHeld) armSendHold(operation_id, account, d);
  return { operation_id, held: mock.sendHeld, approved };
}

/** Push a daemon event on the channel, as the fixture publishes it. */
export function emitEnvelope(kind: string, payload: unknown): void {
  const instance_id = mock.connection.state === "connected" ? mock.connection.instance_id : "fixture-instance-1";
  emit({ type: "event", event: { instance_id, revision: ++mock.revision, kind, payload } });
}

export function emit(event: GuiEvent): void {
  mock.channel?.onmessage(event);
}

export function emitMenu(id: string): void {
  mock.menu?.({ payload: id });
}

function kindOf(role: string): MailboxListing["mailboxes"][number]["kind"] {
  return role === "inbox" || role === "drafts" || role === "sent" || role === "archive" ? role : "extra";
}

/** A fixture row as `message.list` sends it: listed headers are "" when absent. */
function strip(row: FixtureRow): MessageListRow {
  const { body: _b, attachments: _a, ...wire } = row;
  return {
    ...wire,
    from: wire.from ?? "",
    to: wire.to ?? "",
    subject: wire.subject ?? "",
    date_sort: wire.date_sort ?? "",
    date_display: wire.date_display ?? "",
  };
}

function findRow(account: string, rowId: number): [string, FixtureRow] | null {
  for (const [mailbox, rows] of Object.entries(mock.rows[account] ?? {})) {
    const row = rows.find((r) => r.id + mock.rowShift === rowId);
    if (row) return [mailbox, row];
  }
  return null;
}

export function mailboxListing(account: string): MailboxListing {
  const snap = mock.bootstrap.snapshot;
  const rows = snap.mailboxes[account] ?? [];
  const mailboxes = rows.map((m) => {
    const msgs = mock.rows[account]?.[m.slug] ?? [];
    const total = m.role === "drafts" ? (mock.drafts[account]?.drafts.length ?? 0) : msgs.length;
    const unread = m.role === "drafts" ? 0 : msgs.filter((r) => !r.flags.seen).length;
    return { slug: m.slug, label: m.label, role: m.role, kind: kindOf(m.role), total, unread, badge: total };
  });
  const a = snap.accounts.find((x) => x.name === account);
  return {
    account,
    mailboxes,
    total: mailboxes.reduce((n, m) => n + m.total, 0),
    unread: mailboxes.reduce((n, m) => n + m.unread, 0),
    runtime_state: a?.state ?? "opening",
    sync_health: a?.sync_health.state ?? "unknown",
  };
}

/** A refusal about one row, as the Rust layer maps the daemon's -32602. */
function refusedRow(method: string, why: string): GuiError {
  return { kind: "not_found", message: `${method}: the daemon refused the call: ${why} (-32602)`, code: -32602 };
}

/** An unknown account fails the whole command, as `account_unknown` does. */
function knownAccount(cmd: string, account: string): void {
  if (!mock.bootstrap.snapshot.accounts.some((a) => a.name === account)) {
    throw { kind: "not_found", message: `${cmd}: the daemon refused the call: account_unknown: ${account} (-32005)`, code: -32005 };
  }
}

/** Where the mock keeps a signature's file. */
export const signaturePath = (name: string): string => `/fixture/signatures/${name}.md`;

function signatureListing(account: string) {
  return { account, names: Object.keys(mock.signatures.signatures).sort(), default: mock.signatures.defaults[account] ?? null };
}

/** A refusal of `mp_core::signatures`, as the Rust layer hands it over. */
function signatureRefusal(message: string): GuiError {
  return message.startsWith("no signature named") ? { kind: "not_found", message, code: null } : { kind: "protocol", message, code: null };
}

/** `mp_core::signatures::validate_name`, its sentences. */
function signatureName(name: string): string {
  if (name === "") throw signatureRefusal("a signature name cannot be empty");
  if (name.length > 64) throw signatureRefusal(`signature name '${name}' is longer than 64 characters`);
  if (name !== name.trim()) throw signatureRefusal(`signature name '${name}' has leading or trailing whitespace`);
  if (name.startsWith(".")) throw signatureRefusal(`signature name '${name}' cannot start with a dot`);
  if (name.includes("..")) throw signatureRefusal(`signature name '${name}' cannot contain '..'`);
  if (/[/\\]/.test(name)) throw signatureRefusal(`signature name '${name}' cannot contain a path separator`);
  const bad = [...name].find((c) => !/[A-Za-z0-9._ -]/.test(c));
  if (bad) throw signatureRefusal(`signature name '${name}' contains '${bad}'; use letters, digits, '-', '_', '.' or spaces`);
  return name;
}

function signatureKnown(name: string): void {
  if (!(name in mock.signatures.signatures)) throw signatureRefusal(`no signature named '${name}'`);
}

function retargetDefaults(old: string, name: string | null): void {
  for (const [account, current] of Object.entries(mock.signatures.defaults)) {
    if (current !== old) continue;
    if (name === null) delete mock.signatures.defaults[account];
    else mock.signatures.defaults[account] = name;
  }
}

/** fixture.rs's `SIGNATURE_EDIT_LINE`. */
export const SIGNATURE_EDIT_LINE = "Edited behind the fixture's back.";

/** fixture.rs's `signature_changed`: another window edited `work`, and the watcher says so. */
export function simulateSignatureChanged(): void {
  mock.signatures.signatures.work = `${mock.signatures.signatures.work.replace(/\n+$/, "")}\n${SIGNATURE_EDIT_LINE}`;
  emitEnvelope("signature.changed", { name: "work", path: signaturePath("work") });
}

/** One of the five message mutations over `row_ids`, in order, as fixture.rs answers it. */
function mutate(cmd: string, account: string, args: Record<string, unknown>): MutationBatch {
  knownAccount(cmd, account);
  const method = `message.${cmd.slice("message_".length)}`;
  const out: MutationBatch = { done: [], failed: [] };
  const boxes = mock.rows[account];
  for (const rowId of args.row_ids as number[]) {
    const hit = findRow(account, rowId);
    if (!hit) {
      out.failed.push({ row_id: rowId, error: refusedRow(method, `${account} holds no message with row id ${rowId}`) });
      continue;
    }
    const [mailbox, row] = hit;
    const ack: MutationAck = { row_id: rowId, account, id: `${mailbox}/${row.uid}`, selector: row.selector, mailbox };
    let dest: string | null = null;
    if (cmd === "message_archive") dest = "archive";
    if (cmd === "message_move") {
      const wanted = String(args.destination);
      dest = fixtures.bootstrap.snapshot.mailboxes[account]?.find((m) => m.slug === wanted || m.label === wanted)?.slug ?? null;
      if (dest === null) {
        out.failed.push({ row_id: rowId, error: refusedRow(method, `'${wanted}' is not a mailbox of ${account}`) });
        continue;
      }
    }
    const rows = boxes[mailbox];
    if (dest !== null || cmd === "message_delete") rows.splice(rows.indexOf(row), 1);
    if (dest !== null) {
      const selector = `mp://${account}/${dest}/${row.message_id.replace(/^<|>$/g, "")}`;
      (boxes[dest] ??= []).unshift({ ...row, selector });
      ack.moved_to = { mailbox: dest, selector };
    }
    if (cmd === "message_set_flag") {
      row.flags.flagged = Boolean(args.flagged);
      ack.flagged = row.flags.flagged;
    }
    if (cmd === "message_set_read") {
      row.flags.seen = Boolean(args.read);
      ack.read = row.flags.seen;
    }
    out.done.push(ack);
  }
  return out;
}

// ---------------------------------------------------------------------------
// Drafts, as fixture.rs keeps them: every write publishes `draft.changed`, a
// file that does not parse is under `skipped` and is refused with -32010.
// ---------------------------------------------------------------------------

function draftsOf(account: string): DraftListing {
  return (mock.drafts[account] ??= { account, drafts: [], skipped: [], collisions: [] });
}

function draftSelector(account: string, id: string): string {
  return `mp://${account}/drafts/${id}`;
}

function stemOf(path: string): string {
  return (path.split("/").pop() ?? path).replace(/\.md$/, "");
}

/** The `draft.invalid` payload of a file under `skipped`, by its stem. */
function invalidDraft(account: string, id: string): DraftInvalid | null {
  const skip = draftsOf(account).skipped.find((k) => stemOf(k.path) === id);
  return skip ? { account, id, path: skip.path, diagnostics: [{ line: null, message: skip.error }] } : null;
}

function draftInvalidError(method: string, invalid: DraftInvalid): GuiError {
  const why = invalid.diagnostics.map((d) => d.message).join("; ");
  return { kind: "protocol", message: `${method}: the daemon refused the call: ${why} (-32010)`, code: -32010 };
}

function draftChanged(account: string, d: DraftEntry): void {
  emitEnvelope("draft.changed", {
    account,
    id: d.id,
    path: d.path,
    to: d.to,
    subject: d.subject ?? "",
    status: d.status,
    valid: d.valid,
    ready: d.ready,
  });
}

/** Write a new draft at the top of the listing, as a rescan orders it (newest first). */
function writeDraft(
  account: string,
  fields: { id?: string; name?: string; to: string | null; cc: string | null; bcc?: string; subject: string; body?: string },
  source: DraftCreated["source"],
): DraftCreated {
  const id = fields.id ?? `fixture-draft-${mock.nextDraft++}`;
  const path = `/fixture/${account}/drafts/${fields.name ?? id}.md`;
  const entry: DraftEntry = {
    id,
    selector: draftSelector(account, id),
    path,
    status: "draft",
    to: fields.to || null,
    cc: fields.cc || null,
    subject: fields.subject,
    date: "2026-09-30T12:00:00",
    valid: true,
    ready: Boolean(fields.to) && fields.subject !== "",
  };
  draftsOf(account).drafts.unshift(entry);
  mock.draftExtra[`${account}/${id}`] = { bcc: fields.bcc ?? "", body: fields.body ?? "" };
  draftChanged(account, entry);
  return { account, id, selector: entry.selector, path, source };
}

function findDraft(method: string, account: string, id: string): DraftEntry {
  const d = draftsOf(account).drafts.find((x) => x.id === id);
  if (!d) throw refusedRow(method, `no draft matches ${draftSelector(account, id)}`);
  return d;
}

function replySubject(subject: string): string {
  return /^re:/i.test(subject) ? subject : `Re: ${subject}`;
}

function fwdSubject(subject: string): string {
  return subject.toLowerCase().startsWith("fwd: ") ? subject : `Fwd: ${subject}`;
}

function draftFromRow(cmd: string, account: string, args: Record<string, unknown>): DraftCreated {
  knownAccount(cmd, account);
  const method = cmd === "draft_reply" ? "draft.reply" : "draft.forward";
  const hit = findRow(account, Number(args.row_id));
  if (!hit) throw refusedRow(method, `${account} holds no message with row id ${String(args.row_id)}`);
  const row = hit[1];
  const headers = args.headers as { to: string; cc: string; bcc: string; subject: string } | null | undefined;
  const reply = cmd === "draft_reply";
  const derived = reply
    ? { to: row.from ?? "", cc: args.all ? (row.to ?? "") : "", bcc: "", subject: replySubject(row.subject ?? "") }
    : { to: "", cc: "", bcc: "", subject: fwdSubject(row.subject ?? "") };
  const f = headers ?? derived;
  return writeDraft(account, { ...f, body: `> ${row.body ?? ""}` }, { id: row.message_id, selector: row.selector });
}

function draftPreview(account: string, id: string): DraftPreview {
  const invalid = invalidDraft(account, id);
  if (invalid) throw draftInvalidError("draft.preview", invalid);
  const d = findDraft("draft.preview", account, id);
  const extra = mock.draftExtra[`${account}/${id}`];
  const body = extra?.body ?? fixtures.draftBodies[id] ?? "";
  const warnings = d.subject ? [] : ["the subject is empty"];
  return {
    account,
    id,
    selector: d.selector,
    path: d.path,
    from: "Me <me@example.com>",
    to: d.to,
    cc: d.cc,
    bcc: extra?.bcc || null,
    subject: d.subject ?? "",
    body,
    body_truncated: false,
    status: d.status,
    valid: d.valid,
    error: d.to ? null : "no recipient: to, cc and bcc are all empty",
    warnings,
    font_family: "Aptos",
    font_size: "11pt",
    signature: null,
  };
}

/** `draft_approve` or `draft_demote` over `ids`, in order, each failing alone. */
function setDraftStatus(cmd: string, account: string, ids: string[]): unknown {
  knownAccount(cmd, account);
  const method = cmd === "draft_approve" ? "draft.approve" : "draft.demote";
  const status = cmd === "draft_approve" ? "approved" : "draft";
  const out: { done: unknown[]; failed: unknown[] } = { done: [], failed: [] };
  for (const id of ids) {
    const invalid = invalidDraft(account, id);
    if (invalid) {
      out.failed.push({ id, error: draftInvalidError(method, invalid), invalid });
      continue;
    }
    const d = draftsOf(account).drafts.find((x) => x.id === id);
    if (!d) {
      out.failed.push({ id, error: refusedRow(method, `no draft matches ${draftSelector(account, id)}`) });
      continue;
    }
    d.status = status;
    draftChanged(account, d);
    out.done.push({ account, id, status, path: d.path });
  }
  return out;
}

async function answer(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  const account = String(args.account ?? "");
  switch (cmd) {
    case "subscribe_events": {
      mock.channel = args.on_event as Channel<GuiEvent>;
      const status = mock.connection;
      queueMicrotask(() => {
        emit({ type: "connection", status });
        if (status.state === "connected") {
          emit({ type: "rebootstrapped", cause: "subscribed", bootstrap: clone(mock.bootstrap) });
        }
      });
      return undefined;
    }
    case "connection_status":
      return mock.connection;
    case "retry_connect":
    case "restart_daemon":
      return undefined;
    case "bootstrap":
      return clone(mock.bootstrap);
    case "version_info":
      return {
        app_version: "0.1.0",
        protocol_min: 1,
        protocol_max: 1,
        fixture: true,
        daemon: mock.connection.state === "connected" ? { daemon_version: "0.0.0-fixture", protocol: 1, instance_id: "fixture-instance-1" } : null,
      } satisfies VersionInfo;
    case "list_accounts":
      return mock.accounts.map((a): AccountInfo => {
        const snap = mock.bootstrap.snapshot.accounts.find((x) => x.name === a.name);
        return {
          name: a.name,
          default: a.default,
          backend: a.backend,
          store_state: a.state,
          runtime_state: snap?.state ?? "opening",
          sync_health: snap?.sync_health.state ?? "unknown",
          outbox: mock.bootstrap.snapshot.outbox[a.name] ?? { queued: 0, failed: 0 },
        };
      });
    case "list_mailboxes":
      return mailboxListing(account);
    case "list_messages": {
      const mailbox = String(args.mailbox);
      if (mailbox === "drafts") {
        return { kind: "drafts", account, listing: clone(mock.drafts[account]) } satisfies MessageList;
      }
      const rows = (mock.rows[account]?.[mailbox] ?? []).map(strip).map((r) => ({ ...r, id: r.id + mock.rowShift }));
      return { kind: "messages", account, mailbox, total: rows.length, rows } satisfies MessageList;
    }
    case "message_text": {
      const rowId = Number(args.row_id);
      const hit = findRow(account, rowId);
      if (!hit) throw { kind: "not_found", message: `no message has row_id ${rowId}`, code: -32602 };
      return { account, row_id: rowId, body: hit[1].body ?? null } satisfies MessageText;
    }
    case "message_html_meta": {
      const rowId = Number(args.row_id);
      const hit = findRow(account, rowId);
      if (!hit) throw { kind: "not_found", message: `no message has row_id ${rowId}`, code: -32602 };
      const [mailbox, r] = hit;
      const present = (v: string | null) => (v ? v : null);
      return {
        row_id: rowId,
        html_url: `mpmsg://localhost/${account}/${rowId}`,
        selector: r.selector,
        account,
        mailbox,
        message_id: r.message_id,
        from: present(r.from),
        to: present(r.to),
        cc: present(r.cc),
        subject: present(r.subject),
        date: present(r.date_display),
        flags: [r.flags.seen ? "read" : null, r.flags.answered ? "answered" : null, r.flags.flagged ? "flagged" : null].filter(
          (f): f is string => f !== null,
        ),
        invite: r.is_invite,
        attachments: r.attachments ?? [],
      } satisfies MessageMeta;
    }
    case "intercepted_urls": {
      const drained = mock.interceptLog;
      mock.interceptLog = [];
      return drained;
    }
    case "open_external":
      return undefined;
    case "search_local": {
      // fixture.rs's `matches`: subject, sender or body, case-insensitive.
      const params = args.params as { account: string; query: string; mailbox?: string };
      const q = params.query.trim().toLowerCase();
      const hits: LocalSearchHit[] = [];
      for (const [mailbox, rows] of Object.entries(mock.rows[params.account] ?? {})) {
        if (params.mailbox && params.mailbox !== mailbox) continue;
        for (const r of rows) {
          const text = `${r.subject ?? ""}\n${r.from ?? ""}\n${r.body ?? ""}`.toLowerCase();
          if (text.includes(q)) hits.push({ ...strip(r), id: r.id + mock.rowShift, mailbox });
        }
      }
      return hits;
    }
    case "search_server_start":
      return { operation_id: `op-${mock.nextOp++}` };
    case "search_server_cancel":
      return mock.cancelOutcome;
    case "message_archive":
    case "message_delete":
    case "message_move":
    case "message_set_flag":
    case "message_set_read":
      return mutate(cmd, account, args);
    case "draft_discard": {
      knownAccount(cmd, account);
      const out: { done: { account: string; id: string; selector: string; status: string }[]; failed: { id: string; error: unknown }[] } = { done: [], failed: [] };
      for (const id of args.ids as string[]) {
        const drafts = mock.drafts[account]?.drafts ?? [];
        const at = drafts.findIndex((d) => d.id === id);
        const selector = `mp://${account}/drafts/${id}`;
        if (at < 0 || drafts[at].status === "approved") {
          const why = at < 0 ? `no draft matches ${selector}` : `${selector} is approved, a queued send`;
          out.failed.push({ id, error: refusedRow("draft.discard", why) });
          continue;
        }
        const [gone] = drafts.splice(at, 1);
        emitEnvelope("state.remove", { resource: `draft:${account}/${id}` });
        out.done.push({ account, id, selector, status: gone.status });
      }
      return out;
    }
    case "draft_create": {
      knownAccount(cmd, account);
      const name = String(args.name).replace(/\.md$/, "");
      if (draftsOf(account).drafts.some((d) => stemOf(d.path) === name)) {
        throw refusedRow("draft.create", `A draft already exists at /fixture/${account}/drafts/${name}.md`);
      }
      const headers = (args.headers as { to: string; cc: string; bcc: string; subject: string } | null) ?? null;
      const sig = args.no_signature ? null : ((args.signature as string | null) ?? mock.signatures.defaults[account] ?? null);
      const body = sig ? `\n\n${mock.signatures.signatures[sig] ?? ""}\n` : "";
      return writeDraft(
        account,
        { name, to: headers?.to ?? null, cc: headers?.cc ?? null, bcc: headers?.bcc ?? "", subject: headers?.subject ?? "", body },
        null,
      );
    }
    case "draft_reply":
    case "draft_forward":
      return draftFromRow(cmd, account, args);
    case "draft_from_message": {
      knownAccount(cmd, account);
      const m = args.message as DraftMessage;
      const kind = String(args.kind);
      const fields =
        kind === "forward"
          ? { to: "", cc: "", subject: fwdSubject(m.subject) }
          : { to: m.reply_to ?? m.from, cc: kind === "reply_all" ? m.to : "", subject: replySubject(m.subject) };
      return writeDraft(account, { ...fields, body: `> ${m.body_text}` }, null);
    }
    case "draft_path": {
      knownAccount(cmd, account);
      const id = String(args.id);
      const invalid = invalidDraft(account, id);
      if (invalid) return { account, id, selector: draftSelector(account, id), path: invalid.path, status: "invalid" };
      const d = findDraft("draft.path", account, id);
      return { account, id, selector: d.selector, path: d.path, status: d.status };
    }
    case "draft_approve":
    case "draft_demote":
      return setDraftStatus(cmd, account, args.ids as string[]);
    case "draft_validate": {
      knownAccount(cmd, account);
      const id = String(args.id);
      const invalid = invalidDraft(account, id);
      if (invalid) {
        const error = invalid.diagnostics[0].message;
        return { account, reports: [{ id, selector: draftSelector(account, id), valid: false, error, warnings: [] }] };
      }
      const p = draftPreview(account, id);
      return { account, reports: [{ id, selector: p.selector, valid: p.error === null, error: p.error, warnings: p.warnings }] };
    }
    case "draft_preview":
      knownAccount(cmd, account);
      return draftPreview(account, String(args.id));
    case "draft_set_recipients": {
      knownAccount(cmd, account);
      const d = findDraft("draft.path", account, String(args.id));
      d.to = String(args.to) || null;
      d.cc = String(args.cc) || null;
      if (typeof args.subject === "string") d.subject = args.subject;
      d.ready = Boolean(d.to) && Boolean(d.subject);
      const key = `${account}/${d.id}`;
      mock.draftExtra[key] = { body: mock.draftExtra[key]?.body ?? fixtures.draftBodies[d.id] ?? "", bcc: String(args.bcc) };
      draftChanged(account, d);
      return { account, id: d.id, selector: d.selector, path: d.path, status: d.status };
    }
    case "attachment_open": {
      const name = partOf("message.materialise_attachment", account, Number(args.row_id), Number(args.part));
      const path = `/fixture/runtime/handles/h-${mock.nextHandle++}/${name}`;
      mock.opened.push(path);
      return { name, path };
    }
    case "attachment_save": {
      const typed = String(args.dest_dir).trim();
      const dir = expandHome(typed);
      if (!dir.startsWith("/")) throw { kind: "protocol", message: `\`${typed}\` is not an absolute path; start it with / or ~`, code: null };
      const out: { dir: string; saved: unknown[]; failed: unknown[] } = { dir, saved: [], failed: [] };
      for (const part of args.parts as number[]) {
        try {
          const name = partOf("message.materialise_attachment", account, Number(args.row_id), part);
          out.saved.push({ part, name, path: saveName(dir, name) });
        } catch (error) {
          out.failed.push({ part, error });
        }
      }
      return out;
    }
    case "html_open": {
      const rowId = Number(args.row_id);
      if (!findRow(account, rowId)) throw refusedRow("message.materialise_html", `no message has row_id ${rowId}`);
      if (!(String(rowId - mock.rowShift) in htmlFx)) return null;
      const path = `/fixture/runtime/handles/h-${mock.nextHandle++}/message.html`;
      mock.opened.push(path);
      return { name: "message.html", path };
    }
    case "hit_html_open": {
      const path = `/fixture/cache/renditions/hit-${mock.nextHandle++}/message.html`;
      mock.opened.push(path);
      return { name: "message.html", path };
    }
    case "draft_attachments":
      knownAccount(cmd, account);
      return draftAttachmentsOf(account, String(args.id));
    case "draft_attach": {
      knownAccount(cmd, account);
      const id = String(args.id);
      const typed = String(args.path).trim();
      const file = expandHome(typed);
      if (!file.startsWith("/")) throw { kind: "protocol", message: `\`${typed}\` is not an absolute path; start it with / or ~`, code: null };
      if (!mock.files.has(file)) throw { kind: "not_found", message: `No such file: ${typed}`, code: null };
      const entries = (mock.draftAttachments[`${account}/${id}`] ??= []);
      const dup = entries.find((e) => expandHome(e) === file);
      if (dup) throw { kind: "protocol", message: `${dup} is already attached`, code: null };
      entries.push(typed);
      draftChanged(account, findDraft("draft.path", account, id));
      return draftAttachmentsOf(account, id);
    }
    case "draft_attachment_remove": {
      knownAccount(cmd, account);
      const id = String(args.id);
      const entries = mock.draftAttachments[`${account}/${id}`] ?? [];
      const index = Number(args.index);
      if (index >= entries.length) throw { kind: "not_found", message: `the draft lists ${entries.length} attachments, no number ${index + 1}`, code: null };
      entries.splice(index, 1);
      draftChanged(account, findDraft("draft.path", account, id));
      return draftAttachmentsOf(account, id);
    }
    case "draft_attachment_open": {
      knownAccount(cmd, account);
      const entry = (mock.draftAttachments[`${account}/${String(args.id)}`] ?? [])[Number(args.index)];
      if (entry === undefined) throw { kind: "not_found", message: `no attachment ${Number(args.index) + 1}`, code: null };
      const path = expandHome(entry);
      if (!mock.files.has(path)) throw { kind: "not_found", message: `${entry} is missing: no file at ${path}`, code: null };
      mock.opened.push(path);
      return { name: path.slice(path.lastIndexOf("/") + 1), path };
    }
    case "message_fetch": {
      knownAccount(cmd, account);
      const messageId = String(args.message_id);
      const label = String(args.mailbox);
      const box = fixtures.bootstrap.snapshot.mailboxes[account]?.find(
        (m) => m.label.toLowerCase() === label.toLowerCase() || m.slug === label.toLowerCase(),
      );
      if (!box) throw { kind: "protocol", message: `message.fetch: the daemon refused the call: no mailbox \`${label}\` (-32602)`, code: -32602 };
      for (const [mailbox, rows] of Object.entries(mock.rows[account] ?? {})) {
        const r = rows.find((x) => x.message_id === messageId);
        if (r) return { account, mailbox, uid: r.uid, row_id: r.id + mock.rowShift, selector: r.selector, already_present: true };
      }
      if (messageId !== "<server-only@fixture.example>") {
        throw { kind: "protocol", message: `The fetch failed: no message ${messageId} in ${label} on the server`, code: null };
      }
      const id = Math.max(...Object.values(mock.rows).flatMap((b) => Object.values(b).flatMap((rs) => rs.map((r) => r.id)))) + 1;
      const selector = `mp://${account}/${box.slug}/server-only@fixture.example`;
      const row = {
        id,
        uid: id,
        message_id: messageId,
        from: "Old Friend <old@example.com>",
        to: "me@example.com",
        cc: null,
        reply_to: null,
        bcc: null,
        subject: "The old thread",
        date_sort: "2025-03-03T08:00:00",
        date_display: "Mon, 3 Mar 2025 09:00:00 +0100",
        flags: { seen: true, answered: false, forwarded: false, flagged: false },
        has_attachments: false,
        is_invite: false,
        selector,
        body: "The old thread.",
        attachments: [],
      } as unknown as FixtureRow;
      ((mock.rows[account] ??= {})[box.slug] ??= []).unshift(row);
      emitEnvelope("state.invalidate", { resource: `mailbox:${account}/${box.slug}`, scope: { query: "counts" } });
      return { account, mailbox: box.slug, uid: id, row_id: id + mock.rowShift, selector, already_present: false };
    }
    case "signature_list": {
      knownAccount(cmd, account);
      return signatureListing(account);
    }
    case "signature_read": {
      const name = signatureName(String(args.name));
      signatureKnown(name);
      return { name, path: signaturePath(name), content: mock.signatures.signatures[name] };
    }
    case "signature_create": {
      const name = signatureName(String(args.name));
      if (name in mock.signatures.signatures) throw signatureRefusal(`a signature named '${name}' already exists`);
      mock.signatures.signatures[name] = "";
      emitEnvelope("signature.changed", { name, path: signaturePath(name) });
      return { name, path: signaturePath(name), content: "" };
    }
    case "signature_rename": {
      knownAccount(cmd, account);
      const old = signatureName(String(args.old));
      const name = signatureName(String(args.new));
      if (old === name) return signatureListing(account);
      signatureKnown(old);
      if (name in mock.signatures.signatures) throw signatureRefusal(`a signature named '${name}' already exists`);
      mock.signatures.signatures[name] = mock.signatures.signatures[old];
      delete mock.signatures.signatures[old];
      retargetDefaults(old, name);
      emitEnvelope("signature.changed", { name, path: signaturePath(name) });
      return signatureListing(account);
    }
    case "signature_delete": {
      knownAccount(cmd, account);
      const name = signatureName(String(args.name));
      signatureKnown(name);
      delete mock.signatures.signatures[name];
      retargetDefaults(name, null);
      return signatureListing(account);
    }
    case "signature_set_default": {
      knownAccount(cmd, account);
      const raw = args.name as string | null | undefined;
      if (raw == null) {
        delete mock.signatures.defaults[account];
      } else {
        const name = signatureName(raw);
        signatureKnown(name);
        mock.signatures.defaults[account] = name;
      }
      return signatureListing(account);
    }
    case "editor_open": {
      const path = String(args.path);
      mock.editorOpens.push(path);
      const failure = mock.editorFailure;
      mock.editorFailure = null;
      if (failure) throw failure;
      return { editor: `code --wait '${path}'`, source: "probe", fixture: mock.editorFixture };
    }
    case "config_open":
    case "log_open": {
      if (cmd === "config_open" && mock.configState === "absent") throw { kind: "not_found", message: NO_CONFIG, code: null };
      if (cmd === "log_open" && !mock.logExists) throw { kind: "not_found", message: `No log file found at ${MOCK_LOG_PATH}`, code: null };
      const path = cmd === "config_open" ? MOCK_CONFIG_PATH : MOCK_LOG_PATH;
      mock.editorOpens.push(path);
      const failure = mock.editorFailure;
      mock.editorFailure = null;
      if (failure) throw failure;
      return { editor: `code --wait '${path}'`, source: "probe", fixture: mock.editorFixture };
    }
    case "config_get":
      return {
        revision: mock.configRevision,
        path: MOCK_CONFIG_PATH,
        state: mock.configState,
        config: clone(mock.config),
      } satisfies ConfigSnapshot;
    // fixture.rs's `config.add_account` and `config.init`, their refusals in the daemon's words.
    case "config_add_account": {
      const draft = args.account as AccountDraft;
      if (mock.configState === "absent") {
        throw { kind: "protocol", code: -32602, message: `there is no configuration at ${MOCK_CONFIG_PATH}; write one with config.init first` };
      }
      if (mock.config.accounts.some((a) => a.name === draft.name)) {
        throw { kind: "protocol", code: -32602, message: `an account named ${draft.name} is already configured` };
      }
      return { added: [configureAccount(draft)], updated: [], removed: [] };
    }
    case "config_init": {
      const draft = args.account as AccountDraft;
      if (mock.configState !== "absent") {
        throw { kind: "protocol", code: -32602, message: `a configuration already exists at ${MOCK_CONFIG_PATH}; edit it and call config.reload` };
      }
      return { path: MOCK_CONFIG_PATH, added: [configureAccount(draft)], updated: [], removed: [] };
    }
    // fixture.rs's `config.oauth2_login`: its refusals, then a sign-in a test reports and settles.
    case "config_oauth2_login": {
      const a = mock.config.accounts.find((x) => x.name === account);
      if (!a) throw { kind: "not_found", code: -32005, message: `Account '${account}' not found in config` };
      if (a.auth_method !== "oauth2" && a.auth_method !== "graph") {
        throw {
          kind: "protocol",
          code: -32602,
          message: `Account '${account}' uses auth_method = "password", not "oauth2" or "graph". Set auth_method = "oauth2" or "graph" in config.toml to use OAuth2.`,
        };
      }
      const operation_id = `fixture-oauth-${mock.nextSignIn++}`;
      mock.signIns.push({ operation_id, account, kind: a.auth_method });
      return { operation_id };
    }
    case "config_oauth2_cancel": {
      const id = String(args.operation_id);
      const at = mock.signIns.findIndex((r) => r.operation_id === id);
      if (at < 0) return "already_settled";
      mock.signIns.splice(at, 1);
      emitEnvelope("operation.finished", {
        operation_id: id,
        state: "cancelled",
        error: { code: -32008, message: "operation_cancelled", data: { operation_id: id } },
      });
      return "cancelled";
    }
    case "config_reload": {
      if (mock.configInvalid) {
        emitEnvelope("config.invalid", { path: MOCK_CONFIG_PATH, line: CONFIG_INVALID_AT, message: CONFIG_INVALID_MESSAGE });
        throw { kind: "protocol", message: CONFIG_INVALID_MESSAGE, code: -32007 };
      }
      mock.configRevision += 1;
      emitEnvelope("config.changed", { added: [], updated: [], removed: [], config_revision: mock.configRevision });
      return { added: [], updated: [], removed: [] };
    }
    case "config_set_password": {
      const kind = String(args.kind);
      if (!mock.config.accounts.some((a) => a.name === account)) {
        throw { kind: "not_found", message: `no account named ${account} is configured`, code: -32005 };
      }
      mock.passwords.push({ account, kind, value: "<redacted>" });
      return { stored: true, account, kind, key: `${kind}-password-${account}` };
    }
    case "editor_setting_get":
    case "editor_setting_set": {
      if (cmd === "editor_setting_set") storeSetting("editor", (args.editor as string | null) ?? null);
      const editor = mock.settings.get("editor") ?? null;
      return {
        editor,
        file: "/fixture/config/desktop.json",
        env_override: null,
        effective: editor ?? "code --wait {path}",
        effective_source: editor ? "setting" : "probe",
        // terminal.rs `route`: a terminal editor in the setting runs embedded; the probe found code.
        route: editor && isTerminalEditor(editor) ? "embedded" : "external",
      };
    }
    // settings.rs: the known keys only, `null` or a blank value removes one.
    case "setting_get":
      return mock.settings.get(settingKey(args.key)) ?? null;
    case "setting_set": {
      const key = settingKey(args.key);
      storeSetting(key, (args.value as string | null) ?? null);
      return mock.settings.get(key) ?? null;
    }
    case "send_hold_status": {
      const only = args.account as string | null;
      return { holds: mock.holds.filter((h) => only === null || only === undefined || h.account === only) };
    }
    case "send_cancel_hold": {
      const id = String(args.operation_id);
      const at = mock.holds.findIndex((h) => h.operation_id === id);
      if (at < 0) throw refusedRow("send.cancel_hold", `no hold is running for ${id}`);
      const [hold] = mock.holds.splice(at, 1);
      emitEnvelope("send.hold_cancelled", { ...hold, remaining_secs: 0 });
      return { cancelled: true, operation_id: id, revision: mock.revision };
    }
    case "sync_trigger":
      knownAccount(cmd, account);
      return { operation_id: `fixture-op-${mock.nextSync++}` };
    case "send_draft":
      return sendDraft(account, String(args.id));
    case "send_approved": {
      knownAccount(cmd, account);
      const operation_id = `fixture-send-${mock.nextSend++}`;
      const first = draftsOf(account).drafts.find((d) => d.status === "approved");
      const held = mock.sendHeld && first !== undefined;
      if (held) armSendHold(operation_id, account, first);
      return { operation_id, held, approved: false };
    }
    case "outbox_list":
      knownAccount(cmd, account);
      return outboxListing(account);
    case "calendar_events":
      knownAccount(cmd, account);
      return agendaOf(account);
    case "invite_source_open": {
      knownAccount(cmd, account);
      const rowId = Number(args.row_id);
      if (!findRow(account, rowId) && !(mock.calendar[account] ?? []).some((e) => e.row_id === rowId)) {
        throw refusedRow("message.ics", `no message has row_id ${rowId}`);
      }
      if (!(String(rowId) in mock.ics)) throw { kind: "not_found", message: "That event has no ics source in the store", code: null };
      const path = `/fixture/cache/renditions/invite-${account}-${rowId}.ics`;
      mock.editorOpens.push(path);
      const failure = mock.editorFailure;
      mock.editorFailure = null;
      if (failure) throw failure;
      return { editor: `code --wait '${path}'`, source: "probe", fixture: mock.editorFixture };
    }
    case "invite_get": {
      knownAccount(cmd, account);
      const rowId = Number(args.row_id);
      if (!findRow(account, rowId) && !(mock.calendar[account] ?? []).some((e) => e.row_id === rowId)) {
        throw refusedRow("message.invite", `no message has row_id ${rowId}`);
      }
      return inviteEvent(account, rowId);
    }
    case "invite_refusal": {
      const a = fixtures.accounts.find((x) => x.name === account);
      if (!a) throw { kind: "not_found", message: `account_unknown: ${account}`, code: null };
      return { account, refusal: a.backend === "graph" ? GRAPH_RSVP_REFUSAL : null };
    }
    case "calendar_rsvp": {
      knownAccount(cmd, account);
      const response = String(args.response);
      if (!["accept", "tentative", "decline"].includes(response)) {
        throw { kind: "protocol", code: null, message: `An RSVP is accept, tentative or decline, not "${response}"` };
      }
      if (fixtures.accounts.find((x) => x.name === account)?.backend === "graph") {
        throw { kind: "protocol", code: -32602, message: `calendar.rsvp: the daemon refused the call: ${GRAPH_RSVP_REFUSAL} (-32602)` };
      }
      const rowId = Number(args.row_id);
      if (!inviteEvent(account, rowId)) {
        throw { kind: "protocol", code: -32602, message: `calendar.rsvp: the daemon refused the call: row ${rowId} carries no invitation to reply to (-32602)` };
      }
      const operation_id = `fixture-rsvp-${mock.nextRsvp++}`;
      mock.rsvps.push({ operation_id, account, row_id: rowId, response });
      return { operation_id };
    }
    case "send_invite": {
      knownAccount(cmd, account);
      const text = (k: string) => String(args[k] ?? "").trim();
      const refuse = (message: string) => ({ kind: "protocol", code: -32602, message });
      // The Rust layer's own checks, then the daemon's `plan_invite` order.
      if (!text("subject")) throw { kind: "protocol", code: null, message: "An invitation needs a subject" };
      if (!text("start")) throw { kind: "protocol", code: null, message: "An invitation needs a start" };
      const to = [...text("to").split(/[,;]/), ...text("cc").split(/[,;]/)].map((a) => a.trim()).filter(Boolean);
      if (to.length === 0) throw { kind: "protocol", code: null, message: "An invitation needs at least one recipient in To or Cc" };
      if (fixtures.accounts.find((x) => x.name === account)?.backend === "graph") throw refuse(GRAPH_INVITE_REFUSAL);
      if (text("end") && text("duration")) throw refuse("Provide exactly one of --end or --duration, not both");
      if (!text("end") && !text("duration")) throw refuse("An invite needs --end or --duration");
      const operation_id = `fixture-invite-${mock.nextInvite++}`;
      mock.invitesSent.push({ operation_id, account, subject: text("subject"), to: to.map((a) => a.replace(/^.*<([^>]+)>$/, "$1")) });
      return { operation_id };
    }
    case "contact_search": {
      knownAccount(cmd, account);
      const query = String(args.query ?? "");
      const needle = query.trim().toLowerCase();
      const rows = (mock.contacts[account] ?? [])
        .filter((c) => !needle || c.address.toLowerCase().includes(needle) || c.display_name.toLowerCase().includes(needle))
        .sort((a, b) => b.score - a.score)
        .slice(0, Number(args.limit ?? 20))
        .map((c) => ({ ...clone(c), recipient: formatRecipient(c.display_name, c.address) }));
      return { account, query, contacts: rows };
    }
    case "contact_rebuild": {
      knownAccount(cmd, account);
      const operation_id = `fixture-rebuild-${mock.nextRebuild++}`;
      mock.rebuilds.push({ operation_id, account });
      emitEnvelope("operation.progress", { operation_id, phase: "contacts", done: 0, total: null, message: account });
      return { operation_id };
    }
    case "contact_vcard_draft": {
      knownAccount(cmd, account);
      const address = String(args.address).trim();
      const display = String(args.display_name).trim();
      const label = display || address.split("@")[0];
      const stem = (display || address.split("@")[0]).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "contact";
      const dir = `/fixture/${account}/drafts/_vcards`;
      const taken = new Set(mock.vcards.map((v) => v.vcf));
      let vcf = `${dir}/${stem}.vcf`;
      for (let n = 1; taken.has(vcf); n++) vcf = `${dir}/${stem}-${n}.vcf`;
      const draft = writeDraft(
        account,
        { name: String(args.name), to: formatRecipient(display, address), cc: null, subject: `Contact: ${label}`, body: "" },
        null,
      );
      mock.draftAttachments[`${account}/${draft.id}`] = [vcf];
      mock.vcards.push({ account, id: draft.id, vcf });
      return { draft, vcf };
    }
    case "outbox_retry": {
      knownAccount(cmd, account);
      const rowId = Number(args.row_id);
      const row = mock.outbox[account]?.rows.find((r) => r.id === rowId);
      if (!row) throw { kind: "protocol", code: -32602, message: `send.outbox_retry: no outbox row ${rowId} (-32602)` };
      if (row.state !== "failed" && row.state !== "sent_pending_append") {
        throw { kind: "protocol", code: -32602, message: `outbox row ${rowId} is ${row.state}, and only a failed row can be retried` };
      }
      return { operation_id: `fixture-op-retry-${mock.nextRetry++}` };
    }
    case "outbox_discard": {
      knownAccount(cmd, account);
      const rowId = Number(args.row_id);
      const rows = mock.outbox[account]?.rows ?? [];
      const at = rows.findIndex((r) => r.id === rowId);
      if (at < 0) throw refusedRow("send.outbox_discard", `no outbox row ${rowId}`);
      const [row] = rows.splice(at, 1);
      emitEnvelope("state.invalidate", { resource: `outbox:${account}`, scope: { query: "counts" } });
      return { discarded: true, row_id: rowId, message_id: row.message_id, revision: mock.revision };
    }
    default:
      throw { kind: "internal", message: `the mock does not answer ${cmd}` };
  }
}

const SETTING_KEYS = ["editor", "theme", "reader_mode"];

function settingKey(key: unknown): string {
  const k = String(key);
  if (!SETTING_KEYS.includes(k)) {
    throw { kind: "not_found", message: `no desktop setting \`${k}\`; the settings are ${SETTING_KEYS.join(", ")}`, code: null };
  }
  return k;
}

function storeSetting(key: string, value: string | null): void {
  const v = value?.trim() ?? "";
  if (key === "theme" && v !== "" && !["dark", "light", "system"].includes(v)) {
    throw { kind: "setup", message: `the theme \`${v}\` is none of dark, light or system` };
  }
  if (key === "reader_mode" && v !== "" && !["html", "text"].includes(v)) {
    throw { kind: "setup", message: `the reader mode \`${v}\` is neither html nor text` };
  }
  if (v === "") mock.settings.delete(key);
  else mock.settings.set(key, v);
}

/** Whether a template's program is one of editor.rs's `TERMINAL_EDITORS`. */
function isTerminalEditor(template: string): boolean {
  const program = template.trim().split(/\s+/)[0] ?? "";
  return ["vi", "vim", "nvim", "hx", "helix", "nano", "pico", "micro", "kak", "joe", "ne", "mg", "ed"].includes(program.split("/").pop() ?? "");
}

export const invoke = vi.fn(async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
  mock.calls.push({ cmd, args });
  const failure = mock.failing.get(cmd);
  if (failure !== undefined) throw failure;
  const result = await answer(cmd, args);
  const gate = mock.gates.get(cmd);
  if (gate) {
    mock.gates.delete(cmd);
    await gate;
  }
  return result as T;
});

export const listen = vi.fn(async <T,>(event: string, handler: (e: { payload: T }) => void) => {
  if (event === "menu") mock.menu = handler as unknown as (e: { payload: string }) => void;
  return () => {
    if (event === "menu") mock.menu = null;
  };
});

export type UnlistenFn = () => void;

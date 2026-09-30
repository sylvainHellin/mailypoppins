// The fixture-backed stand-in for src/lib/tauri.ts. It answers every command
// from clients/desktop/fixtures/*.json the way src-tauri/src/fixture.rs does,
// holds the one event Channel, and lets a test push GuiEvents and menu items.

import { vi } from "vitest";
import accountsFx from "../../fixtures/accounts.json";
import bootstrapFx from "../../fixtures/bootstrap.json";
import draftBodiesFx from "../../fixtures/draft-bodies.json";
import draftsFx from "../../fixtures/drafts.json";
import messagesFx from "../../fixtures/messages.json";
import signaturesFx from "../../fixtures/signatures.json";
import type {
  Bootstrap,
  DraftCreated,
  DraftEntry,
  DraftInvalid,
  DraftListing,
  DraftMessage,
  DraftPreview,
  HoldStatus,
  MessageListRow,
} from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
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
};

export class Channel<T> {
  onmessage: (message: T) => void = () => {};
}

const clone = <T,>(v: T): T => structuredClone(v);

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
  /** A draft's fields the listing does not carry, by `<account>/<id>`. */
  draftExtra: {} as Record<string, { bcc: string; body: string }>,
  /** The next minted draft id's counter. */
  nextDraft: 1,
  /** The `editor` key of the settings file. */
  editorSetting: null as string | null,
};

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
  mock.draftExtra = {};
  mock.nextDraft = 1;
  mock.editorSetting = null;
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
  const snap = fixtures.bootstrap.snapshot;
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
  if (!fixtures.bootstrap.snapshot.accounts.some((a) => a.name === account)) {
    throw { kind: "not_found", message: `${cmd}: the daemon refused the call: account_unknown: ${account} (-32005)`, code: -32005 };
  }
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
          emit({ type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap });
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
      return fixtures.bootstrap;
    case "version_info":
      return {
        app_version: "0.1.0",
        protocol_min: 1,
        protocol_max: 1,
        fixture: true,
        daemon: mock.connection.state === "connected" ? { daemon_version: "0.0.0-fixture", protocol: 1, instance_id: "fixture-instance-1" } : null,
      } satisfies VersionInfo;
    case "list_accounts":
      return fixtures.accounts.map((a): AccountInfo => {
        const snap = fixtures.bootstrap.snapshot.accounts.find((x) => x.name === a.name);
        return {
          name: a.name,
          default: a.default,
          backend: a.backend,
          store_state: a.state,
          runtime_state: snap?.state ?? "opening",
          sync_health: snap?.sync_health.state ?? "unknown",
          outbox: fixtures.bootstrap.snapshot.outbox[a.name] ?? { queued: 0, failed: 0 },
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
      const sig = args.no_signature ? null : ((args.signature as string | null) ?? fixtures.signatures.defaults[account] ?? null);
      const body = sig ? `\n\n${fixtures.signatures.signatures[sig] ?? ""}\n` : "";
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
    case "signature_list": {
      knownAccount(cmd, account);
      const names = Object.keys(fixtures.signatures.signatures).sort();
      return { account, names, default: fixtures.signatures.defaults[account] ?? null };
    }
    case "editor_open": {
      const path = String(args.path);
      mock.editorOpens.push(path);
      const failure = mock.editorFailure;
      mock.editorFailure = null;
      if (failure) throw failure;
      return { editor: `code --wait '${path}'`, source: "probe" };
    }
    case "editor_setting_get":
    case "editor_setting_set": {
      if (cmd === "editor_setting_set") mock.editorSetting = (args.editor as string | null) ?? null;
      return {
        editor: mock.editorSetting,
        file: "/fixture/config/desktop.json",
        env_override: null,
        effective: mock.editorSetting ?? "code --wait {path}",
        effective_source: mock.editorSetting ? "setting" : "probe",
      };
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
    default:
      throw { kind: "internal", message: `the mock does not answer ${cmd}` };
  }
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

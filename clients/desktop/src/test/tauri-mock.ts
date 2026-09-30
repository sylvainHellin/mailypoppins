// The fixture-backed stand-in for src/lib/tauri.ts. It answers every command
// from clients/desktop/fixtures/*.json the way src-tauri/src/fixture.rs does,
// holds the one event Channel, and lets a test push GuiEvents and menu items.

import { vi } from "vitest";
import accountsFx from "../../fixtures/accounts.json";
import bootstrapFx from "../../fixtures/bootstrap.json";
import draftsFx from "../../fixtures/drafts.json";
import messagesFx from "../../fixtures/messages.json";
import type { Bootstrap, DraftListing, HoldStatus, MessageListRow } from "@/protocol/types";
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

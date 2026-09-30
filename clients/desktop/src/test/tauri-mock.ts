// The fixture-backed stand-in for src/lib/tauri.ts. It answers every command
// from clients/desktop/fixtures/*.json the way src-tauri/src/fixture.rs does,
// holds the one event Channel, and lets a test push GuiEvents and menu items.

import { vi } from "vitest";
import accountsFx from "../../fixtures/accounts.json";
import bootstrapFx from "../../fixtures/bootstrap.json";
import draftsFx from "../../fixtures/drafts.json";
import messagesFx from "../../fixtures/messages.json";
import type { Bootstrap, DraftListing, MessageListRow } from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
  GuiEvent,
  MailboxListing,
  MessageList,
  MessageMeta,
  MessageText,
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

export const mock = {
  connection: { state: "connected", instance_id: "fixture-instance-1", daemon_version: "0.0.0-fixture", protocol: 1, fixture: true } as ConnectionStatus,
  channel: null as Channel<GuiEvent> | null,
  menu: null as ((e: { payload: string }) => void) | null,
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  /** Fail a command with a GuiError-shaped rejection. */
  failing: new Map<string, unknown>(),
  /** Added to every row_id, as a daemon restart that rebuilt the store would. */
  rowShift: 0,
};

export function resetMock(): void {
  mock.connection = { state: "connected", instance_id: "fixture-instance-1", daemon_version: "0.0.0-fixture", protocol: 1, fixture: true };
  mock.channel = null;
  mock.menu = null;
  mock.calls = [];
  mock.failing.clear();
  mock.rowShift = 0;
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
  for (const [mailbox, rows] of Object.entries(fixtures.messages[account] ?? {})) {
    const row = rows.find((r) => r.id + mock.rowShift === rowId);
    if (row) return [mailbox, row];
  }
  return null;
}

export function mailboxListing(account: string): MailboxListing {
  const snap = fixtures.bootstrap.snapshot;
  const rows = snap.mailboxes[account] ?? [];
  const mailboxes = rows.map((m) => {
    const msgs = fixtures.messages[account]?.[m.slug] ?? [];
    const total = m.role === "drafts" ? (fixtures.drafts[account]?.drafts.length ?? 0) : msgs.length;
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
        return { kind: "drafts", account, listing: fixtures.drafts[account] } satisfies MessageList;
      }
      const rows = (fixtures.messages[account]?.[mailbox] ?? []).map(strip).map((r) => ({ ...r, id: r.id + mock.rowShift }));
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
    case "intercepted_urls":
      return [];
    default:
      throw { kind: "internal", message: `the mock does not answer ${cmd}` };
  }
}

export const invoke = vi.fn(async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
  mock.calls.push({ cmd, args });
  const failure = mock.failing.get(cmd);
  if (failure !== undefined) throw failure;
  return (await answer(cmd, args)) as T;
});

export const listen = vi.fn(async <T,>(event: string, handler: (e: { payload: T }) => void) => {
  if (event === "menu") mock.menu = handler as unknown as (e: { payload: string }) => void;
  return () => {
    if (event === "menu") mock.menu = null;
  };
});

export type UnlistenFn = () => void;

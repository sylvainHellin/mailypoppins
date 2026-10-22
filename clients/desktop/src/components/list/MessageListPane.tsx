import { useCallback, useEffect, useRef } from "react";
import { Server, Search, SlidersHorizontal, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { ConversationMemberRow } from "@/components/list/ConversationMemberRow";
import { ConversationRow } from "@/components/list/ConversationRow";
import { DraftRow } from "@/components/list/DraftRow";
import { MessageRow } from "@/components/list/MessageRow";
import { revealRow, useWindow, WINDOW_FROM } from "@/components/list/useWindow";
import { SearchResults } from "@/components/search/SearchResults";
import { SearchStatusBar } from "@/components/search/SearchStatusBar";
import { FILTER_INPUT_ID, markOpenRead, runAction, runMutation } from "@/app/actions";
import { useAppState, useDispatch } from "@/app/store";
import { usePaneFocused } from "@/hooks/use-pane-focused";
import {
  conversationEntries,
  entryAtCursor,
  expandable,
  filteredDrafts,
  filteredRows,
  memberKey,
  sendingKeys,
  targetKey,
  threadKey,
} from "@/app/state";
import type { ConversationRow as Conversation, MessageListRow, ThreadMessage } from "@/protocol/types";

export function MessageListPane() {
  const s = useAppState();
  const dispatch = useDispatch();
  const focused = usePaneFocused("list");
  const scrollRef = useRef<HTMLDivElement>(null);
  // The row toggles read the flags as they are at the click.
  const stateRef = useRef(s);
  stateRef.current = s;

  const list = s.messages.data;
  const f = s.filter.trim();
  const rows = filteredRows(list, s.filter);
  const drafts = filteredDrafts(list, s.filter);
  // A conversation list (ticket 0142) shows its headers and, under each expanded
  // one, its members: one sequence, every line of the same fixed height.
  const entries = list?.kind === "conversations" ? conversationEntries(list, s.expanded, s.filter) : [];
  const count = list?.kind === "drafts" ? drafts.length : list?.kind === "conversations" ? entries.length : rows.length;
  const win = useWindow(scrollRef, count);

  const account = s.selection.account;
  const label =
    (account && s.mailboxes[account]?.data?.mailboxes.find((m) => m.slug === s.selection.mailbox)?.label) ??
    s.selection.mailbox ??
    "Mailbox";

  const sel = s.selection.message;
  const selectedIndex =
    list?.kind === "drafts"
      ? drafts.findIndex((d) => d.id === s.selection.draft)
      : list?.kind === "conversations"
        ? entries.findIndex((e) => entryAtCursor(e, s.selection.conversation))
        : sel
        ? rows.findIndex((r) => r.message_id === sel.message_id && r.selector === sel.selector)
        : -1;
  const tabIndexRow = selectedIndex >= 0 ? selectedIndex : 0;

  // Keep the selected row mounted and in view when the keyboard moves it.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || selectedIndex < 0) return;
    if (count >= WINDOW_FROM) revealRow(el, selectedIndex);
    else el.querySelector<HTMLElement>('[data-cursor="true"]')?.scrollIntoView?.({ block: "nearest" });
  }, [selectedIndex, count, s.focusSeq]);

  const onSelect = useCallback(
    (row: MessageListRow) =>
      dispatch({
        type: "select_message",
        message: { row_id: row.id, message_id: row.message_id, selector: row.selector },
      }),
    [dispatch],
  );
  const onOpen = useCallback(
    (row: MessageListRow) => {
      dispatch({
        type: "select_message",
        message: { row_id: row.id, message_id: row.message_id, selector: row.selector },
        focus: "reader",
      });
      markOpenRead(account ? { account, row_id: row.id } : null, row.flags.seen, dispatch);
    },
    [account, dispatch],
  );
  const onSelectDraft = useCallback((id: string) => dispatch({ type: "select_draft", id }), [dispatch]);
  const mark = useCallback(
    (key: string, range: boolean) => dispatch(range ? { type: "mark_range", key } : { type: "mark_toggle", key }),
    [dispatch],
  );
  const onMark = useCallback(
    (row: MessageListRow, range: boolean) => account && mark(targetKey({ account, row_id: row.id }), range),
    [account, mark],
  );
  const onMarkDraft = useCallback(
    (id: string, range: boolean) => account && mark(targetKey({ account, draft: id }), range),
    [account, mark],
  );
  const onToggleFlag = useCallback(
    (row: MessageListRow) =>
      account && runMutation("toggle_flag", [{ account, row_id: row.id }], stateRef.current, dispatch, false),
    [account, dispatch],
  );
  const onToggleRead = useCallback(
    (row: MessageListRow) =>
      account && runMutation("toggle_read", [{ account, row_id: row.id }], stateRef.current, dispatch, false),
    [account, dispatch],
  );
  const onSelectConversation = useCallback(
    (thread_id: string) => dispatch({ type: "select_conversation", thread_id, member: null }),
    [dispatch],
  );
  const onOpenConversation = useCallback(
    (thread_id: string) => {
      const cur = stateRef.current;
      const row = cur.messages.data?.kind === "conversations" ? cur.messages.data.rows.find((r) => r.thread_id === thread_id) : undefined;
      if (!row) return;
      // A double-click is Enter: a collapsed conversation of several opens, any other reads.
      if (expandable(row) && !cur.expanded[thread_id]) {
        dispatch({ type: "select_conversation", thread_id, member: null });
        return dispatch({ type: "conversation_expand", thread_id });
      }
      dispatch({ type: "select_conversation", thread_id, member: null, focus: "reader" });
      markOpenRead(account ? { account, row_id: row.latest.id } : null, row.latest.flags.seen, dispatch);
    },
    [account, dispatch],
  );
  const onToggleExpand = useCallback(
    (thread_id: string) =>
      dispatch({ type: stateRef.current.expanded[thread_id] ? "conversation_collapse" : "conversation_expand", thread_id }),
    [dispatch],
  );
  const onMarkConversation = useCallback(
    (thread_id: string, range: boolean) => account && mark(threadKey(account, thread_id), range),
    [account, mark],
  );
  // The toggles on a header act on its members here, as the keys do.
  const onToggleFlagConversation = useCallback(
    (row: Conversation) =>
      account && runMutation("toggle_flag", row.member_ids.map((row_id) => ({ account, row_id })), stateRef.current, dispatch, false),
    [account, dispatch],
  );
  const onToggleReadConversation = useCallback(
    (row: Conversation) =>
      account && runMutation("toggle_read", row.member_ids.map((row_id) => ({ account, row_id })), stateRef.current, dispatch, false),
    [account, dispatch],
  );
  const onSelectMember = useCallback(
    (thread_id: string, member: string) => dispatch({ type: "select_conversation", thread_id, member }),
    [dispatch],
  );
  const onOpenMember = useCallback(
    (thread_id: string, member: string) => {
      dispatch({ type: "select_conversation", thread_id, member, focus: "reader" });
      const m = Object.values(stateRef.current.expanded)
        .flatMap((e) => e.data?.messages ?? [])
        .find((x) => memberKey(x) === member);
      if (m && !m.draft_id) markOpenRead(account ? { account, row_id: m.id } : null, m.flags.seen, dispatch);
    },
    [account, dispatch],
  );
  const onMarkMember = useCallback(
    (m: ThreadMessage, range: boolean) =>
      account && mark(m.draft_id ? targetKey({ account, draft: m.draft_id }) : targetKey({ account, row_id: m.id }), range),
    [account, mark],
  );
  const mailboxLabel = (slug: string) =>
    slug === "drafts"
      ? "Drafts"
      : ((account && s.mailboxes[account]?.data?.mailboxes.find((m) => m.slug === slug)?.label) ?? slug);
  const markedCount = s.marked.keys.size;
  const anyMarked = markedCount > 0;
  const rowKey = (id: number) => targetKey({ account: account ?? "", row_id: id });
  const draftKey = (id: string) => targetKey({ account: account ?? "", draft: id });
  const sending = sendingKeys(s);

  const total =
    list?.kind === "messages" || list?.kind === "conversations" ? list.total : list?.kind === "drafts" ? list.listing.drafts.length : null;
  const noun = list?.kind === "drafts" ? "drafts" : list?.kind === "conversations" ? "conversations" : "messages";
  const search = s.search;
  const query = s.filter.trim();

  return (
    <section
      aria-label="Message list"
      className="flex h-full min-h-0 min-w-0 flex-col"
      data-pane="list"
      data-focused={focused}
      onFocus={() => dispatch({ type: "pane_focused", pane: "list" })}
    >
      <header className="flex shrink-0 flex-col gap-2 border-b border-border px-3 py-2">
        <div className="flex items-baseline justify-between gap-2">
          <h2 className="truncate text-sm font-semibold">{search ? `Search: ${search.query}` : label}</h2>
          {search ? (
            <span className="text-xs text-muted-foreground tabular-nums">{`${search.hits.length} results`}</span>
          ) : total !== null ? (
            <span className="text-xs text-muted-foreground tabular-nums">
              {f ? `${count} of ${total}` : `${total} ${noun}`}
            </span>
          ) : null}
        </div>
        <div className="flex items-center gap-1.5">
          <div className="relative min-w-0 flex-1">
            <Search aria-hidden="true" className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              id={FILTER_INPUT_ID}
              type="search"
              value={s.filter}
              onChange={(e) => dispatch({ type: "filter", text: e.currentTarget.value })}
              onKeyDown={(e) => {
                if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
                e.preventDefault();
                if (!query) return;
                dispatch({ type: e.shiftKey ? "search_server" : "search_local", query });
              }}
              placeholder="Filter, Enter to search  ( / )"
              aria-label="Filter this list, or press Enter to search the account, Shift+Enter to search the server"
              className="h-7 pl-7 text-xs"
            />
          </div>
          {/* With nothing typed and no search shown, the server button opens the form. */}
          <Button
            size="icon-sm"
            variant="outline"
            aria-label="Search server"
            title="Search server (Shift+Enter)"
            onClick={() => runAction("search_server", s, dispatch)}
          >
            <Server aria-hidden="true" />
          </Button>
          <Button
            size="icon-sm"
            variant="outline"
            aria-label="Server Search form"
            title="Server Search form: from, to, subject, dates (ff)"
            onClick={() => runAction("open_server_search", s, dispatch)}
          >
            <SlidersHorizontal aria-hidden="true" />
          </Button>
        </div>
        {/* Mounted with no marks, visually hidden and empty, so the first count is announced. */}
        <div data-slot="marked-bar" className={anyMarked ? "flex items-center gap-2 text-xs" : "sr-only"}>
          <span role="status" data-slot="marked-count" className="flex-1 text-muted-foreground tabular-nums">
            {anyMarked ? `${markedCount} marked` : null}
          </span>
          {anyMarked ? (
            <Button size="xs" variant="ghost" onClick={() => dispatch({ type: "mark_clear" })} title="Clear marks (Esc)">
              <X aria-hidden="true" />
              Clear marks
            </Button>
          ) : null}
        </div>
        {search ? (
          <SearchStatusBar
            search={search}
            onCancel={() => runAction("cancel_search", s, dispatch)}
            onServer={() => runAction("search_server", s, dispatch)}
            onExit={() => dispatch({ type: "exit_search" })}
          />
        ) : null}
      </header>

      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto">
        {search ? (
          <SearchResults search={search} />
        ) : s.messages.error && !list ? (
          <p role="alert" className="p-4 text-sm text-destructive">
            The list did not load: {s.messages.error.message}
          </p>
        ) : !list ? (
          <div aria-busy="true" aria-label="Loading messages" className="flex flex-col gap-2 p-3">
            {Array.from({ length: 8 }, (_, i) => (
              <Skeleton key={i} className="h-12 w-full" />
            ))}
          </div>
        ) : count === 0 ? (
          <p className="p-4 text-sm text-muted-foreground">{f ? "Nothing matches the filter." : "This mailbox is empty."}</p>
        ) : (
          <div
            role="listbox"
            aria-multiselectable="true"
            aria-label={`${label} messages`}
            style={{ paddingTop: win.padTop, paddingBottom: win.padBottom }}
          >
            {list.kind === "drafts"
              ? drafts.slice(win.start, win.end).map((d, i) => (
                  <DraftRow
                    key={d.id}
                    draft={d}
                    editing={draftKey(d.id) in s.compose}
                    cursor={d.id === s.selection.draft}
                    marked={s.marked.keys.has(draftKey(d.id))}
                    anyMarked={anyMarked}
                    pending={draftKey(d.id) in s.pending}
                    sending={sending.has(draftKey(d.id))}
                    tabStop={win.start + i === tabIndexRow}
                    position={win.start + i + 1}
                    setSize={count}
                    onSelect={onSelectDraft}
                    onMark={onMarkDraft}
                  />
                ))
              : list.kind === "conversations"
                ? entries.slice(win.start, win.end).map((e, i) => {
                    const at = win.start + i;
                    if (e.kind === "header") {
                      const r = e.row;
                      return (
                        <ConversationRow
                          key={`h:${r.thread_id}`}
                          row={r}
                          expanded={e.expanded}
                          expandable={expandable(r)}
                          cursor={at === selectedIndex}
                          marked={s.marked.keys.has(threadKey(account ?? "", r.thread_id))}
                          anyMarked={anyMarked}
                          pending={r.member_ids.some((id) => rowKey(id) in s.pending)}
                          tabStop={at === tabIndexRow}
                          position={at + 1}
                          setSize={count}
                          onSelect={onSelectConversation}
                          onOpen={onOpenConversation}
                          onMark={onMarkConversation}
                          onToggleExpand={onToggleExpand}
                          onToggleFlag={onToggleFlagConversation}
                          onToggleRead={onToggleReadConversation}
                        />
                      );
                    }
                    const m = e.member;
                    const key = m.draft_id ? draftKey(m.draft_id) : rowKey(m.id);
                    const next = entries[at + 1];
                    return (
                      <ConversationMemberRow
                        key={`m:${e.thread_id}:${memberKey(m)}`}
                        threadId={e.thread_id}
                        member={m}
                        memberKey={memberKey(m)}
                        mailboxLabel={mailboxLabel(m.mailbox)}
                        last={next?.kind !== "member" || next.thread_id !== e.thread_id}
                        cursor={at === selectedIndex}
                        marked={s.marked.keys.has(key)}
                        anyMarked={anyMarked}
                        pending={key in s.pending}
                        tabStop={at === tabIndexRow}
                        position={at + 1}
                        setSize={count}
                        onSelect={onSelectMember}
                        onOpen={onOpenMember}
                        onMark={onMarkMember}
                      />
                    );
                  })
                : rows.slice(win.start, win.end).map((r, i) => (
                  <MessageRow
                    key={r.id}
                    row={r}
                    cursor={win.start + i === selectedIndex}
                    marked={s.marked.keys.has(rowKey(r.id))}
                    anyMarked={anyMarked}
                    pending={rowKey(r.id) in s.pending}
                    tabStop={win.start + i === tabIndexRow}
                    position={win.start + i + 1}
                    setSize={count}
                    onSelect={onSelect}
                    onOpen={onOpen}
                    onMark={onMark}
                    onToggleFlag={onToggleFlag}
                    onToggleRead={onToggleRead}
                  />
                ))}
          </div>
        )}
      </div>
    </section>
  );
}

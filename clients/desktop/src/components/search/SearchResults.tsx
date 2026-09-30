import { useCallback, useEffect, useRef } from "react";
import { SearchHitRow } from "@/components/search/SearchHitRow";
import { markOpenRead } from "@/app/actions";
import { isOpenable } from "@/app/search";
import { useAppState, useDispatch } from "@/app/store";
import { targetKey, type SearchHit, type SearchState } from "@/app/state";

/** The search's hits in place of the mailbox list. */
export function SearchResults({ search }: { search: SearchState }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const ref = useRef<HTMLDivElement>(null);

  const listing = s.mailboxes[search.account]?.data;
  const labelOf = (mailbox: string) => listing?.mailboxes.find((m) => m.slug === mailbox)?.label ?? mailbox;

  const sel = s.selection.message;
  const selHit = s.selection.hit;
  const selectedIndex = selHit
    ? search.hits.findIndex((h) => h.key === selHit)
    : sel
      ? search.hits.findIndex((h) => h.message_id === sel.message_id && h.selector === sel.selector)
      : -1;
  const tabIndexRow = Math.max(0, selectedIndex);

  useEffect(() => {
    if (selectedIndex < 0) return;
    ref.current?.querySelector<HTMLElement>('[data-cursor="true"]')?.scrollIntoView?.({ block: "nearest" });
  }, [selectedIndex, s.focusSeq]);

  const pick = useCallback(
    (hit: SearchHit, focus?: "reader") => {
      // A server-only hit has no row: the cursor sits on it by its key.
      if (!isOpenable(hit)) return dispatch({ type: "select_hit", key: hit.key, ...(focus ? { focus } : {}) });
      if (hit.row_id === null || hit.selector === null || hit.message_id === null) return;
      dispatch({
        type: "select_message",
        message: { row_id: hit.row_id, message_id: hit.message_id, selector: hit.selector },
        ...(focus ? { focus } : {}),
      });
      // Opening a hit marks it read (MSG-08); selecting one does not.
      if (focus) markOpenRead({ account: hit.account, row_id: hit.row_id }, hit.flags.seen, dispatch);
    },
    [dispatch],
  );
  const onSelect = useCallback((hit: SearchHit) => pick(hit), [pick]);
  const onOpen = useCallback((hit: SearchHit) => pick(hit, "reader"), [pick]);
  const onMark = useCallback(
    (hit: SearchHit, range: boolean) => {
      if (hit.row_id === null) return;
      const key = targetKey({ account: hit.account, row_id: hit.row_id });
      dispatch(range ? { type: "mark_range", key } : { type: "mark_toggle", key });
    },
    [dispatch],
  );
  const anyMarked = s.marked.keys.size > 0;
  const keyOf = (h: SearchHit) => (h.row_id === null ? null : targetKey({ account: h.account, row_id: h.row_id }));

  if (search.hits.length === 0) {
    const busy = search.status === "searching" || search.status === "running";
    return (
      <p className="p-4 text-sm text-muted-foreground">
        {busy ? "No results yet." : search.status === "done" ? "Nothing matches." : "No results."}
      </p>
    );
  }
  return (
    <div ref={ref} role="listbox" aria-multiselectable="true" aria-label={`Search results for ${search.query}`}>
      {search.hits.map((h, i) => (
        <SearchHitRow
          key={h.key}
          hit={h}
          mailboxLabel={h.origin === "local" ? labelOf(h.mailbox) : h.mailbox}
          cursor={i === selectedIndex}
          marked={keyOf(h) !== null && s.marked.keys.has(keyOf(h)!)}
          anyMarked={anyMarked}
          pending={keyOf(h) !== null && keyOf(h)! in s.pending}
          tabStop={i === tabIndexRow}
          position={i + 1}
          setSize={search.hits.length}
          onSelect={onSelect}
          onOpen={onOpen}
          onMark={onMark}
        />
      ))}
    </div>
  );
}

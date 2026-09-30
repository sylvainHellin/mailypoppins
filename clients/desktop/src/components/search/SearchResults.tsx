import { useCallback, useEffect, useRef } from "react";
import { SearchHitRow } from "@/components/search/SearchHitRow";
import { useAppState, useDispatch } from "@/app/store";
import type { SearchHit, SearchState } from "@/app/state";

/** The search's hits in place of the mailbox list. */
export function SearchResults({ search }: { search: SearchState }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const ref = useRef<HTMLDivElement>(null);

  const listing = s.mailboxes[search.account]?.data;
  const labelOf = (mailbox: string) => listing?.mailboxes.find((m) => m.slug === mailbox)?.label ?? mailbox;

  const sel = s.selection.message;
  const selectedIndex = sel
    ? search.hits.findIndex((h) => h.message_id === sel.message_id && h.selector === sel.selector)
    : -1;
  const firstOpenable = search.hits.findIndex((h) => h.row_id !== null);
  const tabIndexRow = selectedIndex >= 0 ? selectedIndex : Math.max(0, firstOpenable);

  useEffect(() => {
    if (selectedIndex < 0) return;
    ref.current?.querySelector<HTMLElement>('[aria-selected="true"]')?.scrollIntoView?.({ block: "nearest" });
  }, [selectedIndex, s.focusSeq]);

  const pick = useCallback(
    (hit: SearchHit, focus?: "reader") => {
      if (hit.row_id === null || hit.selector === null || hit.message_id === null) return;
      dispatch({
        type: "select_message",
        message: { row_id: hit.row_id, message_id: hit.message_id, selector: hit.selector },
        ...(focus ? { focus } : {}),
      });
    },
    [dispatch],
  );
  const onSelect = useCallback((hit: SearchHit) => pick(hit), [pick]);
  const onOpen = useCallback((hit: SearchHit) => pick(hit, "reader"), [pick]);

  if (search.hits.length === 0) {
    const busy = search.status === "searching" || search.status === "running";
    return (
      <p className="p-4 text-sm text-muted-foreground">
        {busy ? "No results yet." : search.status === "done" ? "Nothing matches." : "No results."}
      </p>
    );
  }
  return (
    <div ref={ref} role="listbox" aria-label={`Search results for ${search.query}`}>
      {search.hits.map((h, i) => (
        <SearchHitRow
          key={h.key}
          hit={h}
          mailboxLabel={h.origin === "local" ? labelOf(h.mailbox) : h.mailbox}
          selected={i === selectedIndex}
          tabStop={i === tabIndexRow}
          position={i + 1}
          setSize={search.hits.length}
          onSelect={onSelect}
          onOpen={onOpen}
        />
      ))}
    </div>
  );
}

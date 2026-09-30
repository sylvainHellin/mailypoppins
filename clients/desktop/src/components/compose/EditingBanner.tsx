// The drafts open in the external editor, one line each above the panes.
// The region is mounted empty, since a live region that mounts with its
// text already inside may not be announced.

import { CircleAlert, Pencil, RotateCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { openInEditor } from "@/app/compose";
import { useAppState, useDispatch } from "@/app/store";
import type { ComposeSession } from "@/app/state";

function line(c: ComposeSession): string {
  if (c.status === "opening") return `Opening ${c.name} in the editor…`;
  if (c.status === "error") return `${c.name} did not open in the editor`;
  return `Editing ${c.name} in ${c.editor ?? "the editor"}; each save updates the list`;
}

/**
 * "Reopen in editor" runs `editor_open` again on the same file; "Done"
 * forgets the session, since the editor process is not ours to close. A
 * discard or a `state.remove` of the draft ends the session too.
 */
export function EditingBanner() {
  const s = useAppState();
  const dispatch = useDispatch();
  const sessions = Object.values(s.compose);
  return (
    <div role="status" aria-label="Drafts in the editor" data-slot="editing-banner" className="contents">
      {sessions.map((c) => (
        <div
          key={`${c.account}#${c.draftId}`}
          data-editing={c.draftId}
          data-status={c.status}
          className="flex items-center gap-2 border-b border-border bg-card px-4 py-1.5 text-sm"
        >
          {c.status === "error" ? (
            <CircleAlert aria-hidden="true" className="size-4 shrink-0 text-destructive" />
          ) : (
            <Pencil aria-hidden="true" className="size-4 shrink-0 text-link" />
          )}
          <span className="min-w-0 flex-1 truncate" title={c.path}>
            {line(c)}
          </span>
          <Button
            size="xs"
            variant="outline"
            aria-label={`Reopen in editor: ${c.name}`}
            disabled={c.status === "opening"}
            onClick={() => void openInEditor(dispatch, c.account, c.draftId, c.path)}
          >
            <RotateCw aria-hidden="true" />
            Reopen in editor
          </Button>
          <Button size="xs" variant="ghost" aria-label={`Done editing ${c.name}`} onClick={() => dispatch({ type: "compose_done", account: c.account, draftId: c.draftId })}>
            Done
          </Button>
        </div>
      ))}
    </div>
  );
}

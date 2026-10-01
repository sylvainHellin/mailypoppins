// The drafts open in an editor, one line each above the panes: the
// external editor's, and the embedded editor's (ticket 0130) while it runs,
// shown or in the background, or once it ended without code 0.
// The region is mounted empty, since a live region that mounts with its
// text already inside may not be announced.

import { CircleAlert, Eye, Pencil, RotateCw, SquareTerminal } from "lucide-react";
import { Button } from "@/components/ui/button";
import { reopen } from "@/app/compose";
import { useAppState, useDispatch } from "@/app/store";
import { targetKey, type ComposeSession } from "@/app/state";

/** What the banner says of a session; `shown` is whether the reader area shows its editor. */
export function bannerLine(c: ComposeSession, shown: boolean): string {
  const editor = c.editor ?? "the editor";
  if (c.kind === "external") {
    if (c.status === "opening") return `Opening ${c.name} in the editor…`;
    if (c.status === "error") return `${c.name} did not open in the editor`;
    return `Editing ${c.name} in ${editor}; each save updates the list`;
  }
  const st = c.status;
  switch (st.kind) {
    case "running":
      if (c.session === null) return `Opening ${c.name} in the editor…`;
      return shown
        ? `Editing ${c.name} in ${editor}; each save updates the list`
        : `${c.name} is open in ${editor} in the background`;
    case "exited":
      return `The editor of ${c.name} exited with status ${st.code}; the draft keeps what was saved`;
    case "crashed":
      return st.signal !== null
        ? `The editor of ${c.name} was ended by signal ${st.signal}; the draft keeps what was saved`
        : `The editor of ${c.name} ended with no status; the draft keeps what was saved`;
    case "failed":
      return `${c.name} did not open in the editor`;
  }
}

/** The banner's state of a session, its `data-status`. */
function statusOf(c: ComposeSession, shown: boolean): string {
  if (c.kind === "external") return c.status;
  if (c.status.kind !== "running") return c.status.kind;
  if (c.session === null) return "opening";
  return shown ? "editing" : "background";
}

/**
 * "Reopen in editor" runs the editor again on the same file: `editor_open`
 * for the external route, a fresh embedded process for an embedded session
 * that ended. "Show" brings a background embedded editor back to the reader
 * area. "Done" forgets the session: the external editor's process is not
 * the app's to close, and an embedded one that ended has nothing left to
 * kill. A discard or a `state.remove` of the draft ends the session too.
 */
export function EditingBanner() {
  const s = useAppState();
  const dispatch = useDispatch();
  const sessions = Object.values(s.compose);
  return (
    <div role="status" aria-label="Drafts in the editor" data-slot="editing-banner" className="contents">
      {sessions.map((c) => {
        const key = targetKey({ account: c.account, draft: c.draftId });
        const shown = s.composeShown === key;
        const status = statusOf(c, shown);
        const running = c.kind === "embedded" && c.status.kind === "running";
        const alert = status === "error" || status === "failed" || status === "exited" || status === "crashed";
        const Icon = alert ? CircleAlert : c.kind === "embedded" ? SquareTerminal : Pencil;
        return (
          <div
            key={key}
            data-editing={c.draftId}
            data-status={status}
            data-route={c.kind}
            className="flex items-center gap-2 border-b border-border bg-card px-4 py-1.5 text-sm"
          >
            <Icon aria-hidden="true" className={alert ? "size-4 shrink-0 text-destructive" : "size-4 shrink-0 text-link"} />
            <span className="min-w-0 flex-1 truncate" title={c.path}>
              {bannerLine(c, shown)}
            </span>
            {running && !shown ? (
              <Button
                size="xs"
                variant="outline"
                aria-label={`Show the editor: ${c.name}`}
                onClick={() => dispatch({ type: "compose_show", account: c.account, draftId: c.draftId })}
              >
                <Eye aria-hidden="true" />
                Show
              </Button>
            ) : null}
            {running ? null : (
              <Button
                size="xs"
                variant="outline"
                aria-label={`Reopen in editor: ${c.name}`}
                disabled={status === "opening"}
                onClick={() => reopen(c, dispatch)}
              >
                <RotateCw aria-hidden="true" />
                Reopen in editor
              </Button>
            )}
            {running ? null : (
              <Button size="xs" variant="ghost" aria-label={`Done editing ${c.name}`} onClick={() => dispatch({ type: "compose_done", account: c.account, draftId: c.draftId })}>
                Done
              </Button>
            )}
          </div>
        );
      })}
    </div>
  );
}

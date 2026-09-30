import { useEffect } from "react";
import { CloudOff, RefreshCw, Power } from "lucide-react";
import { bannerFor, type AppState } from "@/app/state";
import { queueDepth, queueText } from "@/app/outbox";
import { useDispatch } from "@/app/store";

const NOTICE_MS = 4000;

/**
 * The connection banners, the transient notices and the queue depth
 * (SYN-06), in one polite live region that is always mounted, so a screen
 * reader hears each change.
 */
export function StatusRegion({ state }: { state: AppState }) {
  const dispatch = useDispatch();
  const banner = bannerFor(state);
  const notice = state.notice;
  const queue = queueText(queueDepth(state));
  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => dispatch({ type: "notice", text: null }), NOTICE_MS);
    return () => clearTimeout(t);
  }, [notice, dispatch]);

  return (
    <div aria-live="polite" role="status" className="contents">
      {banner?.kind === "reconnecting" ? (
        <div data-banner="reconnecting" className="flex items-center gap-2 border-b border-border bg-card px-4 py-1.5 text-sm text-warning">
          <CloudOff aria-hidden="true" className="size-4" />
          <span>Reconnecting to the daemon: {banner.reason}. What you see may be out of date.</span>
        </div>
      ) : banner?.kind === "resync" ? (
        <div data-banner="resync" className="flex items-center gap-2 border-b border-border bg-card px-4 py-1.5 text-sm text-link">
          <RefreshCw aria-hidden="true" className="size-4 animate-spin" />
          <span>Resynchronising with the daemon ({banner.reason})…</span>
        </div>
      ) : banner?.kind === "shutting_down" ? (
        <div data-banner="shutting_down" className="flex items-center gap-2 border-b border-border bg-card px-4 py-1.5 text-sm text-warning">
          <Power aria-hidden="true" className="size-4" />
          <span>The daemon is shutting down.</span>
        </div>
      ) : null}
      {notice ? (
        <div className="pointer-events-none fixed bottom-4 left-1/2 z-40 -translate-x-1/2 rounded-lg border border-border bg-popover px-3 py-1.5 text-sm text-popover-foreground shadow-md">
          {notice}
        </div>
      ) : null}
      {/* Mounted at 0, visually hidden and empty, so the first depth is announced. */}
      <div
        data-slot="queue-depth"
        className={queue ? "pointer-events-none fixed bottom-4 left-4 z-30 rounded-md border border-border bg-card px-2 py-1 text-xs text-muted-foreground tabular-nums shadow-sm" : "sr-only"}
      >
        {queue}
      </div>
    </div>
  );
}

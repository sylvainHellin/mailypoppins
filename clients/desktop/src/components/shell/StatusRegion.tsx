import { useCallback } from "react";
import { CircleAlert, CloudOff, RefreshCw, Power, SquarePen } from "lucide-react";
import { Button } from "@/components/ui/button";
import { bannerFor, type AppState } from "@/app/state";
import { checkLabel, shownChecks } from "@/app/health";
import { openConfig } from "@/app/interop";
import type { HealthCheck } from "@/protocol/types";
import { queueDepth, queueText } from "@/app/outbox";
import { useDispatch } from "@/app/store";
import { LIFETIME_MS } from "@/app/activity";
import { useWindDown } from "@/hooks/use-wind-down";

/**
 * The notice line: it leaves after LIFETIME_MS's line, held while the pointer
 * rests on it, and a new text starts the time again. Its own component, so
 * its timer and hover end with it.
 */
function NoticeLine({ text }: { text: string }) {
  const dispatch = useDispatch();
  const done = useCallback(() => dispatch({ type: "notice", text: null }), [dispatch]);
  const hover = useWindDown(LIFETIME_MS.line, done, text);
  return (
    <div
      data-slot="notice-line"
      {...hover}
      className="fixed bottom-4 left-1/2 z-40 -translate-x-1/2 rounded-lg border border-border bg-popover px-3 py-1.5 text-sm text-popover-foreground shadow-md"
    >
      {text}
    </div>
  );
}

/**
 * One health check that is not ok, a line under the connection banner: the
 * icon carries the level, the text stays calm, and `config_loaded` offers the
 * file, which is where a warning about it is fixed.
 */
function HealthLine({ check }: { check: HealthCheck }) {
  const dispatch = useDispatch();
  return (
    <div
      data-health={check.name}
      data-status={check.status}
      className="flex items-center gap-2 border-b border-border bg-card px-4 py-1.5 text-sm"
    >
      <CircleAlert aria-hidden="true" className={`size-4 shrink-0 ${check.status === "fail" ? "text-destructive" : "text-warning"}`} />
      <span className="min-w-0 flex-1 break-words">
        {checkLabel(check.name)}: {check.detail}
      </span>
      {check.name === "config_loaded" ? (
        <Button size="xs" variant="outline" onClick={() => void openConfig(dispatch)}>
          <SquarePen aria-hidden="true" />
          Open config.toml
        </Button>
      ) : null}
    </div>
  );
}

/**
 * The connection banners, the daemon's health checks that are not ok, the
 * transient notices and the queue depth (SYN-06), in one polite live region
 * that is always mounted, so a screen reader hears each change.
 */
export function StatusRegion({ state }: { state: AppState }) {
  const banner = bannerFor(state);
  const notice = state.notice;
  const queue = queueText(queueDepth(state));

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
      {shownChecks(state).map((check) => (
        <HealthLine key={check.name} check={check} />
      ))}
      {notice ? <NoticeLine text={notice} /> : null}
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

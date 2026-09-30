import { Copy, ExternalLink, ShieldAlert, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { canOpenExternally, copyLink, openInBrowser } from "@/components/reader/links";
import { useDispatch } from "@/app/store";
import type { InterceptedUrl } from "@/lib/gui-types";

/**
 * The reader footer's notice for a link the frame was refused. Non-modal:
 * nothing opens until the user clicks "Open in browser".
 */
export function InterceptedLinkNotice({ entry }: { entry: InterceptedUrl }) {
  const dispatch = useDispatch();
  const url = entry.url;
  return (
    <div
      role="region"
      aria-label="Blocked link"
      data-slot="intercept-notice"
      className="flex shrink-0 items-center gap-2 border-t border-border bg-card px-3 py-2 text-xs"
    >
      <ShieldAlert aria-hidden="true" className="size-4 shrink-0 text-warning" />
      <span className="shrink-0 text-muted-foreground">Link blocked:</span>
      <span className="min-w-0 flex-1 truncate font-mono" title={url} data-slot="intercept-url">
        {url}
      </span>
      <Button
        size="xs"
        variant="outline"
        disabled={!canOpenExternally(url)}
        onClick={() => openInBrowser(url, dispatch)}
      >
        <ExternalLink aria-hidden="true" />
        Open in browser
      </Button>
      <Button size="xs" variant="ghost" onClick={() => copyLink(url, dispatch)}>
        <Copy aria-hidden="true" />
        Copy
      </Button>
      <Button size="icon-xs" variant="ghost" aria-label="Dismiss" onClick={() => dispatch({ type: "dismiss_intercept" })}>
        <X aria-hidden="true" />
      </Button>
    </div>
  );
}

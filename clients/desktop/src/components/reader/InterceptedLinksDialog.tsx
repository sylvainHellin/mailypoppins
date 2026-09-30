import { useEffect } from "react";
import { Copy, ExternalLink } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { canOpenExternally, copyLink, openInBrowser } from "@/components/reader/links";
import { useAppState, useDispatch } from "@/app/store";
import * as cmd from "@/lib/commands";
import type { InterceptedUrl } from "@/lib/gui-types";

const SOURCE: Record<InterceptedUrl["source"], string> = {
  navigation: "link",
  new_window: "new window",
  open_external_stub: "opener stub",
};

/**
 * The intercepted-URL log. `intercepted_urls` drains the Rust log, so what it
 * answers is merged into the model's copy, which this lists newest first.
 */
export function InterceptedLinksDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const s = useAppState();
  const dispatch = useDispatch();
  useEffect(() => {
    if (!open) return;
    let live = true;
    cmd
      .interceptedUrls()
      .then((urls) => live && dispatch({ type: "intercepted_fetched", urls }))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [open, dispatch]);

  const entries = [...s.intercepted].reverse();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Intercepted links</DialogTitle>
          <DialogDescription>
            Navigations the reader refused. Nothing opens in a browser unless you click Open.
          </DialogDescription>
        </DialogHeader>
        {entries.length === 0 ? (
          <p className="text-sm text-muted-foreground">No link has been intercepted.</p>
        ) : (
          <ul aria-label="Intercepted links" className="flex max-h-[60vh] flex-col gap-1 overflow-y-auto">
            {entries.map((e) => (
              <li key={`${e.at}|${e.source}|${e.url}`} className="flex items-center gap-2 text-xs">
                <span className="w-20 shrink-0 text-muted-foreground">{SOURCE[e.source]}</span>
                <span className="min-w-0 flex-1 truncate font-mono" title={e.url}>
                  {e.url}
                </span>
                <Button
                  size="icon-xs"
                  variant="ghost"
                  aria-label={`Open ${e.url} in browser`}
                  disabled={!canOpenExternally(e.url)}
                  onClick={() => openInBrowser(e.url, dispatch)}
                >
                  <ExternalLink aria-hidden="true" />
                </Button>
                <Button size="icon-xs" variant="ghost" aria-label={`Copy ${e.url}`} onClick={() => copyLink(e.url, dispatch)}>
                  <Copy aria-hidden="true" />
                </Button>
              </li>
            ))}
          </ul>
        )}
      </DialogContent>
    </Dialog>
  );
}

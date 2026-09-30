// The refused configuration, one line above the panes: the daemon's
// `config.invalid` names the file, the line and why, and the daemon keeps
// serving the configuration it had until a `config.changed` says one loaded.
// A daemon that started on a file that did not load serves none, and says
// so through `config.get`'s state.
// An alert is announced when it is inserted, so the line mounts with its
// text and nothing is left behind once it goes.

import { FileWarning, SquarePen } from "lucide-react";
import { Button } from "@/components/ui/button";
import { openConfig } from "@/app/interop";
import { useAppState, useDispatch } from "@/app/store";

export function ConfigProblemBanner() {
  const s = useAppState();
  const dispatch = useDispatch();
  const p = s.configProblem;
  if (!p) return null;
  const where = p.line === null ? p.path : `${p.path}, line ${p.line}`;
  const why = p.message === "" ? "" : `: ${p.message}`;
  const text = p.atStartup
    ? `config.toml did not load (${where})${why}; the daemon serves no configuration until a reload loads it`
    : `config.toml was refused (${where})${why}; the daemon keeps the configuration it had`;
  return (
    <div
      role="alert"
      aria-label="Configuration problem"
      data-slot="config-problem"
      className="flex items-center gap-2 border-b border-border bg-card px-4 py-1.5 text-sm"
    >
      <FileWarning aria-hidden="true" className="size-4 shrink-0 text-destructive" />
      <span className="min-w-0 flex-1 truncate" title={where}>
        {text}
      </span>
      <Button size="xs" variant="outline" onClick={() => void openConfig(dispatch)}>
        <SquarePen aria-hidden="true" />
        Open config.toml
      </Button>
    </div>
  );
}

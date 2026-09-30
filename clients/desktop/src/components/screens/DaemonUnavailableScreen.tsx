import { RotateCw, ServerOff } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PathRow, ScreenFrame } from "@/components/screens/ScreenFrame";
import type { ConnectError } from "@/lib/gui-types";

export type DaemonUnavailableProps = {
  error: ConnectError;
  onRetry: () => void;
  onRestart: () => void;
};

/** The first connect failed: why, where to look, and the two ways forward. */
export function DaemonUnavailableScreen({ error, onRetry, onRestart }: DaemonUnavailableProps) {
  const title =
    error.kind === "identity_mismatch" ? "The socket answers as another daemon" : "The mailypoppins daemon is not available";
  return (
    <ScreenFrame title={title} icon={<ServerOff aria-hidden="true" className="size-6 text-warning" />}>
      <p role="alert" className="text-sm">
        {error.why}
      </p>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
        <PathRow label="Socket" value={error.socket} />
        <PathRow label="Log" value={error.log} />
      </dl>
      <p className="text-sm text-muted-foreground">
        The desktop client never reads the mail store without the daemon. Start it with <code>mp daemon start</code>,
        or retry once it runs.
      </p>
      <div className="flex flex-wrap gap-2">
        <Button onClick={onRetry}>
          <RotateCw aria-hidden="true" />
          Retry
        </Button>
        <Button variant="outline" onClick={onRestart}>
          Restart daemon…
        </Button>
      </div>
    </ScreenFrame>
  );
}

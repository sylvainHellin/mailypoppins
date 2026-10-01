import { TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PathRow, ScreenFrame } from "@/components/screens/ScreenFrame";
import type { ConnectError, VersionInfo } from "@/lib/gui-types";

export type VersionMismatchProps = {
  error: ConnectError;
  version: VersionInfo | null;
  onRestart: () => void;
};

/**
 * Blocking: the running daemon speaks a protocol this app does not, or runs
 * another version than the `mp` the app starts. The refusal's own daemon
 * version comes first, since the last handshake may be an older daemon's.
 */
export function VersionMismatchScreen({ error, version, onRestart }: VersionMismatchProps) {
  const daemon = error.daemon_version ?? version?.daemon?.daemon_version ?? "unknown";
  const range = version ? `${version.protocol_min} to ${version.protocol_max}` : "unknown";
  return (
    <ScreenFrame
      title="The daemon runs an incompatible version"
      icon={<TriangleAlert aria-hidden="true" className="size-6 text-warning" />}
    >
      <p role="alert" className="text-sm">
        {error.why}
      </p>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
        <PathRow label="App version" value={version?.app_version} />
        <PathRow label="App protocol" value={range} />
        <PathRow label="Daemon version" value={daemon} />
        <PathRow label="Log" value={error.log} />
      </dl>
      <p className="text-sm text-muted-foreground">
        Restarting the daemon runs the <code>mp</code> this app ships with or finds on the PATH. Other clients lose
        their connection for a moment.
      </p>
      <div>
        <Button onClick={onRestart}>Restart daemon…</Button>
      </div>
    </ScreenFrame>
  );
}

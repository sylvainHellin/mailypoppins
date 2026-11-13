import { TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PathRow, ScreenFrame } from "@/components/screens/ScreenFrame";
import type { ConnectError, VersionInfo } from "@/lib/gui-types";

export type VersionMismatchProps = {
  error: ConnectError;
  version: VersionInfo | null;
  onRestart: () => void;
};

/** `1 to 1`, as the App protocol row has always spelled a range. */
function range(min: number, max: number): string {
  return `${min} to ${max}`;
}

/**
 * Blocking: the running daemon speaks a protocol this app does not, or runs
 * another version than the `mp` the app starts. The refusal's own daemon
 * version comes first, since the last handshake may be an older daemon's;
 * a daemon of 0.12.0 or older refuses without naming its version (PERSO-277),
 * which the row says instead of "unknown". A daemon whose lowest protocol is
 * above this app's highest is the newer side: restarting it from this app
 * would put it back on the older `mp`, so the screen says to update the app
 * in the words `mp` and the TUI use for their own update, and offers no
 * restart; the button shows only when the daemon is the older side, or when
 * nothing says which side is (`mp_client::VersionRefusal::client_is_older`).
 */
export function VersionMismatchScreen({ error, version, onRestart }: VersionMismatchProps) {
  const daemon =
    error.daemon_version ?? version?.daemon?.daemon_version ?? "not reported: an older daemon than this app";
  const ours = version ? range(version.protocol_min, version.protocol_max) : null;
  const theirs = error.daemon_protocol ? range(error.daemon_protocol.min, error.daemon_protocol.max) : null;
  const appIsOlder = !!error.daemon_protocol && !!version && error.daemon_protocol.min > version.protocol_max;
  const matching = error.daemon_version ? `the matching version (mailypoppins ${error.daemon_version})` : "the version the daemon runs";
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
        <PathRow label="App protocol" value={ours} />
        <PathRow label="Daemon version" value={daemon} />
        {theirs && <PathRow label="Daemon protocol" value={theirs} />}
        <PathRow label="Log" value={error.log} />
      </dl>
      {appIsOlder ? (
        <p className="text-sm text-muted-foreground">
          {`Update this app to ${matching} and open it again, since restarting the daemon from it would put the daemon back on this older build.`}
        </p>
      ) : (
        <>
          <p className="text-sm text-muted-foreground">
            Restart the daemon to run the version this app expects: it runs the <code>mp</code> this app ships with or
            finds on the PATH, and other clients lose their connection for a moment.
          </p>
          <div>
            <Button onClick={onRestart}>Restart daemon…</Button>
          </div>
        </>
      )}
    </ScreenFrame>
  );
}

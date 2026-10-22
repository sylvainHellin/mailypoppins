import { UserPlus } from "lucide-react";
import { AccountWizardForm } from "@/components/settings/AccountWizard";
import { PathRow, ScreenFrame } from "@/components/screens/ScreenFrame";

/**
 * First run: the daemon has no config.toml and serves no account, so the
 * wizard writes the first one (`config_init`). Once the daemon's
 * `config.changed` names the account and the account list has it, the
 * shell replaces this screen. The wizard tested the account before the
 * write and stores its passwords after it, whether or not the screen is
 * still there; a password the daemon did not store opens the password
 * dialog, an overlay, which survives the swap.
 */
export function SetupScreen({ path }: { path: string }) {
  return (
    <ScreenFrame title="Set up mailypoppins" icon={<UserPlus aria-hidden="true" className="size-6 text-primary" />}>
      <p className="text-sm">The daemon is running but has no configuration yet. Add the first account and it writes one.</p>
      <dl className="grid grid-cols-[6rem_1fr] gap-x-2 text-sm">
        <PathRow label="Config file" value={path} />
      </dl>
      <AccountWizardForm preset="imap" init taken={[]} />
    </ScreenFrame>
  );
}

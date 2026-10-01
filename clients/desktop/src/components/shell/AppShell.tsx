import { useCallback, useEffect, useRef } from "react";
import { Shell } from "@/components/shell/Shell";
import { ConnectingScreen } from "@/components/screens/ConnectingScreen";
import { DaemonUnavailableScreen } from "@/components/screens/DaemonUnavailableScreen";
import { VersionMismatchScreen } from "@/components/screens/VersionMismatchScreen";
import { RestartDaemonDialog } from "@/components/screens/RestartDaemonDialog";
import { CommandPalette } from "@/components/palette/CommandPalette";
import { KeyHelp } from "@/components/palette/KeyHelp";
import { PrefixPopup } from "@/components/palette/PrefixPopup";
import { InterceptedLinksDialog } from "@/components/reader/InterceptedLinksDialog";
import { ConfirmMutationDialog } from "@/components/mutations/ConfirmMutationDialog";
import { MovePicker } from "@/components/mutations/MovePicker";
import { ComposeWizard } from "@/components/compose/ComposeWizard";
import { ComposeLeaveDialog } from "@/components/compose/ComposeLeaveDialog";
import { TerminalHost, TerminalSlotProvider } from "@/components/compose/TerminalHost";
import { AttachmentsDialog } from "@/components/attachments/AttachmentsDialog";
import { RsvpDialog } from "@/components/calendar/RsvpDialog";
import { NewInvitationDialog } from "@/components/calendar/NewInvitationDialog";
import { SignaturesDialog } from "@/components/signatures/SignaturesDialog";
import { ActivityLogDialog } from "@/components/activity/ActivityLogDialog";
import { PasswordDialog } from "@/components/settings/PasswordDialog";
import { AccountWizard } from "@/components/settings/AccountWizard";
import { DeviceCodeDialog } from "@/components/settings/DeviceCodeDialog";
import { SetupScreen } from "@/components/screens/SetupScreen";
import { MENU_ACTIONS, runDialog, useRunAction, type ListGeometry } from "@/app/actions";
import { useCloseGuard } from "@/app/compose";
import { useBoot, useDataSync, useVersionInfo } from "@/app/data";
import { useLayout } from "@/app/layout";
import { useStoredTheme } from "@/app/theme";
import { useStoredReaderMode } from "@/app/readerMode";
import { screenFor } from "@/app/state";
import { useAppState, useDispatch } from "@/app/store";
import { useKeymap } from "@/keymap/useKeymap";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";

/** The root: boot, loaders, keyboard, and the screen the connection calls for. */
export function AppShell() {
  const s = useAppState();
  const dispatch = useDispatch();
  // What the Shell draws, so Widen and Narrow list step from it.
  const listGeometry = useRef<ListGeometry | null>(null);
  const run = useRunAction(s, dispatch, listGeometry);

  const onMenu = useCallback(
    (id: string) => {
      const action = MENU_ACTIONS[id];
      if (action) run(action);
    },
    [run],
  );
  useBoot(dispatch, onMenu);
  useVersionInfo(s, dispatch);
  useStoredTheme(dispatch);
  useStoredReaderMode(dispatch);
  useDataSync(s, dispatch);
  useKeymap(s, dispatch);
  useCloseGuard(s, dispatch);

  const layout = useLayout();
  useEffect(() => dispatch({ type: "set_layout", layout }), [layout, dispatch]);

  const restart = useCallback(() => {
    cmd.restartDaemon().catch((e: unknown) => {
      const err = asGuiError(e);
      dispatch({ type: "error", error: err });
      dispatch({ type: "notice", text: `The restart failed: ${err.message}` });
    });
  }, [dispatch]);
  const retry = useCallback(() => {
    cmd.retryConnect().catch((e: unknown) => dispatch({ type: "error", error: asGuiError(e) }));
  }, [dispatch]);
  const askRestart = useCallback(() => dispatch({ type: "overlay", overlay: "restart" }), [dispatch]);

  const screen = screenFor(s);
  let body;
  if (screen === "unavailable" && s.connection.state === "failed") {
    body = <DaemonUnavailableScreen error={s.connection.error} onRetry={retry} onRestart={askRestart} />;
  } else if (screen === "version_mismatch" && s.connection.state === "failed") {
    body = <VersionMismatchScreen error={s.connection.error} version={s.version} onRestart={askRestart} />;
  } else if (screen === "connecting") {
    body = <ConnectingScreen reason={s.connection.state === "reconnecting" ? s.connection.reason : null} />;
  } else if (screen === "setup") {
    body = <SetupScreen path={s.config.data?.path ?? ""} />;
  } else {
    body = <Shell listGeometry={listGeometry} />;
  }

  const close = (open: boolean) => {
    if (!open) dispatch({ type: "overlay", overlay: null });
  };
  // The embedded editors live beside the body, so a screen change never
  // unmounts one (components/compose/TerminalHost.tsx).
  return (
    <TerminalSlotProvider>
      {body}
      <TerminalHost />
      <PrefixPopup state={s} />
      <CommandPalette open={s.overlay === "palette"} onOpenChange={close} onRun={run} />
      <KeyHelp open={s.overlay === "help"} onOpenChange={close} />
      <RestartDaemonDialog open={s.overlay === "restart"} onOpenChange={close} onConfirm={restart} />
      <InterceptedLinksDialog open={s.overlay === "intercepted"} onOpenChange={close} />
      <ConfirmMutationDialog
        dialog={s.overlay === "mutation" && s.dialog && s.dialog.kind !== "move" ? s.dialog : null}
        onOpenChange={close}
        onConfirm={() => s.dialog && runDialog(s.dialog, dispatch, undefined, s.compose)}
      />
      <MovePicker
        state={s}
        dialog={s.overlay === "mutation" && s.dialog?.kind === "move" ? s.dialog : null}
        onOpenChange={close}
        onPick={(slug) => s.dialog && runDialog(s.dialog, dispatch, slug)}
      />
      <ComposeWizard dialog={s.overlay === "compose" ? s.composeDialog : null} onOpenChange={close} />
      <AttachmentsDialog dialog={s.overlay === "attachments" ? s.attachDialog : null} onOpenChange={close} />
      <RsvpDialog dialog={s.overlay === "rsvp" ? s.rsvpDialog : null} onOpenChange={close} />
      <NewInvitationDialog dialog={s.overlay === "invite" ? s.inviteDialog : null} onOpenChange={close} />
      <SignaturesDialog dialog={s.overlay === "signatures" ? s.signaturesDialog : null} onOpenChange={close} />
      <ActivityLogDialog open={s.overlay === "activity"} onOpenChange={close} />
      <PasswordDialog dialog={s.overlay === "password" ? s.passwordDialog : null} onOpenChange={close} />
      <AccountWizard dialog={s.overlay === "account_wizard" ? s.accountWizard : null} onOpenChange={close} />
      <DeviceCodeDialog open={s.overlay === "device_code"} />
      <ComposeLeaveDialog />
    </TerminalSlotProvider>
  );
}

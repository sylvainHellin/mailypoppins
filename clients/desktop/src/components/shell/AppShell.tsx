import { useCallback, useEffect, useRef } from "react";
import { Shell } from "@/components/shell/Shell";
import { ConnectingScreen } from "@/components/screens/ConnectingScreen";
import { DaemonUnavailableScreen } from "@/components/screens/DaemonUnavailableScreen";
import { VersionMismatchScreen } from "@/components/screens/VersionMismatchScreen";
import { RestartDaemonDialog } from "@/components/screens/RestartDaemonDialog";
import { CommandPalette } from "@/components/palette/CommandPalette";
import { KeyHelp } from "@/components/palette/KeyHelp";
import { InterceptedLinksDialog } from "@/components/reader/InterceptedLinksDialog";
import { MENU_ACTIONS, useRunAction, type ListGeometry } from "@/app/actions";
import { useBoot, useDataSync, useVersionInfo } from "@/app/data";
import { useLayout } from "@/app/layout";
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
  useDataSync(s, dispatch);
  useKeymap(s, dispatch);

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
  } else {
    body = <Shell listGeometry={listGeometry} />;
  }

  const close = (open: boolean) => {
    if (!open) dispatch({ type: "overlay", overlay: null });
  };
  return (
    <>
      {body}
      <CommandPalette open={s.overlay === "palette"} onOpenChange={close} onRun={run} />
      <KeyHelp open={s.overlay === "help"} onOpenChange={close} />
      <RestartDaemonDialog open={s.overlay === "restart"} onOpenChange={close} onConfirm={restart} />
      <InterceptedLinksDialog open={s.overlay === "intercepted"} onOpenChange={close} />
    </>
  );
}

// The embedded editors in the shell (ticket 0130, U3): one TerminalPane per
// embedded session of `state.compose`, drawn over the reader area.
// docs/shell.md, "Compose", is the description.
//
// A pane spawns its editor when it mounts and its terminal buffer dies when
// it unmounts, so the panes live here, under AppShell, which stays mounted
// whatever the shell shows: a view, the narrow layout's sidebar, the
// connecting screen of a daemon restart. The reader area renders a slot
// (`TerminalSlot`) and the host is laid over it, `position: fixed` at the
// slot's rectangle; with no slot every pane is hidden and keeps running.

import {
  createContext,
  lazy,
  Suspense,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type Dispatch,
  type ReactNode,
} from "react";
import { useAppState, useDispatch } from "@/app/store";
import type { Action } from "@/app/reducer";
import type { EmbeddedSession } from "@/app/state";
import { asGuiError, fixtureNotice } from "@/lib/gui-types";
import { tauriBridge, type TerminalBridge, type TerminalExit, type TerminalStarted } from "@/lib/terminal";

// xterm and its addons load with the first embedded session, not with the app.
const TerminalPane = lazy(() => import("@/components/compose/TerminalPane").then((m) => ({ default: m.TerminalPane })));

type SlotContext = { slot: HTMLElement | null; setSlot: (el: HTMLElement | null) => void };

const Slot = createContext<SlotContext | null>(null);

/** Shares the reader area's slot between the Shell, which renders it, and the host. */
export function TerminalSlotProvider({ children }: { children: ReactNode }) {
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  return <Slot.Provider value={{ slot, setSlot }}>{children}</Slot.Provider>;
}

/** Where the reader area's terminal goes; the host is laid over it. */
export function TerminalSlot() {
  const ctx = useContext(Slot);
  return <div ref={ctx?.setSlot} data-slot="terminal-slot" className="min-h-0 min-w-0 flex-1" />;
}

type Rect = { left: number; top: number; width: number; height: number };

const sameRect = (a: Rect | null, b: Rect | null) =>
  a === b || (a !== null && b !== null && a.left === b.left && a.top === b.top && a.width === b.width && a.height === b.height);

/** The slot's rectangle in the viewport, followed through resizes and every render of the host. */
function useSlotRect(slot: HTMLElement | null): Rect | null {
  const [rect, setRect] = useState<Rect | null>(null);
  const measure = useRef(() => {});
  measure.current = () => {
    const r = slot?.getBoundingClientRect();
    const next = r ? { left: r.left, top: r.top, width: r.width, height: r.height } : null;
    setRect((prev) => (sameRect(prev, next) ? prev : next));
  };
  // A banner line or the splitter moves the slot without resizing the window.
  useLayoutEffect(() => measure.current());
  useEffect(() => {
    if (!slot) return;
    const on = () => measure.current();
    const observer = new ResizeObserver(on);
    observer.observe(slot);
    window.addEventListener("resize", on);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", on);
    };
  }, [slot]);
  return slot ? rect : null;
}

type PaneProps = { session: EmbeddedSession; visible: boolean; bridge: TerminalBridge; dispatch: Dispatch<Action> };

/**
 * One session's pane, keyed by the draft and its spawn. It frees the PTY
 * after the exit frame (`terminal_kill`, which the Rust layer needs to drop
 * the session), and kills the child when it unmounts while the child may
 * still run: a session the model forgot (Close the editor, a discard, the
 * draft's `state.remove`, Done) takes its process with it.
 */
function EmbeddedPane({ session: c, visible, bridge, dispatch }: PaneProps) {
  const started = useRef<number | null>(null);
  const killed = useRef(false);
  const mounted = useRef(false);
  const ids = { account: c.account, draftId: c.draftId, spawn: c.spawn };

  const kill = () => {
    if (started.current === null || killed.current) return;
    killed.current = true;
    bridge.kill(started.current).catch(() => {});
  };
  const killRef = useRef(kill);
  killRef.current = kill;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      killRef.current();
    };
  }, []);

  const onStarted = (answer: TerminalStarted) => {
    started.current = answer.session;
    // Unmounted while the spawn was in flight: nothing will show it.
    if (!mounted.current) return kill();
    dispatch({ type: "compose_started", ...ids, session: answer.session, editor: answer.editor });
    const fixture = fixtureNotice(answer);
    if (fixture) dispatch({ type: "notice", text: fixture });
  };
  const onExit = (exit: TerminalExit) => {
    kill();
    dispatch({ type: "compose_exited", ...ids, code: exit.code, signal: exit.signal });
  };
  const onError = (e: unknown) => {
    // A refused spawn; a refused write or resize of a live session changes nothing the user can act on.
    if (started.current === null) dispatch({ type: "compose_spawn_failed", ...ids, error: asGuiError(e) });
  };

  return (
    <TerminalPane
      session={{ account: c.account, id: c.draftId, path: c.path }}
      bridge={bridge}
      visible={visible}
      onStarted={onStarted}
      onExit={onExit}
      onError={onError}
    />
  );
}

/** Every embedded session's pane; the shown one over the reader area's slot, the rest hidden. */
export function TerminalHost({ bridge = tauriBridge }: { bridge?: TerminalBridge }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const slot = useContext(Slot)?.slot ?? null;
  const rect = useSlotRect(slot);
  const sessions = Object.entries(s.compose).filter(
    (e): e is [string, EmbeddedSession] => e[1].kind === "embedded" && e[1].status.kind !== "failed",
  );
  const shown = slot ? s.composeShown : null;
  return (
    <div
      data-slot="terminal-host"
      hidden={!shown}
      className="fixed z-10 flex"
      style={rect ? { left: rect.left, top: rect.top, width: rect.width, height: rect.height } : undefined}
      onFocus={() => dispatch({ type: "pane_focused", pane: "reader" })}
    >
      {sessions.length > 0 ? (
        <Suspense fallback={null}>
          {sessions.map(([key, c]) => (
            <EmbeddedPane key={`${key}#${c.spawn}`} session={c} visible={key === shown} bridge={bridge} dispatch={dispatch} />
          ))}
        </Suspense>
      ) : null}
    </div>
  );
}

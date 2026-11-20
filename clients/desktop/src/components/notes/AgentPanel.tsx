// The agent's terminal panel beside the reader (PERSO-315): a hand-off opens
// the `[agent]` command on the batch prompt in a terminal session, the
// embedded editor's xterm.js on the Rust layer's PTY, and the panel stays
// until it is closed, shown or hidden, whatever mail the reader shows.
//
// As the embedded editors (components/compose/TerminalHost.tsx), the pane
// lives under AppShell, which stays mounted whatever the shell shows, laid
// over the slot the Shell renders to the right of the reader; with no slot
// (another view, the narrow layout, the panel hidden) it is hidden and the
// agent keeps running. Closing the panel stops the agent.

import { createContext, lazy, Suspense, useContext, useEffect, useLayoutEffect, useRef, useState, type ComponentType, type ReactNode } from "react";
import { Bot, PanelRightClose, Square, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { agentStarted } from "@/app/noteActions";
import type { AgentPanel } from "@/app/state";
import type { TerminalFontSetting } from "@/lib/terminal-font";
import { useAppState, useDispatch } from "@/app/store";
import { asGuiError, fixtureNotice } from "@/lib/gui-types";
import { agentBridge, type AgentRun, type TerminalBridge, type TerminalExit, type TerminalStarted } from "@/lib/terminal";
import type { TerminalPaneProps } from "@/components/compose/TerminalPane";

// xterm and its addons load with the first agent, not with the app.
const TerminalPane = lazy(() =>
  import("@/components/compose/TerminalPane").then((m) => ({ default: m.TerminalPane as ComponentType<TerminalPaneProps<AgentRun>> })),
);

type SlotContext = { slot: HTMLElement | null; setSlot: (el: HTMLElement | null) => void };

const Slot = createContext<SlotContext | null>(null);

/** Shares the panel's slot between the Shell, which renders it, and the host. */
export function AgentSlotProvider({ children }: { children: ReactNode }) {
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  return <Slot.Provider value={{ slot, setSlot }}>{children}</Slot.Provider>;
}

/** What the panel's header says of its agent. */
export function agentStatusLine(agent: AgentPanel): string {
  const n = agent.ids.length;
  const notes = `${n} ${n === 1 ? "note" : "notes"}`;
  switch (agent.status.kind) {
    case "starting":
      return `Starting on ${notes}…`;
    case "running":
      return `Working on ${notes}`;
    case "exited":
      return agent.status.code === 0 ? `Done with ${notes}` : `Exited (${agent.status.code ?? `signal ${agent.status.signal}`})`;
    case "failed":
      return `Did not start: ${agent.status.error}`;
  }
}

/**
 * The panel to the right of the reader: its header, with Hide and Stop or
 * Close, over the slot the host lays the terminal on.
 */
export function AgentSlot() {
  const s = useAppState();
  const dispatch = useDispatch();
  const ctx = useContext(Slot);
  const agent = s.agent;
  if (!agent) return null;
  const live = agent.status.kind === "starting" || agent.status.kind === "running";
  return (
    <aside aria-label="Agent" data-slot="agent-panel" className="flex min-h-0 w-[min(560px,45%)] shrink-0 flex-col border-l border-border">
      <header className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-1.5 text-sm">
        <Bot aria-hidden="true" className="size-4 shrink-0" />
        <span className="font-medium">Agent</span>
        <span role="status" data-slot="agent-status" className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
          {agentStatusLine(agent)}
        </span>
        <Button size="icon-sm" variant="ghost" aria-label="Hide the agent panel" title="Hide; the agent keeps running" onClick={() => dispatch({ type: "agent_toggle" })}>
          <PanelRightClose aria-hidden="true" />
        </Button>
        <Button
          size="icon-sm"
          variant="ghost"
          aria-label={live ? "Stop the agent and close the panel" : "Close the agent panel"}
          title={live ? "Stop the agent and close" : "Close"}
          onClick={() => dispatch({ type: "agent_close" })}
        >
          {live ? <Square aria-hidden="true" /> : <X aria-hidden="true" />}
        </Button>
      </header>
      <div ref={ctx?.setSlot} data-slot="agent-slot" className="min-h-0 min-w-0 flex-1" />
    </aside>
  );
}

/** A thin tab at the reader's right edge while the panel is hidden, which shows it again. */
export function AgentTab() {
  const s = useAppState();
  const dispatch = useDispatch();
  if (!s.agent || s.agent.shown) return null;
  return (
    <button
      type="button"
      aria-label="Show the agent panel"
      title="Show the agent panel"
      data-slot="agent-tab"
      onClick={() => dispatch({ type: "agent_toggle" })}
      className="flex w-6 shrink-0 cursor-pointer items-start justify-center border-l border-border pt-2 text-muted-foreground hover:bg-accent/60"
    >
      <Bot aria-hidden="true" className="size-4" />
    </button>
  );
}

type Rect = { left: number; top: number; width: number; height: number };

/** The slot's rectangle in the viewport, followed through resizes. */
function useSlotRect(slot: HTMLElement | null): Rect | null {
  const [rect, setRect] = useState<Rect | null>(null);
  const measure = useRef(() => {});
  measure.current = () => {
    const r = slot?.getBoundingClientRect();
    const next = r ? { left: r.left, top: r.top, width: r.width, height: r.height } : null;
    setRect((prev) =>
      prev !== null && next !== null && prev.left === next.left && prev.top === next.top && prev.width === next.width && prev.height === next.height
        ? prev
        : next,
    );
  };
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

/**
 * The agent's one pane, keyed by its spawn. It reports the start, which marks
 * the notes handed off, frees the PTY after the exit frame, and kills the
 * agent when it unmounts while it may still run: a closed panel stops it.
 */
function AgentPane({
  agent,
  visible,
  font,
  bridge,
}: {
  agent: AgentPanel;
  visible: boolean;
  font: TerminalFontSetting;
  bridge: TerminalBridge<AgentRun>;
}) {
  const dispatch = useDispatch();
  const started = useRef<number | null>(null);
  const killed = useRef(false);
  const mounted = useRef(false);
  const spawn = agent.spawn;

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
    if (!mounted.current) return kill();
    dispatch({ type: "agent_status", spawn, status: { kind: "running", session: answer.session, command: answer.editor } });
    const fixture = fixtureNotice(answer);
    if (fixture) dispatch({ type: "notice", text: fixture });
    void agentStarted(agent.ids, dispatch);
  };
  const onExit = (exit: TerminalExit) => {
    kill();
    dispatch({ type: "agent_status", spawn, status: { kind: "exited", code: exit.code, signal: exit.signal } });
  };
  const onError = (e: unknown) => {
    // A refused start leaves the notes open; a refused write or resize changes nothing.
    if (started.current !== null) return;
    const error = asGuiError(e).message;
    dispatch({ type: "agent_status", spawn, status: { kind: "failed", error } });
    dispatch({ type: "notice", text: `The agent did not start: ${error}`, level: "error" });
  };

  return (
    <TerminalPane
      session={{ text: agent.text, date: agent.date }}
      bridge={bridge}
      label="Agent terminal"
      font={font}
      visible={visible}
      onStarted={onStarted}
      onExit={onExit}
      onError={onError}
    />
  );
}

/** The agent's pane over the panel's slot, hidden with no slot and kept running. */
export function AgentHost({ bridge = agentBridge }: { bridge?: TerminalBridge<AgentRun> }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const slot = useContext(Slot)?.slot ?? null;
  const rect = useSlotRect(slot);
  const agent = s.agent;
  const shown = agent !== null && slot !== null;
  return (
    <div
      data-slot="agent-host"
      hidden={!shown}
      className="fixed z-10 flex"
      style={rect ? { left: rect.left, top: rect.top, width: rect.width, height: rect.height } : undefined}
      onFocus={() => dispatch({ type: "pane_focused", pane: "reader" })}
    >
      {agent && agent.status.kind !== "failed" ? (
        <Suspense fallback={null}>
          <AgentPane key={agent.spawn} agent={agent} visible={shown} font={s.terminalFont} bridge={bridge} />
        </Suspense>
      ) : null}
    </div>
  );
}

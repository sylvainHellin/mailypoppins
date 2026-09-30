import { useEffect, useLayoutEffect, type RefObject } from "react";
import type { ListGeometry } from "@/app/actions";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { AppSidebar } from "@/components/sidebar/AppSidebar";
import { MessageListPane } from "@/components/list/MessageListPane";
import { ReaderPane } from "@/components/reader/ReaderPane";
import { NarrowBar } from "@/components/shell/NarrowBar";
import { Splitter } from "@/components/shell/Splitter";
import { StatusRegion } from "@/components/shell/StatusRegion";
import { ActivityStack } from "@/components/mutations/ActivityStack";
import { useAppState, useDispatch } from "@/app/store";
import { listWidthFor, useWidth } from "@/app/layout";
import { LIST_WIDTH_MIN, type Pane } from "@/app/state";

/** Move DOM focus to the focused pane's roving item when the model asks. */
function useFocusFollow(focus: Pane, seq: number): void {
  useEffect(() => {
    if (seq === 0) return;
    const pane = document.querySelector<HTMLElement>(`[data-pane="${focus}"]`);
    if (!pane) return;
    const target =
      pane.querySelector<HTMLElement>('[data-roving="active"]') ??
      pane.querySelector<HTMLElement>('[tabindex="0"], button:not([disabled])');
    if (target && document.activeElement !== target) target.focus({ preventScroll: false });
    target?.scrollIntoView?.({ block: "nearest" });
  }, [focus, seq]);
}

/**
 * The inset shell. Wide: sidebar, list, reader. Medium: the sidebar is an
 * icon rail. Narrow: one of the three at a time, the focused one.
 */
export function Shell({ listGeometry }: { listGeometry?: RefObject<ListGeometry | null> }) {
  const s = useAppState();
  const dispatch = useDispatch();
  useFocusFollow(s.focus, s.focusSeq);
  const [panesRef, paneWidth] = useWidth();
  const list = listWidthFor(s.prefs.listWidth, paneWidth);
  useLayoutEffect(() => {
    if (listGeometry) listGeometry.current = { width: list.width, max: list.max };
  }, [listGeometry, list.width, list.max]);

  const layout = s.layout;
  const narrow = layout === "narrow";
  const sidebarOpen = layout === "wide" && !s.prefs.sidebarCollapsed;
  const showList = narrow ? s.focus === "list" : !(s.zoomed && s.focus === "reader");
  const showReader = narrow ? s.focus === "reader" : !(s.zoomed && s.focus === "list");
  const showSidebar = !narrow || s.focus === "sidebar";

  return (
    <SidebarProvider
      open={sidebarOpen}
      onOpenChange={(open) => layout === "wide" && dispatch({ type: "set_sidebar_open", open })}
      className="h-svh overflow-hidden"
      data-layout={layout}
    >
      {narrow && showSidebar ? (
        <div className="flex min-w-0 flex-1 flex-col bg-sidebar">
          <StatusRegion state={s} />
          <AppSidebar collapsible="none" />
        </div>
      ) : showSidebar ? (
        <AppSidebar collapsible="icon" />
      ) : null}
      {narrow && s.focus === "sidebar" ? null : (
        <SidebarInset className="min-h-0 min-w-0 overflow-hidden">
          <StatusRegion state={s} />
          {narrow ? <NarrowBar view={s.focus} onUp={() => dispatch({ type: "up" })} /> : null}
          <div ref={panesRef} className="flex min-h-0 flex-1" data-panes="">
            {showList ? (
              <div
                className="min-h-0 min-w-0 shrink-0"
                style={narrow || !showReader ? { flex: "1 1 auto" } : { width: list.width }}
              >
                <MessageListPane />
              </div>
            ) : null}
            {showList && showReader && !narrow ? (
              <Splitter
                value={list.width}
                min={LIST_WIDTH_MIN}
                max={list.max}
                onChange={(px) => dispatch({ type: "set_list_width", px: Math.min(px, list.max) })}
                label="Resize the message list"
              />
            ) : null}
            {showReader ? (
              <div className="min-h-0 min-w-0 flex-1">
                <ReaderPane />
              </div>
            ) : null}
          </div>
        </SidebarInset>
      )}
      <ActivityStack />
    </SidebarProvider>
  );
}

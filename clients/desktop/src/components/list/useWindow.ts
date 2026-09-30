// A fixed-row-height windowing hook: past WINDOW_FROM rows only the visible
// slice (plus overscan) is mounted. Below it every row renders.
//
// Off in M1 (WINDOW_FROM is Infinity): the fixtures hold at most 8 rows, and
// the hook loses DOM focus when `G` or `gg` jumps the selection past the
// overscan, since the focused row unmounts before the new one mounts.
// TODO(ticket 0129): keep the roving row mounted across a jump (or move
// focus after revealRow lands), then set a real threshold again.
// (The ticket is written without its `#`, which the colour guard reads as hex.)

import { useEffect, useState, type RefObject } from "react";

export const ROW_HEIGHT = 64;
export const WINDOW_FROM = Number.POSITIVE_INFINITY;
const OVERSCAN = 10;

export type Window = { start: number; end: number; padTop: number; padBottom: number };

export function windowFor(count: number, scrollTop: number, viewport: number): Window {
  if (count < WINDOW_FROM) return { start: 0, end: count, padTop: 0, padBottom: 0 };
  const first = Math.floor(scrollTop / ROW_HEIGHT);
  const visible = Math.ceil(viewport / ROW_HEIGHT);
  const start = Math.max(0, first - OVERSCAN);
  const end = Math.min(count, first + visible + OVERSCAN);
  return { start, end, padTop: start * ROW_HEIGHT, padBottom: (count - end) * ROW_HEIGHT };
}

export function useWindow(ref: RefObject<HTMLElement | null>, count: number): Window {
  const [view, setView] = useState({ top: 0, height: 800 });
  useEffect(() => {
    const el = ref.current;
    if (!el || count < WINDOW_FROM) return;
    const update = () => setView({ top: el.scrollTop, height: el.clientHeight || 800 });
    update();
    el.addEventListener("scroll", update, { passive: true });
    const ro = typeof ResizeObserver === "function" ? new ResizeObserver(update) : null;
    ro?.observe(el);
    return () => {
      el.removeEventListener("scroll", update);
      ro?.disconnect();
    };
  }, [ref, count]);
  return windowFor(count, view.top, view.height);
}

/** Scroll a windowed list so row `index` is mounted and visible. */
export function revealRow(el: HTMLElement, index: number): void {
  const top = index * ROW_HEIGHT;
  if (top < el.scrollTop) el.scrollTop = top;
  else if (top + ROW_HEIGHT > el.scrollTop + el.clientHeight) el.scrollTop = top + ROW_HEIGHT - el.clientHeight;
}

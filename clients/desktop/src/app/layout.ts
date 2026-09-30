// The adaptive layout: wide (sidebar + list + reader), medium (icon rail +
// list + reader), narrow (one view at a time).

import { useEffect, useState, useSyncExternalStore } from "react";
import { LIST_WIDTH_MAX, LIST_WIDTH_MIN, READER_MIN, type Layout } from "@/app/state";

export const WIDE_MIN = 1100;
export const MEDIUM_MIN = 760;

const WIDE_QUERY = `(min-width: ${WIDE_MIN}px)`;
const MEDIUM_QUERY = `(min-width: ${MEDIUM_MIN}px)`;

export function layoutFor(width: number): Layout {
  if (width >= WIDE_MIN) return "wide";
  if (width >= MEDIUM_MIN) return "medium";
  return "narrow";
}

function snapshot(): Layout {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return "wide";
  if (window.matchMedia(WIDE_QUERY).matches) return "wide";
  if (window.matchMedia(MEDIUM_QUERY).matches) return "medium";
  return "narrow";
}

function subscribe(onChange: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return () => {};
  const queries = [window.matchMedia(WIDE_QUERY), window.matchMedia(MEDIUM_QUERY)];
  for (const q of queries) q.addEventListener("change", onChange);
  window.addEventListener("resize", onChange);
  return () => {
    for (const q of queries) q.removeEventListener("change", onChange);
    window.removeEventListener("resize", onChange);
  };
}

export function useLayout(): Layout {
  return useSyncExternalStore(subscribe, snapshot, () => "wide");
}

/**
 * The list width the pane row has room for: the stored one, less whatever
 * would leave the reader under READER_MIN, never under LIST_WIDTH_MIN. A
 * persisted 720 px from a wide window cannot crush the reader in the medium
 * layout, and the preference itself is kept for when the window grows again.
 * An unmeasured row (width 0) does not clamp.
 */
export function listWidthFor(stored: number, paneWidth: number): { width: number; max: number } {
  const room = paneWidth > 0 ? paneWidth - READER_MIN : Number.POSITIVE_INFINITY;
  const max = Math.max(LIST_WIDTH_MIN, Math.min(LIST_WIDTH_MAX, room));
  return { width: Math.max(LIST_WIDTH_MIN, Math.min(stored, max)), max };
}

/**
 * A callback ref and the width a ResizeObserver last reported for its
 * element, 0 until then; a remounted element is observed again.
 */
export function useWidth(): [(el: HTMLElement | null) => void, number] {
  const [el, setEl] = useState<HTMLElement | null>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    if (!el || typeof ResizeObserver !== "function") return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[entries.length - 1]?.contentRect.width;
      if (w !== undefined) setWidth(Math.round(w));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [el]);
  return [setEl, width];
}

import { useRef } from "react";

export type SplitterProps = {
  /** The width of the pane on the splitter's left. */
  value: number;
  min: number;
  max: number;
  onChange: (px: number) => void;
  label: string;
};

/**
 * A pointer-event and keyboard splitter (the ARIA window splitter pattern);
 * written here rather than pulling in a resizable-panels package.
 */
export function Splitter({ value, min, max, onChange, label }: SplitterProps) {
  const drag = useRef<{ x: number; start: number } | null>(null);
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={value}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        drag.current = { x: e.clientX, start: value };
      }}
      onPointerMove={(e) => {
        if (drag.current) onChange(drag.current.start + e.clientX - drag.current.x);
      }}
      onPointerUp={(e) => {
        drag.current = null;
        e.currentTarget.releasePointerCapture(e.pointerId);
      }}
      onKeyDown={(e) => {
        const step = e.shiftKey ? 64 : 16;
        if (e.key === "ArrowLeft") onChange(value - step);
        else if (e.key === "ArrowRight") onChange(value + step);
        else if (e.key === "Home") onChange(min);
        else if (e.key === "End") onChange(max);
        else return;
        e.preventDefault();
        e.stopPropagation();
      }}
      className="group relative w-px shrink-0 cursor-col-resize bg-border outline-none after:absolute after:inset-y-0 after:-left-1.5 after:w-3 hover:bg-framing focus-visible:bg-ring focus-visible:outline-none"
    />
  );
}

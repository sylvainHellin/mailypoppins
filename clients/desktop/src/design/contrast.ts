// WCAG 2.x contrast over the semantic tokens in src/index.css.
// Pure functions, shared by scripts/contrast.ts (which writes
// docs/design-tokens.md) and by the token test, so the documented ratios and
// the enforced ones are computed by one implementation.

export type Rgb = { r: number; g: number; b: number };

/** Parse `#rgb` or `#rrggbb`. */
export function parseHex(hex: string): Rgb {
  const h = hex.trim().replace(/^#/, "");
  const full =
    h.length === 3
      ? h
          .split("")
          .map((c) => c + c)
          .join("")
      : h;
  if (!/^[0-9a-fA-F]{6}$/.test(full)) {
    throw new Error(`not a hex colour: ${hex}`);
  }
  return {
    r: parseInt(full.slice(0, 2), 16),
    g: parseInt(full.slice(2, 4), 16),
    b: parseInt(full.slice(4, 6), 16),
  };
}

function channel(c: number): number {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

export function luminance({ r, g, b }: Rgb): number {
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

export function contrast(a: string, b: string): number {
  const la = luminance(parseHex(a));
  const lb = luminance(parseHex(b));
  const [hi, lo] = la > lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

/** The `--name: #hex;` declarations of the first `:root { … }` block. */
export function rootTokens(css: string): Record<string, string> {
  const start = css.indexOf(":root");
  if (start < 0) return {};
  const open = css.indexOf("{", start);
  const close = css.indexOf("}", open);
  const block = css.slice(open + 1, close);
  const out: Record<string, string> = {};
  for (const m of block.matchAll(/--([a-z0-9-]+)\s*:\s*(#[0-9a-fA-F]{3,6})\s*;/g)) {
    out[m[1]] = m[2];
  }
  return out;
}

/** What a pair has to reach: body text 4.5:1, large text and UI parts 3:1. */
export type Requirement = "text" | "large" | "ui";

export const MINIMUM: Record<Requirement, number> = { text: 4.5, large: 3, ui: 3 };

export type Pair = { fg: string; bg: string; need: Requirement; use: string };

/** Every foreground/background pairing the shell renders. */
export const PAIRS: Pair[] = [
  { fg: "foreground", bg: "background", need: "text", use: "Body text on the inset pane" },
  { fg: "foreground", bg: "sidebar", need: "text", use: "Text on the outer canvas" },
  { fg: "muted-foreground", bg: "background", need: "text", use: "Secondary text (dates, counts)" },
  { fg: "muted-foreground", bg: "sidebar", need: "text", use: "Secondary text on the canvas" },
  { fg: "muted-foreground", bg: "muted", need: "text", use: "Secondary text on a muted surface" },
  { fg: "muted-foreground", bg: "card", need: "text", use: "Secondary text on a card" },
  { fg: "card-foreground", bg: "card", need: "text", use: "Card text" },
  { fg: "popover-foreground", bg: "popover", need: "text", use: "Palette, menus, dialogs" },
  { fg: "muted-foreground", bg: "popover", need: "text", use: "Palette shortcuts and hints" },
  { fg: "primary-foreground", bg: "primary", need: "text", use: "Primary action button (pumpkin)" },
  { fg: "secondary-foreground", bg: "secondary", need: "text", use: "Secondary button" },
  { fg: "accent-foreground", bg: "accent", need: "text", use: "Hovered item" },
  { fg: "selection-foreground", bg: "selection", need: "text", use: "Selected list row (ocean)" },
  { fg: "muted-foreground", bg: "selection", need: "text", use: "Secondary text in a selected row" },
  { fg: "sidebar-foreground", bg: "sidebar", need: "text", use: "Sidebar item" },
  { fg: "sidebar-accent-foreground", bg: "sidebar-accent", need: "text", use: "Active sidebar item" },
  { fg: "sidebar-primary-foreground", bg: "sidebar-primary", need: "text", use: "Sidebar primary mark" },
  { fg: "link", bg: "background", need: "text", use: "Links and ocean text" },
  { fg: "link", bg: "sidebar", need: "text", use: "Ocean text on the canvas" },
  { fg: "warning", bg: "background", need: "text", use: "Attention text (pumpkin)" },
  { fg: "warning", bg: "sidebar", need: "text", use: "Attention text on the canvas" },
  { fg: "destructive", bg: "background", need: "text", use: "Error text" },
  { fg: "destructive", bg: "card", need: "text", use: "Error text on a card" },
  { fg: "ring", bg: "background", need: "ui", use: "Focus ring on the inset pane" },
  { fg: "ring", bg: "sidebar", need: "ui", use: "Focus ring on the canvas" },
  { fg: "ring", bg: "selection", need: "ui", use: "Focus ring around a selected row" },
  { fg: "ring", bg: "popover", need: "ui", use: "Focus ring in a dialog" },
  { fg: "sidebar-ring", bg: "sidebar", need: "ui", use: "Sidebar focus ring" },
  { fg: "input", bg: "background", need: "ui", use: "Input border" },
  // Only the label of a disabled palette entry (CommandPalette.tsx), an
  // inactive control WCAG exempts; the key help and every badge, which inform,
  // are `muted-foreground`. Text that informs does not take this token.
  { fg: "disabled-foreground", bg: "popover", need: "large", use: "Disabled palette entry (inactive control)" },
  { fg: "disabled-foreground", bg: "sidebar", need: "large", use: "Disabled sidebar entry (informational)" },
];

export type Row = Pair & { fgHex: string; bgHex: string; ratio: number; pass: boolean };

export function evaluate(tokens: Record<string, string>): Row[] {
  return PAIRS.map((p) => {
    const fgHex = tokens[p.fg];
    const bgHex = tokens[p.bg];
    if (!fgHex || !bgHex) {
      throw new Error(`token missing for pair ${p.fg} on ${p.bg}`);
    }
    const ratio = contrast(fgHex, bgHex);
    return { ...p, fgHex, bgHex, ratio, pass: ratio >= MINIMUM[p.need] };
  });
}

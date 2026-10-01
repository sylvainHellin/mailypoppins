# Design tokens

The desktop client has a dark and a light palette, and every colour a component uses is a semantic token defined once in `src/index.css`.
The tokens are the shadcn set (`background`, `foreground`, `card`, `popover`, `primary`, `secondary`, `muted`, `accent`, `destructive`, `border`, `input`, `ring`, `sidebar-*`, `chart-*`) plus eight of our own: `selection`, `selection-foreground`, `link`, `framing`, `warning`, `disabled-foreground`, `reader-canvas` and `overlay`.
The sixteen `terminal-*` tokens are the embedded editor's ANSI colours (see Terminal).
Tailwind reaches them as `bg-background`, `text-link`, `ring-ring` and so on.

## Themes

The dark palette is the `:root` block, and the light palette the `:root.light` block, which redeclares every colour but `reader-canvas`.
The theme setting is dark, light or system, dark by default (see [shell.md](shell.md), "Settings").
`src/app/theme.ts` paints a theme by putting the `light` or `dark` class on `<html>` and setting `color-scheme` and its meta tag; system follows `prefers-color-scheme` and its changes.
shadcn's `dark:` variants match under `.dark`, so the light palette draws shadcn's light defaults.

## Dark palette

It derives from the dark inverse of Basenord Palette D:

| Role | Value | Where it goes |
|---|---|---|
| Prussian blue | `#0C1B33` | `sidebar`, the outer canvas of the inset shell; `background` is the same hue lifted to `#0F213D` so the inset pane reads as a surface |
| Cream | `#F4F1E8` | every `*-foreground` that carries body text |
| Ocean blue | `#2E86AB` | `framing` as is; `ring` (`#4BA3CC`) and `link` (`#6DB6DA`) are lighter variants that clear 3:1 and 4.5:1; `selection` (`#174A68`) is a darkened variant under cream text |
| Pumpkin | `#FF6700` | `primary` (primary actions only), `warning` (attention and deltas), `chart-1` |

`destructive` (`#FF7A85`) is a warm red kept apart from pumpkin, so an error never reads as a primary action.

## Light palette

It derives from Basenord Palette D itself:

| Role | Value | Where it goes |
|---|---|---|
| Cream | `#F4F1E8` | `sidebar`, the outer canvas; `background` (`#FBFAF6`) is cream lifted, `card` and `popover` are white, and `muted` (`#ECE8DD`), `secondary` (`#E6E1D3`) and `accent` (`#E4DFD0`) sit a shade under cream |
| Prussian blue | `#0C1B33` | every `*-foreground` that carries body text; its tints are `muted-foreground` (`#46546C`), `disabled-foreground` (`#77839A`), `input` (`#7D889B`) and `border` (`#D3D6DC`) |
| Ocean blue | `#2E86AB` | `framing` as is; `ring` (`#1F6F93`) and `link` (`#1A6385`) are darker shades that clear 3:1 and 4.5:1 on cream and white; `selection` (`#CFE3EE`) is a pale tint under Prussian text |
| Pumpkin | `#FF6700` | `primary` under Prussian text (5.89:1), `chart-1`; `warning` is the darker `#B34700`, since `#FF6700` reaches only 2.6:1 on cream |

`destructive` (`#B4232E`) is a deep red, 6.2:1 on the inset pane.

## Shared tokens

| Token | Value | Where it goes |
|---|---|---|
| `reader-canvas` | `#FFFFFF` | the reader frame's page in both palettes, since mail is authored for white and a message that sets no background would draw black text on Prussian |
| `overlay` | `#0C1B3399` dark, `#0C1B3366` light | the scrim behind a dialog or a sheet: Prussian at 60% alpha over the dark palette, 40% over the light one |

## Terminal

The embedded editor's xterm.js theme (`src/components/compose/terminalTheme.ts`) reads the tokens from `<html>` at mount and again whenever `src/app/theme.ts` changes its class, so it follows the palette.
Its surfaces are the shell's tokens:

| xterm slot | Token |
|---|---|
| `background`, `cursorAccent` | `background` |
| `foreground`, `cursor` | `foreground` |
| `selectionBackground`, `selectionForeground` | `selection`, `selection-foreground` |
| `selectionInactiveBackground` | `muted` |
| scrollbar slider, hover and active | `border`, `input`, `input` |

The sixteen ANSI colours are tokens of their own, `--terminal-<name>` and `--terminal-bright-<name>`, declared in both palette blocks.
Where a palette token fits, the ANSI colour is that token; the other hues (green, yellow, magenta, cyan and the bright reds, greens, magentas and cyans) are new values, lifted to read on the dark inset pane and darkened to read on cream.

| ANSI | Dark | Light |
|---|---|---|
| black | `#25406A` (`border`) | `#0C1B33` (`foreground`) |
| red | `#FF7A85` (`destructive`) | `#B4232E` (`destructive`) |
| green | `#8FD19E` | `#2F7A3E` |
| yellow | `#F2C14E` | `#8A6100` |
| blue | `#4BA3CC` (`ring`) | `#1A6385` (`link`) |
| magenta | `#D7A2E8` | `#8A3FA0` |
| cyan | `#5CC8C8` | `#1E7A7A` |
| white | `#B4BFCE` (`muted-foreground`) | `#E6E1D3` (`secondary`) |
| bright black | `#7D8BA3` (`disabled-foreground`) | `#77839A` (`disabled-foreground`) |
| bright red | `#FF9AA2` | `#D6404B` |
| bright green | `#B0E3BB` | `#3E9450` |
| bright yellow | `#FF6700` (`warning`, Pumpkin) | `#B34700` (`warning`) |
| bright blue | `#6DB6DA` (`link`) | `#1F6F93` (`ring`) |
| bright magenta | `#E6C3F0` | `#A35BB8` |
| bright cyan | `#8EDCDC` | `#2A9494` |
| bright white | `#F4F1E8` (`foreground`, Cream) | `#FFFFFF` (`card`) |

Black and white sit close to the background in each palette, as terminal themes place them, so the contrast pass does not check the ANSI colours; a Neovim colour scheme with `termguicolors` draws in its own truecolour anyway.

That is why the embedded Neovim or Vim gets a colour scheme of the app's own, `src-tauri/resources/nvim/colors/mailypoppins.vim` (ticket 0137; [rust-layer.md](rust-layer.md), "Terminal sessions", "The look").
It reads the same tokens: `background`, `foreground`, `muted`, `accent`, `primary`, `border`, `destructive`, `selection` and the sixteen `terminal-*` colours, copied as hex values into one dictionary per palette, picked by Vim's `background`.
Each group sets the hex value for `termguicolors` and an ANSI index from 0 to 15 for a terminal without it: a `terminal-*` colour takes its own slot, a surface the nearest slot of its palette (`muted`, `accent`, `border` and `selection` take black in dark and white in light), and Normal takes `NONE`, the pane's own `background` and `foreground`, so the editor's background is the app's in both modes.
`src/design/colorscheme.test.ts` fails when a hex value there differs from `index.css` or sits outside the two dictionaries, and when a `terminal-*` colour takes another slot's index; a token change is copied there by hand.
The font is the host element's computed `font-mono` at `text-sm`: `@theme inline` emits no `--font-mono` variable, so the class is the only place the stack resolves.

## Rules

- No component hardcodes a palette value: `src/app/colour-guard.test.ts` fails on a hex colour, a functional colour, an arbitrary `bg-[#…]` class or a Tailwind palette class (`bg-black/10`, `text-slate-400`) anywhere under `src/components`, shadcn's `src/components/ui` included, or `src/app`.
- Text pairs reach 4.5:1, focus rings and input borders 3:1, and disabled entries 3:1 although WCAG exempts inactive controls.
- `disabled-foreground` is only the label of an inactive control; text that informs, such as the key help's not-yet-available actions and the milestone badges, is `muted-foreground`.
- Every focusable shows a solid 2 px `ring` outline on `:focus-visible` (the base layer of `src/index.css`), whatever the component's own ring.
- Icons are Lucide outline at stroke width 1.75, set once by `LucideProvider` in `src/main.tsx`.
- Type is Inter when installed, else the system UI stack; no webfont ships (no Inter file exists on the build machine and adding the package needs approval).
- `prefers-reduced-motion` collapses every animation and transition.

## Row and notice patterns

- The cursor row is `selection`; a marked row is `accent`, the hover colour at full strength, with its mark box filled in `link`.
- The highlighted row of a cmdk list (the palette, the key help, the recipient completion) is `selection` as well, so the row the next Enter takes reads like the list's cursor row.
- A change the daemon has not confirmed draws a spinning Lucide `LoaderCircle` in `muted-foreground` among the row's icons; it has no colour of its own.
- The flag toggle is `warning` when set, and the unread dot `link`.
- A notice is a `popover` surface: an applied one carries a `link` check, a failure a `destructive` alert icon.
- The which-key popup is a `popover` surface with a `ring` border, the TUI's accent frame; its title and keys are `link`.
- A held send carries a `warning` icon and a `warning` countdown bar on a `muted` track.

## Contrast

The tables, one per palette, are generated by `pnpm contrast` (`scripts/contrast.ts` over `src/design/contrast.ts`), which fails on any pair under its minimum; `src/design/contrast.test.ts` enforces the same minimums in `pnpm test`, and fails when the light block leaves a colour of the dark one undeclared.

<!-- contrast:begin (generated by `pnpm contrast`, do not edit) -->
### Dark

| Foreground | Background | Hex | Ratio | Minimum | Pass | Used for |
|---|---|---|---|---|---|---|
| `foreground` | `background` | `#F4F1E8` on `#0F213D` | 14.25:1 | 4.5:1 (text) | yes | Body text on the inset pane |
| `foreground` | `sidebar` | `#F4F1E8` on `#0C1B33` | 15.24:1 | 4.5:1 (text) | yes | Text on the outer canvas |
| `muted-foreground` | `background` | `#B4BFCE` on `#0F213D` | 8.65:1 | 4.5:1 (text) | yes | Secondary text (dates, counts) |
| `muted-foreground` | `sidebar` | `#B4BFCE` on `#0C1B33` | 9.25:1 | 4.5:1 (text) | yes | Secondary text on the canvas |
| `muted-foreground` | `muted` | `#B4BFCE` on `#1A3457` | 6.75:1 | 4.5:1 (text) | yes | Secondary text on a muted surface |
| `muted-foreground` | `card` | `#B4BFCE` on `#142A4A` | 7.73:1 | 4.5:1 (text) | yes | Secondary text on a card |
| `card-foreground` | `card` | `#F4F1E8` on `#142A4A` | 12.74:1 | 4.5:1 (text) | yes | Card text |
| `popover-foreground` | `popover` | `#F4F1E8` on `#172F52` | 11.88:1 | 4.5:1 (text) | yes | Palette, menus, dialogs |
| `muted-foreground` | `popover` | `#B4BFCE` on `#172F52` | 7.21:1 | 4.5:1 (text) | yes | Palette shortcuts and hints |
| `primary-foreground` | `primary` | `#0C1B33` on `#FF6700` | 5.89:1 | 4.5:1 (text) | yes | Primary action button (pumpkin) |
| `secondary-foreground` | `secondary` | `#F4F1E8` on `#1D3A60` | 10.18:1 | 4.5:1 (text) | yes | Secondary button |
| `accent-foreground` | `accent` | `#F4F1E8` on `#1F3D66` | 9.70:1 | 4.5:1 (text) | yes | Hovered item, marked list row |
| `selection-foreground` | `selection` | `#F4F1E8` on `#174A68` | 8.39:1 | 4.5:1 (text) | yes | Selected list row (ocean) |
| `muted-foreground` | `selection` | `#B4BFCE` on `#174A68` | 5.09:1 | 4.5:1 (text) | yes | Secondary text in a selected row |
| `muted-foreground` | `accent` | `#B4BFCE` on `#1F3D66` | 5.88:1 | 4.5:1 (text) | yes | Secondary text in a marked row |
| `background` | `link` | `#0F213D` on `#6DB6DA` | 7.17:1 | 3:1 (ui) | yes | Check in a mark box |
| `warning` | `popover` | `#FF6700` on `#172F52` | 4.60:1 | 4.5:1 (text) | yes | Held send icon and countdown bar in a toast |
| `destructive` | `popover` | `#FF7A85` on `#172F52` | 5.35:1 | 4.5:1 (text) | yes | Failure icon in a toast |
| `sidebar-foreground` | `sidebar` | `#F4F1E8` on `#0C1B33` | 15.24:1 | 4.5:1 (text) | yes | Sidebar item |
| `sidebar-accent-foreground` | `sidebar-accent` | `#F4F1E8` on `#174A68` | 8.39:1 | 4.5:1 (text) | yes | Active sidebar item |
| `sidebar-primary-foreground` | `sidebar-primary` | `#0C1B33` on `#4BA3CC` | 6.07:1 | 4.5:1 (text) | yes | Sidebar primary mark |
| `link` | `background` | `#6DB6DA` on `#0F213D` | 7.17:1 | 4.5:1 (text) | yes | Links and ocean text |
| `link` | `sidebar` | `#6DB6DA` on `#0C1B33` | 7.67:1 | 4.5:1 (text) | yes | Ocean text on the canvas |
| `warning` | `background` | `#FF6700` on `#0F213D` | 5.51:1 | 4.5:1 (text) | yes | Attention text (pumpkin) |
| `warning` | `sidebar` | `#FF6700` on `#0C1B33` | 5.89:1 | 4.5:1 (text) | yes | Attention text on the canvas |
| `destructive` | `background` | `#FF7A85` on `#0F213D` | 6.42:1 | 4.5:1 (text) | yes | Error text |
| `destructive` | `card` | `#FF7A85` on `#142A4A` | 5.74:1 | 4.5:1 (text) | yes | Error text on a card |
| `ring` | `background` | `#4BA3CC` on `#0F213D` | 5.68:1 | 3:1 (ui) | yes | Focus ring on the inset pane |
| `ring` | `sidebar` | `#4BA3CC` on `#0C1B33` | 6.07:1 | 3:1 (ui) | yes | Focus ring on the canvas |
| `ring` | `selection` | `#4BA3CC` on `#174A68` | 3.34:1 | 3:1 (ui) | yes | Focus ring around a selected row |
| `ring` | `popover` | `#4BA3CC` on `#172F52` | 4.74:1 | 3:1 (ui) | yes | Focus ring in a dialog |
| `sidebar-ring` | `sidebar` | `#4BA3CC` on `#0C1B33` | 6.07:1 | 3:1 (ui) | yes | Sidebar focus ring |
| `input` | `background` | `#5075A8` on `#0F213D` | 3.41:1 | 3:1 (ui) | yes | Input border |
| `disabled-foreground` | `popover` | `#7D8BA3` on `#172F52` | 3.89:1 | 3:1 (large) | yes | Disabled palette entry (inactive control) |
| `disabled-foreground` | `sidebar` | `#7D8BA3` on `#0C1B33` | 4.99:1 | 3:1 (large) | yes | Disabled sidebar entry (informational) |

### Light

| Foreground | Background | Hex | Ratio | Minimum | Pass | Used for |
|---|---|---|---|---|---|---|
| `foreground` | `background` | `#0C1B33` on `#FBFAF6` | 16.48:1 | 4.5:1 (text) | yes | Body text on the inset pane |
| `foreground` | `sidebar` | `#0C1B33` on `#F4F1E8` | 15.24:1 | 4.5:1 (text) | yes | Text on the outer canvas |
| `muted-foreground` | `background` | `#46546C` on `#FBFAF6` | 7.32:1 | 4.5:1 (text) | yes | Secondary text (dates, counts) |
| `muted-foreground` | `sidebar` | `#46546C` on `#F4F1E8` | 6.77:1 | 4.5:1 (text) | yes | Secondary text on the canvas |
| `muted-foreground` | `muted` | `#46546C` on `#ECE8DD` | 6.25:1 | 4.5:1 (text) | yes | Secondary text on a muted surface |
| `muted-foreground` | `card` | `#46546C` on `#FFFFFF` | 7.65:1 | 4.5:1 (text) | yes | Secondary text on a card |
| `card-foreground` | `card` | `#0C1B33` on `#FFFFFF` | 17.21:1 | 4.5:1 (text) | yes | Card text |
| `popover-foreground` | `popover` | `#0C1B33` on `#FFFFFF` | 17.21:1 | 4.5:1 (text) | yes | Palette, menus, dialogs |
| `muted-foreground` | `popover` | `#46546C` on `#FFFFFF` | 7.65:1 | 4.5:1 (text) | yes | Palette shortcuts and hints |
| `primary-foreground` | `primary` | `#0C1B33` on `#FF6700` | 5.89:1 | 4.5:1 (text) | yes | Primary action button (pumpkin) |
| `secondary-foreground` | `secondary` | `#0C1B33` on `#E6E1D3` | 13.17:1 | 4.5:1 (text) | yes | Secondary button |
| `accent-foreground` | `accent` | `#0C1B33` on `#E4DFD0` | 12.92:1 | 4.5:1 (text) | yes | Hovered item, marked list row |
| `selection-foreground` | `selection` | `#0C1B33` on `#CFE3EE` | 13.01:1 | 4.5:1 (text) | yes | Selected list row (ocean) |
| `muted-foreground` | `selection` | `#46546C` on `#CFE3EE` | 5.78:1 | 4.5:1 (text) | yes | Secondary text in a selected row |
| `muted-foreground` | `accent` | `#46546C` on `#E4DFD0` | 5.74:1 | 4.5:1 (text) | yes | Secondary text in a marked row |
| `background` | `link` | `#FBFAF6` on `#1A6385` | 6.35:1 | 3:1 (ui) | yes | Check in a mark box |
| `warning` | `popover` | `#B34700` on `#FFFFFF` | 5.50:1 | 4.5:1 (text) | yes | Held send icon and countdown bar in a toast |
| `destructive` | `popover` | `#B4232E` on `#FFFFFF` | 6.52:1 | 4.5:1 (text) | yes | Failure icon in a toast |
| `sidebar-foreground` | `sidebar` | `#0C1B33` on `#F4F1E8` | 15.24:1 | 4.5:1 (text) | yes | Sidebar item |
| `sidebar-accent-foreground` | `sidebar-accent` | `#0C1B33` on `#CFE3EE` | 13.01:1 | 4.5:1 (text) | yes | Active sidebar item |
| `sidebar-primary-foreground` | `sidebar-primary` | `#F4F1E8` on `#1F6F93` | 4.95:1 | 4.5:1 (text) | yes | Sidebar primary mark |
| `link` | `background` | `#1A6385` on `#FBFAF6` | 6.35:1 | 4.5:1 (text) | yes | Links and ocean text |
| `link` | `sidebar` | `#1A6385` on `#F4F1E8` | 5.87:1 | 4.5:1 (text) | yes | Ocean text on the canvas |
| `warning` | `background` | `#B34700` on `#FBFAF6` | 5.27:1 | 4.5:1 (text) | yes | Attention text (pumpkin) |
| `warning` | `sidebar` | `#B34700` on `#F4F1E8` | 4.87:1 | 4.5:1 (text) | yes | Attention text on the canvas |
| `destructive` | `background` | `#B4232E` on `#FBFAF6` | 6.24:1 | 4.5:1 (text) | yes | Error text |
| `destructive` | `card` | `#B4232E` on `#FFFFFF` | 6.52:1 | 4.5:1 (text) | yes | Error text on a card |
| `ring` | `background` | `#1F6F93` on `#FBFAF6` | 5.36:1 | 3:1 (ui) | yes | Focus ring on the inset pane |
| `ring` | `sidebar` | `#1F6F93` on `#F4F1E8` | 4.95:1 | 3:1 (ui) | yes | Focus ring on the canvas |
| `ring` | `selection` | `#1F6F93` on `#CFE3EE` | 4.23:1 | 3:1 (ui) | yes | Focus ring around a selected row |
| `ring` | `popover` | `#1F6F93` on `#FFFFFF` | 5.59:1 | 3:1 (ui) | yes | Focus ring in a dialog |
| `sidebar-ring` | `sidebar` | `#1F6F93` on `#F4F1E8` | 4.95:1 | 3:1 (ui) | yes | Sidebar focus ring |
| `input` | `background` | `#7D889B` on `#FBFAF6` | 3.43:1 | 3:1 (ui) | yes | Input border |
| `disabled-foreground` | `popover` | `#77839A` on `#FFFFFF` | 3.82:1 | 3:1 (large) | yes | Disabled palette entry (inactive control) |
| `disabled-foreground` | `sidebar` | `#77839A` on `#F4F1E8` | 3.38:1 | 3:1 (large) | yes | Disabled sidebar entry (informational) |
<!-- contrast:end -->

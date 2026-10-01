# Logo

The mark is an umbrella rising from an envelope, its hook below, drawn as a single cream monoline on an ink tile with an orange ferrule.

| File | Use |
|---|---|
| `tile.svg` | the mark on its ink tile, the canonical form; the website's `logo.svg` |
| `tile-cream.svg` | the same on a cream tile, for light surfaces that want the tile |
| `mark-ink.svg`, `mark-cream.svg` | the mark alone on a transparent ground, for light and dark surfaces |
| `favicon.svg` | the tile with heavier strokes so it reads at 16 and 32 px; the website's `favicon.svg` |
| `app-icon.svg` | the tile inset to 832 px on a transparent 1024 px canvas, the source of `clients/desktop/src-tauri/icons/` through `pnpm tauri icon` |
| `wordmark-ink.svg`, `wordmark-cream.svg` | the mark with the name set in Inter 600 |

Colours: ink `#0C1B33`, cream `#F4F1E8`, orange `#FF6700`.
The glyph is on a 1024 grid, stroke 18 (34 in the favicon), round caps and joins.
The website's nav redraws the mark inline in `currentColor` at stroke 40.

Regenerate the app icons after a change: `rsvg-convert -w 1024 assets/logo/app-icon.svg -o /tmp/app-icon.png && cd clients/desktop && pnpm tauri icon /tmp/app-icon.png`, then drop the `android/` and `ios/` folders it adds.

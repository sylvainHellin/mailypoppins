// What the embedded terminal's font setting may hold (src/app/terminalFont.ts
// stores it): the rules of settings.rs's `family_is_safe` and
// `size_is_valid`, in their words, and what a stored value reads as. Pure, so
// the fixture backend applies the same rules without reaching the commands.

export type TerminalFontSetting = {
  /** One font family name, unquoted; null draws in the app's mono stack alone. */
  family: string | null;
  /** CSS pixels; null takes the host's computed `text-sm`. */
  size: number | null;
};

/** What an unset or unreadable setting means. */
export const DEFAULT_TERMINAL_FONT: TerminalFontSetting = { family: null, size: null };

/** The longest family taken, in characters (settings.rs `TERMINAL_FONT_FAMILY_MAX`). */
export const TERMINAL_FONT_FAMILY_MAX = 100;

/** The smallest and largest size taken, in CSS pixels (settings.rs `TERMINAL_FONT_SIZE_RANGE`). */
export const TERMINAL_FONT_SIZE_MIN = 8;
export const TERMINAL_FONT_SIZE_MAX = 32;

/**
 * Letters and digits of any script, spaces, `-`, `_`, `.` and `+`: nothing
 * that closes the double quotes the family goes in, or adds a family to the
 * stack. `\p{Alphabetic}` and `\p{N}` are Rust's `char::is_alphanumeric`.
 */
const SAFE_FAMILY = /^[\p{Alphabetic}\p{N} ._+-]+$/u;

/** A whole number without a leading zero and at most one decimal. */
const SIZE_SHAPE = /^[1-9]\d?(\.\d)?$/;

/** Why a family is refused, in settings.rs's words; null when it is taken or blank. */
export function familyRefusal(value: string): string | null {
  const family = value.trim();
  if (family === "") return null;
  if ([...family].length <= TERMINAL_FONT_FAMILY_MAX && SAFE_FAMILY.test(family)) return null;
  return `the terminal font \`${family}\` is not one family name of up to ${TERMINAL_FONT_FAMILY_MAX} letters, digits, spaces, hyphens, underscores, periods and plus signs`;
}

/** Why a size is refused, in settings.rs's words; null when it is taken or blank. */
export function sizeRefusal(value: string): string | null {
  const size = value.trim();
  if (size === "") return null;
  const n = Number(size);
  if (SIZE_SHAPE.test(size) && n >= TERMINAL_FONT_SIZE_MIN && n <= TERMINAL_FONT_SIZE_MAX) return null;
  return `the terminal font size \`${size}\` is not a number of pixels from ${TERMINAL_FONT_SIZE_MIN} to ${TERMINAL_FONT_SIZE_MAX}, with one decimal at most`;
}

/** A stored family, trimmed; null when blank or refused. */
export function parseFontFamily(value: string | null | undefined): string | null {
  const family = value?.trim() ?? "";
  return family !== "" && familyRefusal(family) === null ? family : null;
}

/** A stored size in pixels; null when blank or refused. */
export function parseFontSize(value: string | null | undefined): number | null {
  const size = value?.trim() ?? "";
  return size !== "" && sizeRefusal(size) === null ? Number(size) : null;
}

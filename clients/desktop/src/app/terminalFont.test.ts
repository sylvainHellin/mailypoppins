// The embedded terminal's font setting (src/app/terminalFont.ts): what a
// family and a size may hold, what a stored value reads as, and the font
// stack terminalTheme.ts builds from it.

import { afterEach, describe, expect, it } from "vitest";
import { loadTerminalFont } from "@/app/terminalFont";
import {
  DEFAULT_TERMINAL_FONT,
  familyRefusal,
  parseFontFamily,
  parseFontSize,
  sizeRefusal,
  TERMINAL_FONT_FAMILY_MAX,
} from "@/lib/terminal-font";
import { terminalFont } from "@/components/compose/terminalTheme";
import * as cmd from "@/lib/commands";
import { mock, resetMock } from "@/test/tauri-mock";

const SYMBOLS = '"Symbols Nerd Font Mono"';

/** A host with the computed font the pane's `font-mono text-sm` classes would give it. */
function host(family: string, size: string): HTMLElement {
  const el = document.createElement("div");
  el.style.fontFamily = family;
  el.style.fontSize = size;
  document.body.append(el);
  return el;
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("the terminal font family", () => {
  it("takes letters and digits of any script, spaces, hyphens, underscores, periods and plus signs", () => {
    for (const family of ["JetBrainsMono Nerd Font Mono", "M+ 1m", "Source_Code-Pro.v2", "ヒラギノ角ゴシック"]) {
      expect(familyRefusal(family)).toBeNull();
      expect(parseFontFamily(` ${family} `)).toBe(family);
    }
  });

  it("refuses anything that could close the quoted name or add a family to the list", () => {
    const long = "a".repeat(TERMINAL_FONT_FAMILY_MAX + 1);
    for (const bad of ['Fira Code", serif', "Fira Code, monospace", "Fira 'Code'", "Fira\\Code", "Fira; color: red", "Fira { }", "Fira\nCode", "Fira (Code)", long]) {
      expect(familyRefusal(bad)).toMatch(/letters, digits, spaces/);
      expect(parseFontFamily(bad)).toBeNull();
    }
  });

  it("reads blank, null and a refused value as unset", () => {
    expect(parseFontFamily(null)).toBeNull();
    expect(parseFontFamily("   ")).toBeNull();
    expect(familyRefusal("   ")).toBeNull();
  });
});

describe("the terminal font size", () => {
  it("takes 8 to 32 pixels, with one decimal at most", () => {
    for (const [value, size] of [["16", 16], [" 8 ", 8], ["32", 32], ["13.5", 13.5]] as const) {
      expect(sizeRefusal(value)).toBeNull();
      expect(parseFontSize(value)).toBe(size);
    }
  });

  it("refuses anything else", () => {
    for (const bad of ["7", "32.5", "33", "0", "-12", "+12", "1e1", "inf", "NaN", "12px", "12.25", ".5", "12.", "016"]) {
      expect(sizeRefusal(bad)).toMatch(/from 8 to 32/);
      expect(parseFontSize(bad)).toBeNull();
    }
    expect(parseFontSize(null)).toBeNull();
    expect(sizeRefusal("")).toBeNull();
  });
});

describe("the terminal's font stack", () => {
  it("is the host's mono stack at the host's size, then the symbols, when nothing is set", () => {
    const el = host("Menlo, monospace", "15px");
    const computed = getComputedStyle(el).fontFamily;
    const unset = { fontFamily: `${computed}, ${SYMBOLS}`, fontSize: 15 };
    expect(terminalFont(el)).toEqual(unset);
    expect(terminalFont(el, DEFAULT_TERMINAL_FONT)).toEqual(unset);
  });

  it("puts a chosen family first, quoted, then the host's stack, then the symbols last", () => {
    const el = host("Menlo, monospace", "15px");
    const computed = getComputedStyle(el).fontFamily;
    expect(terminalFont(el, { family: "JetBrainsMono Nerd Font Mono", size: null })).toEqual({
      fontFamily: `"JetBrainsMono Nerd Font Mono", ${computed}, ${SYMBOLS}`,
      fontSize: 15,
    });
  });

  it("takes a chosen size over the host's", () => {
    const el = host("Menlo, monospace", "15px");
    expect(terminalFont(el, { family: null, size: 16 }).fontSize).toBe(16);
    expect(terminalFont(el, { family: "Iosevka Term", size: 13.5 })).toMatchObject({ fontSize: 13.5 });
  });
});

describe("the stored setting", () => {
  it("round-trips through desktop.json's two keys, which refuse what the rules refuse", async () => {
    resetMock();
    expect(await loadTerminalFont()).toEqual(DEFAULT_TERMINAL_FONT);
    expect(await cmd.settingSet("terminal_font_family", " JetBrainsMono Nerd Font Mono ")).toBe("JetBrainsMono Nerd Font Mono");
    expect(await cmd.settingSet("terminal_font_size", "16")).toBe("16");
    await expect(cmd.settingSet("terminal_font_family", 'x", serif')).rejects.toMatchObject({ kind: "setup" });
    await expect(cmd.settingSet("terminal_font_size", "64")).rejects.toMatchObject({ kind: "setup" });
    expect(await loadTerminalFont()).toEqual({ family: "JetBrainsMono Nerd Font Mono", size: 16 });
  });

  it("reads a value the rules refuse, or a file that does not read, as unset", async () => {
    resetMock();
    mock.settings.set("terminal_font_family", "a, b");
    mock.settings.set("terminal_font_size", "99");
    expect(await loadTerminalFont()).toEqual(DEFAULT_TERMINAL_FONT);
    mock.failing.set("setting_get", { kind: "setup", message: "desktop.json is not a JSON object" });
    expect(await loadTerminalFont()).toEqual(DEFAULT_TERMINAL_FONT);
  });
});

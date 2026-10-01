import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { applyTheme, parseTheme, systemScheme } from "@/app/theme";
import { GUI_ENTRIES } from "@/keymap/catalog";
import * as cmd from "@/lib/commands";
import { renderApp, shellReady } from "@/test/render";
import { mock, resetMock } from "@/test/tauri-mock";

/** A `prefers-color-scheme: dark` query the test flips, with the listeners it holds. */
function systemQuery(dark: boolean) {
  const listeners = new Set<(e: MediaQueryListEvent) => void>();
  const query = {
    matches: dark,
    media: "(prefers-color-scheme: dark)",
    addEventListener: (_: string, f: (e: MediaQueryListEvent) => void) => listeners.add(f),
    removeEventListener: (_: string, f: (e: MediaQueryListEvent) => void) => listeners.delete(f),
  };
  const win = {
    document,
    matchMedia: (q: string) => (q === query.media ? query : { matches: false, addEventListener() {}, removeEventListener() {} }),
  } as unknown as Window;
  const flip = (to: boolean) => {
    query.matches = to;
    for (const f of [...listeners]) f({ matches: to } as MediaQueryListEvent);
  };
  return { win, listeners, flip };
}

const root = () => document.documentElement;
const meta = () => document.querySelector<HTMLMetaElement>('meta[name="color-scheme"]');

afterEach(() => {
  applyTheme("dark");
});

describe("applyTheme", () => {
  it("puts the light or dark class, color-scheme and its meta on the document", () => {
    const tag = document.createElement("meta");
    tag.name = "color-scheme";
    tag.content = "dark";
    document.head.append(tag);
    expect(applyTheme("light")).toBe("light");
    expect(root().classList.contains("light")).toBe(true);
    expect(root().classList.contains("dark")).toBe(false);
    expect(root().style.colorScheme).toBe("light");
    expect(meta()?.content).toBe("light");
    expect(applyTheme("dark")).toBe("dark");
    expect(root().classList.contains("dark")).toBe(true);
    expect(root().classList.contains("light")).toBe(false);
    expect(root().style.colorScheme).toBe("dark");
    expect(meta()?.content).toBe("dark");
    tag.remove();
  });

  it("system paints the system's scheme and follows its changes until another theme is applied", () => {
    const { win, listeners, flip } = systemQuery(false);
    expect(applyTheme("system", win)).toBe("light");
    expect(root().classList.contains("light")).toBe(true);
    expect(listeners.size).toBe(1);
    flip(true);
    expect(root().classList.contains("dark")).toBe(true);
    expect(root().style.colorScheme).toBe("dark");
    flip(false);
    expect(root().classList.contains("light")).toBe(true);
    // A chosen theme stops the following.
    applyTheme("dark", win);
    expect(listeners.size).toBe(0);
    flip(false);
    expect(root().classList.contains("dark")).toBe(true);
  });

  it("system twice keeps one listener", () => {
    const { win, listeners } = systemQuery(true);
    expect(applyTheme("system", win)).toBe("dark");
    applyTheme("system", win);
    expect(listeners.size).toBe(1);
  });

  it("system is dark where the webview has no matchMedia", () => {
    const win = { document } as unknown as Window;
    expect(systemScheme(win)).toBe("dark");
    expect(applyTheme("system", win)).toBe("dark");
  });

  it("an unset or unknown stored value is dark", () => {
    expect(parseTheme(null)).toBe("dark");
    expect(parseTheme("sepia")).toBe("dark");
    expect(parseTheme("light")).toBe("light");
    expect(parseTheme("system")).toBe("system");
  });
});

describe("the theme setting", () => {
  it("setting_get and setting_set read and write desktop.json's known keys", async () => {
    resetMock();
    expect(await cmd.settingGet("theme")).toBeNull();
    expect(await cmd.settingSet("theme", " light ")).toBe("light");
    expect(await cmd.settingGet("theme")).toBe("light");
    expect(await cmd.settingSet("theme", null)).toBeNull();
    await expect(cmd.settingGet("colour" as never)).rejects.toMatchObject({ kind: "not_found" });
    await expect(cmd.settingSet("theme", "sepia")).rejects.toMatchObject({ kind: "setup" });
  });

  it("the stored theme is painted at startup and kept in the store", async () => {
    renderApp(1400, () => mock.settings.set("theme", "light"));
    await shellReady();
    await waitFor(() => expect(root().classList.contains("light")).toBe(true));
    expect(mock.calls.filter((c) => c.cmd === "setting_get").map((c) => c.args)).toContainEqual({ key: "theme" });
  });

  it("Settings' Theme buttons paint at once and store the choice", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(screen.getByRole("button", { name: /^Settings$/ }));
    const view = await screen.findByRole("region", { name: "Settings" });
    const group = await within(view).findByRole("group", { name: "Theme" });
    expect(within(group).getByRole("button", { name: "Dark" })).toHaveAttribute("aria-pressed", "true");
    await user.click(within(group).getByRole("button", { name: "Light" }));
    expect(root().classList.contains("light")).toBe(true);
    expect(within(group).getByRole("button", { name: "Light" })).toHaveAttribute("aria-pressed", "true");
    expect(within(group).getByRole("button", { name: "Dark" })).toHaveAttribute("aria-pressed", "false");
    await waitFor(() => expect(mock.settings.get("theme")).toBe("light"));
    await user.click(within(group).getByRole("button", { name: "System" }));
    await waitFor(() => expect(mock.settings.get("theme")).toBe("system"));
  });

  it("a refused write says so on the notice line and keeps the painted theme", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.failing.set("setting_set", { kind: "internal", message: "could not write desktop.json" });
    await user.keyboard(":");
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    const row = within(dialog).getByText("Theme: light").closest("[data-testid='palette-item']");
    await user.click(row as HTMLElement);
    expect(root().classList.contains("light")).toBe(true);
    expect(await screen.findByText("The theme was not saved: could not write desktop.json")).toBeInTheDocument();
  });

  it("the palette has Theme: dark, light and system, with no key", async () => {
    const rows = GUI_ENTRIES.filter((e) => e.label.startsWith("Theme: "));
    expect(rows.map((e) => [e.label, e.id, e.keys])).toEqual([
      ["Theme: dark", "theme_dark", []],
      ["Theme: light", "theme_light", []],
      ["Theme: system", "theme_system", []],
    ]);
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    const row = within(dialog).getByText("Theme: dark").closest("[data-testid='palette-item']");
    expect(row).not.toHaveAttribute("data-disabled", "true");
    await act(async () => {
      await user.click(row as HTMLElement);
    });
    await waitFor(() => expect(mock.settings.get("theme")).toBe("dark"));
    expect(root().classList.contains("dark")).toBe(true);
  });
});

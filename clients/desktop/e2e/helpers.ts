// Shared steps of the end-to-end specs. The selectors are the app's own
// accessible names and its `data-slot`, `data-field` and `data-draft-id`
// hooks, the ones the vitest suites read too.

import { $, browser, expect } from "@wdio/globals";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

/** A lazily found element, as `$` answers. */
type Found = ReturnType<typeof $>;

/** Drive the fixture daemon (src-tauri/src/fixture.rs, `fixture_simulate`). */
export async function simulate(what: string): Promise<void> {
  await browser.execute(
    (w: string) =>
      (window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args: object) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke(
        "fixture_simulate",
        { what: w },
      ),
    what,
  );
}

/** Press keys one after the other, as a chord of the app's keymap is typed. */
export async function press(...keys: string[]): Promise<void> {
  for (const key of keys) await browser.keys(key);
}

/** A sidebar entry of `work`, the first account, by its label. */
export async function openMailbox(label: "Inbox" | "Drafts" | "Sent" | "Archive"): Promise<void> {
  const entry = $(`nav[aria-label="Accounts and mailboxes"] button[aria-label^="${label},"]`);
  await entry.click();
  await $(`[role="listbox"][aria-label="${label} messages"]`).waitForDisplayed();
}

/** The row of a message or draft in the open list whose accessible name has `text`. */
export function row(text: string): Found {
  return $(`[role="listbox"] [role="option"][aria-label*="${text}"]`);
}

/** The open dialog. */
export function dialog(): Found {
  return $('[role="dialog"]');
}

/**
 * The draft file holding `text`, among those the fixture keeps under the
 * run's temp dir (E2E_SCRATCH/tmp): its name without `.md`, and its text.
 */
export function draftFile(account: string, text: string): { stem: string; text: string } {
  const tmp = join(process.env.E2E_SCRATCH ?? "", "tmp");
  const run = readdirSync(tmp).find((d) => d.startsWith("mp-desktop-fixture-"));
  if (!run) throw new Error(`no fixture run directory under ${tmp}`);
  const dir = join(tmp, run, "drafts", account);
  const files = readdirSync(dir).filter((f) => f.endsWith(".md"));
  const hits = files.map((f) => ({ stem: f.slice(0, -3), text: readFileSync(join(dir, f), "utf8") })).filter((d) => d.text.includes(text));
  if (hits.length !== 1) throw new Error(`${hits.length} draft files of ${account} hold "${text}": ${files.join(", ")}`);
  return hits[0];
}

/** Open the selected row: a click only moves the cursor, Enter opens. */
export async function open(text: string): Promise<void> {
  await row(text).click();
  await press("Enter");
}

/** Pick an option of a native select: WebDriver's option click does not change it in WebKit. */
export async function choose(select: Found, value: string): Promise<void> {
  await browser.execute(
    (el: HTMLSelectElement, v: string) => {
      const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set;
      setter?.call(el, v);
      el.dispatchEvent(new Event("change", { bubbles: true }));
    },
    (await select.getElement()) as unknown as HTMLSelectElement,
    value,
  );
  await expect(select).toHaveValue(value);
}

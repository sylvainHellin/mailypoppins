// The HTML frame is sandboxed without allow-same-origin (docs/reader.md), so
// WebDriver, which runs its scripts from the app's page, cannot read inside
// it; the spec checks that it loaded, and reads the body in Text mode.

import { $, expect } from "@wdio/globals";
import { open, openMailbox, row } from "../helpers.ts";

const reader = () => $('aside[aria-label="Reader"]');

describe("the reader", () => {
  it("opens a message in the HTML frame the mpmsg scheme serves", async () => {
    await openMailbox("Inbox");
    await open("Angebot Dachsanierung");
    // Invisible until its load event, so displayed means the scheme answered.
    await expect(reader().$('iframe[data-slot="reader-frame"][title$=": Angebot Dachsanierung"]')).toBeDisplayed();
  });

  it("shows the plain text in Text mode", async () => {
    await reader().$('[role="group"][aria-label="Reader mode"] button[title^="Show the plain text"]').click();
    const text = reader().$('pre[aria-label="Message text: Angebot Dachsanierung"]');
    await text.waitForDisplayed();
    await expect(text).toHaveText(expect.stringContaining("anbei unser Angebot fuer die Dachsanierung"));
  });

  it("marks the opened message read in the list", async () => {
    await expect(row("Angebot Dachsanierung")).not.toHaveAttribute("aria-label", "unread", { containing: true });
  });
});

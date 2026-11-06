import { browser, expect } from "@wdio/globals";

describe("start on the fixtures", () => {
  it("shows both accounts' mailboxes and the inbox's messages", async () => {
    const sidebar = browser.$('[aria-label="Accounts and mailboxes"]');
    await sidebar.waitForDisplayed({ timeout: 30_000 });
    await expect(sidebar).toHaveText(expect.stringContaining("work"));
    await expect(sidebar).toHaveText(expect.stringContaining("home"));

    const list = browser.$('[aria-label="Message list"]');
    await list.waitForDisplayed();
    await expect(list.$('[aria-label*="Quarterly ledger review"]')).toBeDisplayed();
  });
});

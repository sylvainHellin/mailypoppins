// The send flows fixture_simulate drives: send_hold:<secs>, send_fail,
// send_partial (on an outbox retry) and send_pending_append, each read off
// the activity card, the Drafts list and the outbox view's state chips.

import { $, expect } from "@wdio/globals";
import { dialog, openMailbox, press, row, simulate } from "../helpers.ts";

const ANGEBOT = "Re: Angebot Dachsanierung";
const held = (subject: string) => $(`[role="group"][aria-label="Held send: ${subject}"]`);
const outbox = () => $('[aria-label="Outbox of work"]');
const chip = (state: string) => outbox().$(`.//*[@data-slot="outbox-state"][normalize-space()="${state}"]`);

/** Send the draft selected in Drafts, through the confirmation the app asks for. */
async function sendSelected(subject: string): Promise<void> {
  await openMailbox("Drafts");
  await row(subject).click();
  await press("x");
  await dialog().waitForDisplayed();
  await press("y");
  await dialog().waitForDisplayed({ reverse: true });
}

describe("sends on the fixtures", () => {
  before(async () => {
    // The bootstrap arms a 60 s hold on the Angebot draft at start.
    if (await held(ANGEBOT).isExisting()) {
      await held(ANGEBOT).$('button[aria-label="Cancel send"]').click();
      await held(ANGEBOT).waitForExist({ reverse: true });
    }
  });

  it("a held send counts down on its card, and Cancel keeps the draft", async () => {
    await simulate("send_hold:30");
    await sendSelected(ANGEBOT);
    await expect(held(ANGEBOT)).toHaveText(expect.stringContaining("Sending in"));
    await held(ANGEBOT).$('button[aria-label="Cancel send"]').click();
    await held(ANGEBOT).waitForExist({ reverse: true, timeout: 15_000 });
    await expect(row(ANGEBOT)).toBeExisting();
  });

  it("a failed send keeps the draft and parks a Failed row in the outbox", async () => {
    await simulate("send_hold:0");
    await simulate("send_fail");
    await sendSelected(ANGEBOT);
    await expect(row(ANGEBOT)).toBeExisting();
    await press("g", "o");
    await expect(chip("Failed")).toBeDisplayed();
  });

  it("a retry whose last recipient is refused leaves a partly delivered row", async () => {
    await simulate("send_partial");
    await press("R");
    await dialog().waitForDisplayed();
    await press("y");
    await expect(chip("Partly delivered")).toBeDisplayed();
    await expect(chip("Failed")).not.toBeExisting();
  });

  it("a send that owes its Sent copy shows Sent, copy owed", async () => {
    await press("Escape");
    await openMailbox("Inbox");
    await press("c", "n");
    const wizard = $('[data-slot="compose-wizard"][data-kind="new"]');
    await wizard.waitForDisplayed();
    await wizard.$('[data-field="to"]').setValue("kim@example.com");
    await wizard.$('[data-field="subject"]').setValue("E2E copy owed");
    await wizard.$('[data-field="body"]').setValue("Delivered, its Sent copy still owed.");
    await wizard.$("[data-submit]").click();
    await wizard.waitForDisplayed({ reverse: true });
    await simulate("send_pending_append");
    await sendSelected("E2E copy owed");
    await expect(row("E2E copy owed")).not.toBeExisting();
    await press("g", "o");
    await expect(chip("Sent, copy owed")).toBeDisplayed();
  });
});

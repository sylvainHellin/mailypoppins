// The draft flows fixture_simulate drives (editor_save, editor_invalid), and
// the PERSO-83 compose checks the webview reaches: cf's Signature select, cn's
// inline Body that skips the editor, and ta with a typed path (Browse is the
// native picker, out of WebDriver's reach).

import { $, expect } from "@wdio/globals";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { choose, draftFile, openMailbox, press, row, simulate } from "../helpers.ts";

const wizard = (kind: "new" | "forward") => $(`[data-slot="compose-wizard"][data-kind="${kind}"]`);
const banner = () => $('[data-slot="editing-banner"]');

describe("drafts and the editor", () => {
  let stem = "";

  it("cn's Create and edit opens the new draft in the editor, which fixture mode only names", async () => {
    await openMailbox("Inbox");
    await press("c", "n");
    await wizard("new").waitForDisplayed();
    await wizard("new").$('[data-field="to"]').setValue("kim@example.com");
    await wizard("new").$('[data-field="subject"]').setValue("E2E editor draft");
    await wizard("new").$("[data-submit]").click();
    await wizard("new").waitForDisplayed({ reverse: true });
    await expect(banner()).toHaveText(expect.stringContaining("Editing draft-"));
    await expect($("body")).toHaveText(expect.stringContaining("Fixture mode: the editor was not launched"));
  });

  it("a save in the editor lists the draft in Drafts with the saved text", async () => {
    await simulate("editor_save");
    await openMailbox("Drafts");
    await expect(row("E2E editor draft")).toBeExisting();
    const draft = draftFile("work", "E2E editor draft");
    stem = draft.stem;
    expect(draft.text).toContain("A line the fixture's editor added.");
  });

  it("a save that breaks the frontmatter marks the draft invalid", async () => {
    await simulate("editor_invalid");
    await expect($(`[role="option"][aria-label*="${stem}"][aria-label*=", invalid: "]`)).toBeExisting();
  });
});

describe("compose checks of PERSO-83", () => {
  it("cf forwards with the signature picked in the wizard", async () => {
    await openMailbox("Inbox");
    await row("Planned power outage Friday").click();
    await press("c", "f");
    await wizard("forward").waitForDisplayed();
    const signature = wizard("forward").$('[data-field="signature"]');
    await expect(signature).toHaveValue("work");
    await choose(signature, "short");
    await wizard("forward").$('[data-field="to"]').setValue("kim@example.com");
    await wizard("forward").$("[data-submit]").click();
    await wizard("forward").waitForDisplayed({ reverse: true });
    await openMailbox("Drafts");
    await expect(row("FW: Planned power outage Friday")).toBeExisting();
    // The short signature is "Me"; the account default, work's, would read "Kind regards,".
    expect(draftFile("work", "FW: Planned power outage Friday").text).toMatch(/<!-- mp:sig-start -->\nMe\n<!-- mp:sig-end -->/);
  });

  it("cn with an inline Body creates the draft and skips the editor", async () => {
    await openMailbox("Inbox");
    await press("c", "n");
    await wizard("new").waitForDisplayed();
    await wizard("new").$('[data-field="to"]').setValue("kim@example.com");
    await wizard("new").$('[data-field="subject"]').setValue("E2E inline body");
    await wizard("new").$('[data-field="body"]').setValue("Kurze Frage: morgen um zehn?");
    await expect(wizard("new").$("[data-submit]")).toHaveText(expect.stringContaining("Create"));
    await wizard("new").$("[data-submit]").click();
    await wizard("new").waitForDisplayed({ reverse: true });
    await openMailbox("Drafts");
    await expect(row("E2E inline body")).toBeExisting();
    const inline = draftFile("work", "E2E inline body");
    expect(inline.text).toContain("Kurze Frage: morgen um zehn?");
    await expect(banner()).not.toHaveText(expect.stringContaining(inline.stem));
  });

  it("ta attaches a typed path under ~ to the selected draft", async () => {
    writeFileSync(join(process.env.E2E_SCRATCH ?? "", "home", "e2e-report.pdf"), "%PDF-1.4\n");
    await openMailbox("Drafts");
    await row("E2E inline body").click();
    await press("t", "a");
    const attach = $('[data-slot="attachments-dialog"][data-kind="attach"]');
    await attach.waitForDisplayed();
    await attach.$("input").setValue("~/e2e-report.pdf");
    await attach.$('button[type="submit"]').click();
    await attach.waitForDisplayed({ reverse: true });
    const listed = $('aside[aria-label="Reader"] ul[aria-label="Attachments"]');
    await expect(listed).toHaveText(expect.stringContaining("~/e2e-report.pdf"));
    expect(draftFile("work", "E2E inline body").text).toContain("~/e2e-report.pdf");
  });
});

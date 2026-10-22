import { describe, expect, it, vi } from "vitest";
import {
  accountDraft,
  candidateDraft,
  checkLine,
  connectionKey,
  mailboxTwice,
  missingPasswordsLine,
  passwordKinds,
  probePlan,
  probeRefusalLine,
  runConnectionTest,
  withListing,
  writtenLine,
  CLIENT_EMPTY,
  DEFAULT_TENANT,
  FROM_EMPTY,
  MAILYPOPPINS_CLIENT_ID,
  HOST_EMPTY,
  NAME_EMPTY,
  NAME_SHAPE,
  nameTaken,
  overrideBad,
  parseOverrides,
  PORT_BAD,
  PRESET_ORDER,
  presetForm,
  stepErrors,
  stepOf,
  TENANT_EMPTY,
  USERNAME_EMPTY,
  withPreset,
  type WizardForm,
} from "@/app/wizard";
import { parseDeviceCode } from "@/app/signin";
import { invoke, mock, PROBE_IMAP_MAILBOXES, PROBE_WRONG_PASSWORD, probeConfiguredRefusal, probeRefused, resetMock } from "@/test/tauri-mock";

/** The keys the daemon's `account_block` reads (src/daemon/methods/config.rs), as paths. */
const ACCOUNT_BLOCK_KEYS = new Set([
  "name",
  "default_from",
  "auth_method",
  "save_to_sent",
  "oauth2",
  "oauth2.client_id",
  "oauth2.tenant_id",
  "mailboxes",
  "mailboxes.inbox",
  "mailboxes.archive",
  "mailboxes.sent",
  "mailboxes.drafts",
  "mailboxes.trash",
  "mailboxes.junk",
  "mailboxes.overrides",
  ...["smtp", "imap"].flatMap((t) =>
    ["", ".host", ".username", ".port", ".fetch_concurrency", ".body_fetch_deadline_secs", ".sync_interval_secs", ".accept_invalid_certs"].map(
      (k) => `${t}${k}`,
    ),
  ),
]);

function keyPaths(v: unknown, prefix = ""): string[] {
  if (typeof v !== "object" || v === null || Array.isArray(v)) return [];
  return Object.entries(v).flatMap(([k, inner]) => {
    const path = prefix ? `${prefix}.${k}` : k;
    return [path, ...keyPaths(inner, path)];
  });
}

const filled = (preset: WizardForm["preset"]): WizardForm => ({
  ...presetForm(preset),
  name: "new",
  default_from: "Me <me@example.com>",
  smtp_host: preset === "graph" ? "" : presetForm(preset).smtp_host || "smtp.example.com",
  smtp_username: preset === "graph" ? "" : "me@example.com",
  client_id: "client",
  tenant_id: "tenant",
});

describe("the account wizard's checks", () => {
  it("the name is required, slug-shaped and unused", () => {
    const form = filled("imap");
    expect(stepErrors("identity", { ...form, name: "  " }, []).name).toBe(NAME_EMPTY);
    expect(stepErrors("identity", { ...form, name: "my work" }, []).name).toBe(NAME_SHAPE);
    expect(stepErrors("identity", { ...form, name: "a/b" }, []).name).toBe(NAME_SHAPE);
    expect(stepErrors("identity", { ...form, name: "work" }, ["work", "home"]).name).toBe(nameTaken("work"));
    expect(stepErrors("identity", { ...form, name: "work-2" }, ["work", "home"])).toEqual({});
  });

  it("the IMAP presets need an SMTP host, a username and valid ports", () => {
    for (const preset of ["imap", "proton", "microsoft365"] as const) {
      const form = filled(preset);
      expect(stepErrors("servers", form, [])).toEqual({});
      const errors = stepErrors("servers", { ...form, smtp_host: " ", smtp_username: "", smtp_port: "70000", imap_port: "x" }, []);
      expect(errors).toMatchObject({ smtp_host: HOST_EMPTY, smtp_username: USERNAME_EMPTY, smtp_port: PORT_BAD, imap_port: PORT_BAD });
      // The IMAP host and username fall back to the SMTP ones.
      expect(stepErrors("servers", { ...form, imap_host: "", imap_username: "" }, [])).toEqual({});
    }
    expect(stepErrors("servers", { ...filled("graph"), smtp_host: "" }, []).smtp_host).toBeUndefined();
  });

  it("Microsoft 365 OAuth2 needs the client ID and the tenant ID, and Graph the email address", () => {
    const errors = stepErrors("servers", { ...filled("microsoft365"), client_id: "", tenant_id: " " }, []);
    expect(errors).toMatchObject({ client_id: CLIENT_EMPTY, tenant_id: TENANT_EMPTY });
    expect(stepErrors("servers", { ...filled("graph"), client_id: "", tenant_id: " " }, [])).toEqual({});
    expect(stepErrors("servers", { ...filled("imap"), client_id: "" }, []).client_id).toBeUndefined();
    expect(stepErrors("identity", { ...filled("graph"), default_from: "" }, []).default_from).toBe(FROM_EMPTY);
    expect(stepErrors("identity", { ...filled("imap"), default_from: "" }, []).default_from).toBeUndefined();
  });

  it("the review checks every step again", () => {
    const errors = stepErrors("review", { ...filled("microsoft365"), name: "", tenant_id: "", sent: "" }, []);
    expect(Object.keys(errors).sort()).toEqual(["name", "sent", "tenant_id"]);
  });
});

describe("the account wizard's draft", () => {
  it("every preset's draft has only account_block's keys and no password", () => {
    for (const preset of PRESET_ORDER) {
      const draft = accountDraft(filled(preset));
      for (const path of keyPaths(draft)) expect(ACCOUNT_BLOCK_KEYS.has(path), `${preset}: ${path}`).toBe(true);
      expect(JSON.stringify(draft)).not.toMatch(/password/i);
    }
  });

  it("follows the CLI's presets and fallbacks", () => {
    const proton = accountDraft(filled("proton"));
    expect(proton.smtp).toEqual({ host: "127.0.0.1", port: 1025, username: "me@example.com", accept_invalid_certs: true });
    expect(proton.imap).toEqual({ host: "127.0.0.1", port: 1143, accept_invalid_certs: true });
    expect(proton.auth_method).toBeUndefined();
    expect(proton.oauth2).toBeUndefined();

    const ms = accountDraft(filled("microsoft365"));
    expect(ms.auth_method).toBe("oauth2");
    expect(ms.oauth2).toEqual({ client_id: "client", tenant_id: "tenant" });
    expect(ms.smtp).toMatchObject({ host: "smtp.office365.com", port: 587 });
    expect(ms.imap).toEqual({ host: "outlook.office365.com", port: 993 });

    const graph = accountDraft(filled("graph"));
    expect(graph.auth_method).toBe("graph");
    expect(graph.oauth2).toEqual({ client_id: "client", tenant_id: "tenant" });
    expect(graph.smtp).toBeUndefined();
    expect(graph.imap).toBeUndefined();
    expect(graph.mailboxes).toEqual({ inbox: "Inbox", archive: "Archive", sent: "Sent Items" });

    const imap = accountDraft({
      ...filled("imap"),
      default_from: "",
      trash: " Papierkorb ",
      overrides: " Labels/* ignore \n\nSent Items/Old  status-only\n",
    });
    expect(imap.default_from).toBe("me@example.com");
    expect(imap.mailboxes).toEqual({
      inbox: "INBOX",
      archive: "Archive",
      sent: "Sent",
      trash: "Papierkorb",
      overrides: [
        { match: "Labels/*", policy: "ignore" },
        { match: "Sent Items/Old", policy: "status-only" },
      ],
    });
  });

  it("an override line needs a pattern and one of the three policies (#0141)", () => {
    expect(parseOverrides("Labels/* sometimes")).toEqual({ overrides: [], bad: "Labels/* sometimes" });
    expect(parseOverrides("ignore").bad).toBe("ignore");
    expect(stepErrors("mailboxes", { ...filled("imap"), overrides: "Labels/*" }, []).overrides).toBe(overrideBad("Labels/*"));
    expect(stepErrors("mailboxes", { ...filled("imap"), overrides: "Labels/* ignore" }, [])).toEqual({});
    expect(stepOf("overrides")).toBe("mailboxes");
  });

  it("an empty Graph client and tenant become the mailypoppins app and common, as in the CLI", () => {
    const graph = accountDraft({ ...filled("graph"), client_id: " ", tenant_id: "" });
    expect(graph.oauth2).toEqual({ client_id: MAILYPOPPINS_CLIENT_ID, tenant_id: DEFAULT_TENANT });
    expect(MAILYPOPPINS_CLIENT_ID).toBe("439d7ebd-7898-4df2-b2d6-88bb61e2597b");
    expect(DEFAULT_TENANT).toBe("common");
    expect(accountDraft({ ...filled("graph"), client_id: "", tenant_id: "contoso.com" }).oauth2).toEqual({
      client_id: MAILYPOPPINS_CLIENT_ID,
      tenant_id: "contoso.com",
    });
  });

  it("a preset switch keeps who the user is and takes the provider's servers", () => {
    const typed = { ...presetForm("imap"), name: "mine", smtp_username: "me@example.com", smtp_host: "smtp.x" };
    const ms = withPreset(typed, "microsoft365");
    expect(ms).toMatchObject({ name: "mine", smtp_username: "me@example.com", smtp_host: "smtp.office365.com", smtp_port: "587" });
    expect(withPreset(presetForm("imap"), "proton").name).toBe("proton");
  });
});

describe("the account wizard's connection test", () => {
  const secrets = { smtp: "pw", imap: "", sameImap: true };

  it("probes what the CLI probes, in its order, and only a password login stops the wizard", () => {
    expect(probePlan("imap", true)).toEqual([
      { check: "smtp", password: "smtp", required: true },
      { check: "imap", password: "smtp", required: false },
      { check: "mailboxes", password: "smtp", required: false },
    ]);
    expect(probePlan("proton", false)).toEqual([
      { check: "smtp", password: "smtp", required: true },
      { check: "imap", password: "imap", required: true },
      { check: "mailboxes", password: "imap", required: false },
    ]);
    expect(probePlan("microsoft365", true)).toEqual([{ check: "mailboxes", password: null, required: false }]);
    expect(probePlan("graph", true).map((c) => [c.check, c.required])).toEqual([
      ["graph", false],
      ["mailboxes", false],
    ]);
  });

  it("carries the draft without its mailbox mapping, and goes stale when the candidate or the IMAP password choice moves", () => {
    const form = filled("imap");
    expect(candidateDraft(form).mailboxes).toBeUndefined();
    expect(candidateDraft(form)).toEqual({ ...accountDraft(form), mailboxes: undefined });
    expect(connectionKey({ ...form, sent: "Other", overrides: "x ignore" }, true)).toBe(connectionKey(form, true));
    expect(connectionKey({ ...form, imap_host: "imap2" }, true)).not.toBe(connectionKey(form, true));
    expect(connectionKey(form, false)).not.toBe(connectionKey(form, true));
  });

  it("passes with the server's mailboxes, and fails on a refused SMTP login without going on", async () => {
    resetMock();
    const ok = await runConnectionTest(filled("imap"), secrets);
    expect(ok).toMatchObject({ result: { passed: true, listed: PROBE_IMAP_MAILBOXES } });
    resetMock();
    const refused = await runConnectionTest(filled("imap"), { ...secrets, smtp: PROBE_WRONG_PASSWORD });
    expect(refused).toMatchObject({ result: { passed: false, listed: null } });
    if (!("result" in refused)) throw new Error("a result");
    expect(refused.result.runs).toEqual([{ check: "smtp", required: true, ok: false, error: probeRefused("smtp"), mailboxes: [] }]);
    expect(mock.calls.filter((c) => c.cmd === "config_probe")).toHaveLength(1);
  });

  it("the SMTP password failing for IMAP, and a list that cannot be read, are warnings, as in the CLI", async () => {
    resetMock();
    vi.mocked(invoke)
      .mockImplementationOnce(async () => ({ ok: true, error: null, mailboxes: [] }))
      .mockImplementationOnce(async () => ({ ok: false, error: "IMAP login failed: no", mailboxes: [] }))
      .mockImplementationOnce(async () => ({ ok: false, error: "IMAP login failed: no", mailboxes: [] }));
    const outcome = await runConnectionTest(filled("imap"), secrets);
    if (!("result" in outcome)) throw new Error("a result");
    expect(outcome.result.passed).toBe(true);
    expect(outcome.result.listed).toBeNull();
    const [, imap, list] = outcome.result.runs;
    expect(checkLine(imap, true)).toBe(
      'IMAP: the SMTP password did not work for IMAP. IMAP login failed: no. Untick "IMAP uses the SMTP password" to give IMAP its own, or set it in Settings later.',
    );
    expect(checkLine(list, true)).toBe("Mailboxes: the server's list could not be read. IMAP login failed: no. Type the names on the next step.");
  });

  it("names the daemon's refusal of a configured name as a taken name", async () => {
    expect(probeRefusalLine("work", probeConfiguredRefusal("work"))).toBe("An account named work is already configured. Go back to Identity and choose another name.");
    expect(probeRefusalLine("work", "something else")).toBe("something else");
    resetMock();
    mock.failing.set("config_probe", { kind: "protocol", code: -32602, message: probeConfiguredRefusal("new") });
    expect(await runConnectionTest(filled("graph"), secrets)).toEqual({
      refusal: "An account named new is already configured. Go back to Identity and choose another name.",
    });
  });
});

describe("the account wizard's mailbox defaults", () => {
  it("picks the CLI's preferred names from the server's list, each taken once, and leaves the marked roles to the server", () => {
    const form = { ...filled("imap"), junk: "Old", overrides: "Labels/* ignore" };
    expect(withListing(form, PROBE_IMAP_MAILBOXES)).toMatchObject({ inbox: "INBOX", archive: "Archive", sent: "Sent", drafts: "", trash: "", junk: "", overrides: "Labels/* ignore" });
    // Gmail's names: the archive is All Mail, the sent folder found case-insensitively.
    const gmail = ["INBOX", "[Gmail]/Drafts", "All Mail", "sent messages"];
    expect(withListing(form, gmail)).toMatchObject({ inbox: "INBOX", archive: "All Mail", sent: "sent messages" });
    // No preferred name: the first mailbox the roles before did not take.
    expect(withListing(form, ["Posteingang", "Gesendet", "Archiv"])).toMatchObject({ inbox: "Posteingang", archive: "Gesendet", sent: "Archiv" });
    expect(withListing(filled("graph"), ["Inbox", "Sent Items", "Archive"])).toMatchObject({ inbox: "Inbox", archive: "Archive", sent: "Sent Items" });
  });

  it("refuses one mailbox for two roles, but lets the marked roles share one", () => {
    const form = filled("imap");
    expect(stepErrors("mailboxes", { ...form, archive: "INBOX" }, []).archive).toBe(mailboxTwice("INBOX", "inbox"));
    expect(stepErrors("mailboxes", { ...form, trash: "Sent" }, []).trash).toBe(mailboxTwice("Sent", "sent"));
    expect(stepErrors("mailboxes", { ...form, trash: "Bin", junk: "Bin" }, [])).toEqual({});
  });
});

describe("the account wizard's write", () => {
  it("stores the passwords the test used, and says what it stored or did not", () => {
    expect(passwordKinds("imap", true)).toEqual(["smtp"]);
    expect(passwordKinds("proton", false)).toEqual(["smtp", "imap"]);
    expect(passwordKinds("graph", false)).toEqual([]);
    expect(writtenLine("a", "imap", false, true)).toBe("Added the account a; its password is stored");
    expect(writtenLine("a", "imap", true, false)).toBe("Wrote config.toml with the account a; its SMTP and IMAP passwords are stored");
    expect(writtenLine("a", "microsoft365", false, true)).toBe("Added the account a; its sign-in is stored");
    expect(missingPasswordsLine("a", ["smtp", "imap"])).toBe(
      "Account a is configured, but its SMTP and IMAP passwords are not stored, so it cannot log in yet. Set them in Settings.",
    );
  });
});

describe("the device code", () => {
  it("splits the progress message on its one space", () => {
    expect(parseDeviceCode("https://microsoft.com/devicelogin FXTR-CODE")).toEqual({ url: "https://microsoft.com/devicelogin", code: "FXTR-CODE" });
    expect(parseDeviceCode("no-space")).toBeNull();
    expect(parseDeviceCode(null)).toBeNull();
  });
});

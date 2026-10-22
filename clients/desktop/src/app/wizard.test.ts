import { describe, expect, it } from "vitest";
import {
  accountDraft,
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

describe("the device code", () => {
  it("splits the progress message on its one space", () => {
    expect(parseDeviceCode("https://microsoft.com/devicelogin FXTR-CODE")).toEqual({ url: "https://microsoft.com/devicelogin", code: "FXTR-CODE" });
    expect(parseDeviceCode("no-space")).toBeNull();
    expect(parseDeviceCode(null)).toBeNull();
  });
});

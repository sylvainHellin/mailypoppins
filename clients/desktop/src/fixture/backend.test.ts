import { describe, expect, it } from "vitest";
import { retiredOauth2Message, shellWord } from "@/fixture/backend";

describe("the fixture's printed mp commands", () => {
  it("quote an account name a shell would split, as the daemon's do (PERSO-199)", () => {
    expect(shellWord("work")).toBe("work");
    expect(shellWord("me@tum.de_2-x")).toBe("me@tum.de_2-x");
    expect(shellWord("TUM Mail")).toBe("'TUM Mail'");
    expect(shellWord("Bob's $HOME")).toBe("'Bob'\\''s $HOME'");
    expect(shellWord("")).toBe("''");
    const message = retiredOauth2Message("TUM Mail");
    expect(message).toContain("`mp config oauth2-login --account 'TUM Mail'`");
    expect(message).toContain("`mp config set-password smtp --account 'TUM Mail'`");
  });
});

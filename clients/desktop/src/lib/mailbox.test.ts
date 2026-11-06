import { describe, expect, it } from "vitest";
import { readableRecipients, readableSender, senderAddress, senderName } from "@/lib/mailbox";

describe("the sender's address", () => {
  it("is what the last angle brackets hold, else the whole field", () => {
    expect(senderAddress('"Meyer, Robin" <robin@example.com>')).toBe("robin@example.com");
    expect(senderAddress("Robin <robin@example.com> ")).toBe("robin@example.com");
    expect(senderAddress("  robin@example.com ")).toBe("robin@example.com");
    expect(senderAddress("Robin")).toBe("Robin");
  });

  it("is never one a display name or a comment carries", () => {
    expect(senderAddress('"Bob <bob@x.example>" <alice@y.example>')).toBe("alice@y.example");
    expect(senderAddress("Bob <bob@x.example> <alice@y.example>")).toBe("alice@y.example");
    expect(senderAddress("Alice <alice@y.example> (via <list@z.example>)")).toBe("alice@y.example");
    expect(senderAddress("Alerts > Prod <a@x.example>")).toBe("a@x.example");
  });
});

describe("the sender's name", () => {
  it("is the display name unquoted and unescaped, else the address", () => {
    expect(senderName('"Doe, Jane" <jane@x.example>')).toBe("Doe, Jane");
    expect(senderName('"Jane \\"JJ\\" Doe" <jane@x.example>')).toBe('Jane "JJ" Doe');
    expect(senderName('"Bob <bob@x.example>" <alice@y.example>')).toBe("Bob <bob@x.example>");
    expect(senderName('"jane@x.example" <jane@x.example>')).toBe("jane@x.example");
    expect(senderName("<jane@x.example>")).toBe("jane@x.example");
    expect(senderName("jane@x.example")).toBe("jane@x.example");
    expect(senderName("  ")).toBe("(no sender)");
    // A comment stays in the name; the address is read without one.
    expect(senderName("Alice (Team) <alice@x.example> (via <list@y.example>)")).toBe("Alice (Team)");
    expect(senderAddress("Alice (Team) <alice@x.example> (via <list@y.example>)")).toBe("alice@x.example");
    // An unclosed `(` is text, and the `<...>` after it still names the mailbox.
    expect(senderAddress("bob@x.example ( <alice@y.example>")).toBe("alice@y.example");
  });
});

describe("a header as a reader is shown it", () => {
  it("names the sender unquoted, without a name that repeats the address", () => {
    expect(readableSender('"Doe, Jane" <jane@x.example>')).toBe("Doe, Jane <jane@x.example>");
    expect(readableSender('"Say \\"hi\\" \\\\ bye" <a@x.example>')).toBe('Say "hi" \\ bye <a@x.example>');
    expect(readableSender('"A@x.example" <a@x.example>')).toBe("a@x.example");
    expect(readableSender('Bo"b <b@x.example>')).toBe('Bo"b <b@x.example>');
    expect(readableSender(" (unknown) ")).toBe("(unknown)");
  });

  it("names each recipient of a list so", () => {
    expect(readableRecipients('"Doe, Jane" <jane@x.example>, "b@y.example" <b@y.example>,c@z.example')).toBe(
      "Doe, Jane <jane@x.example>, b@y.example, c@z.example",
    );
    expect(readableRecipients("")).toBe("");
    // An escaped quote leaves the name quoted, so its comma stays in it.
    expect(readableRecipients('"Jane \\"JJ\\", Doe" <jane@x.example>, bob@y.example')).toBe(
      'Jane "JJ", Doe <jane@x.example>, bob@y.example',
    );
  });
});

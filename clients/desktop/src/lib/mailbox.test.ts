import { describe, expect, it } from "vitest";
import { readableRecipients, readableSender, senderAddress, senderName, splitAddresses } from "@/lib/mailbox";

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

  // The table `mp_core::parse::tests::the_sender_address_the_desktop_reads_too`
  // runs through `extract_email_address`, so Copy sender copies what a reply goes to.
  it.each([
    ['"(" <x@blocked.example>', "x@blocked.example"],
    ['"\\"" <x@blocked.example>', "x@blocked.example"],
    ['"bob@blocked.example," <alice@ok.example>', "alice@ok.example"],
    ["x@y.example (Cron Daemon)", "x@y.example"],
    ["x@blocked.example (c) alice@ok.example", "alice@ok.example"],
    ["a@x.example, b@y.example", "a@x.example"],
    ["Team: a@x.example, b@y.example;", "a@x.example"],
    ["Bob, alice@y.example (x <bob@x.example>)", "alice@y.example"],
    ["John <john@x.example", "John <john@x.example"],
    ["MAILER-DAEMON", "MAILER-DAEMON"],
    ["Team: alice@ok.example;, x@blocked.example", "alice@ok.example"],
    ["a@x.example, , b@y.example", "a@x.example"],
    ['x@blocked.example, "< G":;', "x@blocked.example"],
    ['<x@blocked.example>, "< G":;', "x@blocked.example"],
    ["x@blocked.example, alice@ok.example", "x@blocked.example"],
    ['"Bob <bob@x.example>"', '"Bob <bob@x.example>"'],
    ['"Bob <bob@x.example>" alice@y.example', "alice@y.example"],
    ["Cron Dæmon <root@x.example>", "root@x.example"],
    // A quoted local part is the address's (PERSO-261).
    ['"john.doe"@example.com', '"john.doe"@example.com'],
    ['"john.doe"@example.com, c@y.example', '"john.doe"@example.com'],
    ['John <"john.doe"@example.com>', '"john.doe"@example.com'],
    ['"john doe"@example.com (J)', '"john doe"@example.com'],
    ['"a,b"@x.example, c@y.example', '"a,b"@x.example'],
    // Several mailboxes: the first holding a `<...>`, by its last one.
    ["A <a@x.example>, B <b@y.example>", "a@x.example"],
    ["x@blocked.example, Alice <alice@ok.example>", "alice@ok.example"],
    ["<x@blocked.example>, Alice <alice@ok.example>", "x@blocked.example"],
    ["Bob <bob@x.example> <alice@y.example>, c@z.example", "alice@y.example"],
    ["Doe, Jane <jane@y.example>", "jane@y.example"],
    ["alice@y.example (Bob, <bob@x.example>", "bob@x.example"],
    ['x@blocked.example ("), alice@ok.example', "x@blocked.example"],
    // An older row's encoded-word name decoded to an address and a comma:
    // the bracketed mailbox, as main answered.
    ["x@blocked.example, <alice@ok.example>", "alice@ok.example"],
    // Only a `"` never closed is a literal.
    ['"Doe, <x@y.example>" Bo"b <b@x.example>, c@y.example', "b@x.example"],
  ])("reads %s as the daemon does", (from, address) => {
    expect(senderAddress(from)).toBe(address);
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
    // A quoted string closed before a `"` never closed is opened.
    expect(senderName('"Doe, Jane" Bo"b <b@x.example>')).toBe('Doe, Jane Bo"b');
  });

  // `mp_core::parse::tests::a_sender_is_shown_unquoted_and_by_the_mailbox_mailbox_addresses_reads`.
  it.each([
    ["A <a@x.example>, B <b@y.example>", "A", "A <a@x.example>, B <b@y.example>"],
    [
      "x@blocked.example, Alice <alice@ok.example>",
      "Alice",
      "x@blocked.example, Alice <alice@ok.example>",
    ],
    ["x@blocked.example, <alice@ok.example>", "alice@ok.example", "x@blocked.example, <alice@ok.example>"],
    ['"Doe, Jane" <jane@x.example>, B <b@y.example>', "Doe, Jane", "Doe, Jane <jane@x.example>, B <b@y.example>"],
  ])("of %s is that of the mailbox a reply goes to", (from, name, readable) => {
    expect(senderName(from)).toBe(name);
    expect(readableSender(from)).toBe(readable);
  });
});

describe("an ordinary From", () => {
  // As the daemon stores it, which is how main stored it
  // (`mp_core::parse::tests::an_ordinary_address_header_reads_as_it_always_did`).
  it.each([
    ["Jane Doe <jane@x.example>", "Jane Doe <jane@x.example>", "Jane Doe", "jane@x.example"],
    ['"Doe, Jane" <jane@x.example>', "Doe, Jane <jane@x.example>", "Doe, Jane", "jane@x.example"],
    ["jane@x.example", "jane@x.example", "jane@x.example", "jane@x.example"],
    ["<jane@x.example>", "jane@x.example", "jane@x.example", "jane@x.example"],
    ["Jürgen Müller <j@x.example>", "Jürgen Müller <j@x.example>", "Jürgen Müller", "j@x.example"],
    ["Alice (Team) <a@x.example>", "Alice (Team) <a@x.example>", "Alice (Team)", "a@x.example"],
    ["root@x.example (Cron Daemon)", "root@x.example (Cron Daemon)", "root@x.example (Cron Daemon)", "root@x.example"],
    ["CCBE [TUBVCMS] <r@x.example>", "CCBE [TUBVCMS] <r@x.example>", "CCBE [TUBVCMS]", "r@x.example"],
  ])("%s reads as it always did", (from, readable, name, address) => {
    expect(readableSender(from)).toBe(readable);
    expect(senderName(from)).toBe(name);
    expect(senderAddress(from)).toBe(address);
  });

  it("shows a quoted bracket as the name's", () => {
    expect(readableSender('"Bob <bob@x.example>" <alice@y.example>')).toBe("Bob <bob@x.example> <alice@y.example>");
    expect(readableSender('"Bob <bob@x.example>"')).toBe('"Bob <bob@x.example>"');
    expect(senderName('"Bob <bob@x.example>"')).toBe('"Bob <bob@x.example>"');
    expect(senderAddress('"Alerts > Prod" <a@x.example> ("<b@y.example>")')).toBe("a@x.example");
    expect(senderAddress('" <x@y.example>')).toBe("x@y.example");
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

  // `mp_core::addresses::tests::split_addresses_keeps_a_comment_whole`.
  it.each([
    ['x@blocked.example ("), alice@ok.example', ['x@blocked.example (")', "alice@ok.example"]],
    ["a@x.example (Team, eng), b@y.example", ["a@x.example (Team, eng)", "b@y.example"]],
    ["a@x.example (\\), b), b@y.example", ["a@x.example (\\), b)", "b@y.example"]],
    ["bob@x.example ( , <alice@y.example>", ["bob@x.example (", "<alice@y.example>"]],
    ['Bo"b <b@x.example>, c@y.example', ['Bo"b <b@x.example>', "c@y.example"]],
    [
      '"Doe, Jane" <jane@x.example>, Bo"b <b@x.example>, c@y.example',
      ['"Doe, Jane" <jane@x.example>', 'Bo"b <b@x.example>', "c@y.example"],
    ],
    [
      '"Smith (Ops), A" <a@x.example>, Bo"b (c, d) <b@x.example>, c@y.example',
      ['"Smith (Ops), A" <a@x.example>', 'Bo"b (c, d) <b@x.example>', "c@y.example"],
    ],
  ])("never cuts a recipient inside a comment or a closed quote: %s", (list, recipients) => {
    expect(splitAddresses(list)).toEqual(recipients);
  });
});

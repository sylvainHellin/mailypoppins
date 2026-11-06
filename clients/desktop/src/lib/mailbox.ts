// A From read as the daemon reads it (`mp_core::parse::mailbox_addresses`):
// once comments are dropped, it is cut into mailboxes at every `,` and `;`
// outside a quoted string, and each names the address in its last `<...>`
// outside a quoted string, else its own address, else its bare tokens holding
// an `@`, never one inside a quoted string but a quoted local part
// (`"john.doe"@x`); Copy sender copies the first mailbox's. The daemon
// stores a name quoted whenever it holds syntax (`"Bob <bob@x>" <alice@y>`); a
// row an older build stored may hold it decoded and bare (`bob@x, <alice@y>`),
// so a row, the reader and Copy sender read it here, and name the mailbox a
// block matches and a reply goes to.

/**
 * The index in `raw` of every character outside its comments, honouring
 * quoted strings when `quotes`; null when one is never closed.
 */
function uncommented(raw: string, quotes: boolean): number[] | null {
  const kept: number[] = [];
  let depth = 0;
  let opened = 0;
  let quoted = false;
  let escaped = false;
  for (let at = 0; at < raw.length; at++) {
    const c = raw[at];
    if (escaped) {
      escaped = false;
    } else if (c === "\\" && (quoted || depth > 0)) {
      escaped = true;
    } else if (quotes && c === '"' && depth === 0) {
      quoted = !quoted;
    } else if (c === "(" && !quoted) {
      if (depth === 0) opened = at;
      depth += 1;
      continue;
    } else if (c === ")" && depth > 0) {
      depth -= 1;
      continue;
    }
    if (depth === 0) kept.push(at);
  }
  if (quoted) return null;
  if (depth > 0) {
    // The outermost `(` never closed: a literal, and what follows is read afresh.
    const rest = uncommented(raw.slice(opened + 1), quotes);
    return rest === null ? null : [...kept, opened, ...rest.map((at) => opened + 1 + at)];
  }
  return kept;
}

/**
 * The index of every character of `raw` outside its `(comments)`; an
 * unclosed `(` or `"` is text, as a decoded display name may hold one.
 */
function uncommentedOffsets(raw: string): number[] {
  return uncommented(raw, true) ?? uncommented(raw, false) ?? Array.from({ length: raw.length }, (_, at) => at);
}

/** A character of a From outside its comments (`uncommentedChars`). */
interface UncommentedChar {
  at: number;
  c: string;
  /** Whether it lies in a quoted string, its `"` and a `\` escape included. */
  quoted: boolean;
}

/**
 * Every character of `raw` outside its comments, with its index and whether it
 * lies in a quoted string; a `"` never closed quotes nothing. A `<`, `>`, `,`,
 * `;` or `:` is syntax only outside a quoted string (`mp_core::parse::uncommented_chars`).
 */
function uncommentedChars(raw: string): UncommentedChar[] {
  let quoted = false;
  let escaped = false;
  const chars = uncommentedOffsets(raw).map((at) => {
    const c = raw[at];
    let inside = quoted;
    if (escaped) {
      escaped = false;
      inside = true;
    } else if (quoted && c === "\\") {
      escaped = true;
      inside = true;
    } else if (c === '"') {
      quoted = !quoted;
      inside = true;
    }
    return { at, c, quoted: inside };
  });
  if (quoted) for (const char of chars) char.quoted = false;
  return chars;
}

/** `name` with its quoted strings opened, a `\` escape in one undone; as written when a `"` is never closed. */
function unquotedName(name: string): string {
  let out = "";
  let quoted = false;
  let escaped = false;
  for (const c of name) {
    if (escaped) {
      out += c;
      escaped = false;
    } else if (quoted && c === "\\") {
      escaped = true;
    } else if (c === '"') {
      quoted = !quoted;
    } else {
      out += c;
    }
  }
  return (quoted ? name : out).trim();
}

/**
 * The display name and address of the mailbox `from` names, or null when it
 * holds no `<...>` outside a comment and a quoted string. The address is read
 * without comments, the name keeps them (`Alice (Team) <a@x>`). The name is
 * empty when there is none or it only repeats the address, as Graph's often
 * does (`"a@x" <a@x>`).
 */
function displayMailbox(from: string): { name: string; address: string } | null {
  const chars = uncommentedChars(from);
  let open = -1;
  for (let i = chars.length - 1; i >= 0; i--) {
    if (chars[i].c === "<" && !chars[i].quoted) {
      open = i;
      break;
    }
  }
  if (open < 0) return null;
  // `>` is searched only after the `<`: a name may hold one (`Alerts > Prod <a@x>`).
  let close = -1;
  for (let i = open + 1; i < chars.length; i++) {
    if (chars[i].c === ">" && !chars[i].quoted) {
      close = i;
      break;
    }
  }
  if (close < 0) return null;
  const address = chars
    .slice(open + 1, close)
    .map((char) => char.c)
    .join("")
    .trim();
  if (!address) return null;
  const name = unquotedName(from.slice(0, chars[open].at));
  return { name: name.toLowerCase() === address.toLowerCase() ? "" : name, address };
}

/** `"Doe, Jane" <jane@example.com>` to `Doe, Jane`; a bare address stays. */
export function senderName(from: string): string {
  const f = from.trim();
  if (!f) return "(no sender)";
  const m = displayMailbox(f);
  return m ? m.name || m.address : f;
}

/** A character of one mailbox's text as its address is read (`addressText`). */
interface AddressChar {
  c: string;
  /** Whether it lies in a quoted local part, where nothing is syntax. */
  local: boolean;
}

/**
 * The mailboxes of a From given as `uncommentedChars` answers it: cut at every
 * `,` and `;` outside a quoted string (`mp_core::parse::mailbox_parts`).
 */
function mailboxParts(chars: UncommentedChar[]): UncommentedChar[][] {
  const parts: UncommentedChar[][] = [[]];
  for (const char of chars) {
    if (!char.quoted && (char.c === "," || char.c === ";")) parts.push([]);
    else parts[parts.length - 1].push(char);
  }
  return parts;
}

/**
 * The address in the last `<...>` of one mailbox outside a quoted string, the
 * `>` searched only after it, trimmed; null when there is none, it is never
 * closed or it holds nothing (`mp_core::parse::bracketed_in`).
 */
function lastBracketed(part: UncommentedChar[]): string | null {
  let open = -1;
  for (let i = part.length - 1; i >= 0; i--) {
    if (part[i].c === "<" && !part[i].quoted) {
      open = i;
      break;
    }
  }
  if (open < 0) return null;
  let close = -1;
  for (let i = open + 1; i < part.length; i++) {
    if (part[i].c === ">" && !part[i].quoted) {
      close = i;
      break;
    }
  }
  if (close < 0) return null;
  const address = part
    .slice(open + 1, close)
    .map((char) => char.c)
    .join("")
    .trim();
  return address || null;
}

/**
 * One mailbox's text as its address is read (`mp_core::parse::address_text`):
 * a quoted string is a name, each character a space, unless an `@` outside a
 * quoted string follows it, which makes it a local part kept as written.
 */
function addressText(part: UncommentedChar[]): AddressChar[] {
  const text: AddressChar[] = [];
  let at = 0;
  while (at < part.length) {
    if (!part[at].quoted) {
      text.push({ c: part[at].c, local: false });
      at += 1;
      continue;
    }
    let end = at;
    while (end < part.length && part[end].quoted) end += 1;
    const local = end < part.length && part[end].c === "@" && !part[end].quoted;
    for (let i = at; i < end; i++) text.push(local ? { c: part[i].c, local: true } : { c: " ", local: false });
    at = end;
  }
  return text;
}

/** `text` with every character outside a local part that `trimmed` answers cut from both ends. */
function trimPlain(text: AddressChar[], trimmed: (c: string) => boolean): AddressChar[] {
  const cut = (char: AddressChar) => !char.local && trimmed(char.c);
  let start = 0;
  while (start < text.length && cut(text[start])) start += 1;
  let end = text.length;
  while (end > start && cut(text[end - 1])) end -= 1;
  return text.slice(start, end);
}

const isSpace = (c: string) => /\s/.test(c);
const joined = (text: AddressChar[]) => text.map((char) => char.c).join("");

/**
 * The address one mailbox with no `<` names (`mp_core::parse::listed_member`):
 * what comes up to its last `:` dropped (a group's name), trimmed and any `"`
 * of a quote never closed trimmed off; null when it holds no `@`, holds
 * whitespace outside a local part, or a group's name holds an `@`.
 */
function listedMember(text: AddressChar[]): string | null {
  let colon = -1;
  for (let i = text.length - 1; i >= 0; i--) {
    if (text[i].c === ":" && !text[i].local) {
      colon = i;
      break;
    }
  }
  if (colon >= 0 && text.slice(0, colon).some((char) => char.c === "@" && !char.local)) return null;
  const member = trimPlain(
    trimPlain(text.slice(colon + 1), isSpace),
    (c) => c === '"',
  );
  const spaced = member.some((char) => isSpace(char.c) && !char.local);
  return !spaced && member.some((char) => char.c === "@") ? joined(member) : null;
}

/**
 * The addresses one mailbox names (`mp_core::parse::part_addresses`): that in
 * its last `<...>` when it holds a `<` outside a quoted string, none when that
 * `<` is never closed; otherwise its address, else every bare token of it
 * holding an `@`, the last first.
 */
function partAddresses(part: UncommentedChar[]): string[] {
  if (part.some((char) => char.c === "<" && !char.quoted)) {
    const address = lastBracketed(part);
    return address === null ? [] : [address];
  }
  const text = addressText(part);
  const member = listedMember(text);
  if (member !== null) return [member];
  const tokens: AddressChar[][] = [[]];
  for (const char of text) {
    if (!char.local && (isSpace(char.c) || char.c === "," || char.c === ";")) tokens.push([]);
    else tokens[tokens.length - 1].push(char);
  }
  const found: string[] = [];
  for (const token of tokens.reverse()) {
    const address = joined(trimPlain(token, (c) => c === '"' || c === "<" || c === ">"));
    if (address.includes("@") && !found.includes(address)) found.push(address);
  }
  return found;
}

/** The address of every mailbox `from` names, each once, in order (`mp_core::parse::mailbox_addresses`). */
function mailboxAddresses(from: string): string[] {
  const addresses: string[] = [];
  for (const part of mailboxParts(uncommentedChars(from))) {
    for (const address of partAddresses(part)) {
      if (!addresses.includes(address)) addresses.push(address);
    }
  }
  return addresses;
}

/**
 * The address of the first mailbox `from` names (`mp_core::parse::extract_email_address`):
 * `"Bob <bob@x>" <alice@example.com>` to `alice@example.com`,
 * `x@example.com (Cron Daemon)` to `x@example.com`; a field naming no address stays, trimmed.
 */
export function senderAddress(from: string): string {
  return mailboxAddresses(from)[0] ?? from.trim();
}

/**
 * A From as a reader is shown it: `Name <address>` with the name unquoted and
 * unescaped, the bare address when there is no name or it repeats the
 * address. For display only: the name is no longer quoted.
 */
export function readableSender(from: string): string {
  const m = displayMailbox(from);
  if (!m) return from.trim();
  return m.name ? `${m.name} <${m.address}>` : m.address;
}

/**
 * A comma-separated address list split at every `,` outside a quoted name and
 * a comment, each part kept whole with its comments; a `(` or `"` never closed
 * is a literal (`mp_core::addresses::split_addresses`).
 */
function splitAddresses(list: string): string[] {
  const parts: string[] = [];
  const push = (part: string) => {
    if (part.trim()) parts.push(part.trim());
  };
  let start = 0;
  for (const { at, c, quoted } of uncommentedChars(list)) {
    if (c === "," && !quoted) {
      push(list.slice(start, at));
      start = at + 1;
    }
  }
  push(list.slice(start));
  return parts;
}

/** A To or Cc as a reader is shown it: each recipient as `readableSender` shows a sender. */
export function readableRecipients(list: string): string {
  return splitAddresses(list).map(readableSender).join(", ");
}

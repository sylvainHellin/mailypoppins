// A From read as the daemon reads it (`mp_core::parse::mailbox_addresses`):
// once comments are dropped, it is cut into mailboxes at every `,` and `;`
// outside a quoted string, and each names the address in its last `<...>`
// outside a quoted string, else its own address, else its bare tokens holding
// an `@`, never one inside a quoted string but a quoted local part
// (`"john.doe"@x`); Copy sender copies the address a reply goes to, that of
// the first mailbox holding a `<...>`, else the first one's. The daemon
// stores a name quoted whenever it holds syntax (`"Bob <bob@x>" <alice@y>`); a
// row an older build stored may hold it decoded and bare (`bob@x, <alice@y>`),
// so a row, the reader and Copy sender read it here, and name the mailbox a
// block matches and a reply goes to.

/**
 * The index in `raw` of every character outside its comments, honouring
 * quoted strings when `quotes`. A `"` never closed is a literal and what
 * follows it is read with no quoted string, as an unclosed `(` is a literal
 * and what follows it read afresh (`mp_core::parse::uncommented`).
 */
function uncommented(raw: string, quotes: boolean): number[] {
  const kept: number[] = [];
  let depth = 0;
  let opened = 0;
  let quoted = false;
  let escaped = false;
  // Where the quoted string still open starts, and how many indexes were kept before it.
  let quoteAt = 0;
  let quoteKept = 0;
  for (let at = 0; at < raw.length; at++) {
    const c = raw[at];
    if (escaped) {
      escaped = false;
    } else if (c === "\\" && (quoted || depth > 0)) {
      escaped = true;
    } else if (quotes && c === '"' && depth === 0) {
      if (!quoted) {
        quoteAt = at;
        quoteKept = kept.length;
      }
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
  if (quoted) {
    // The `"` never closed: a literal, and what follows is read with no quoted string.
    const rest = uncommented(raw.slice(quoteAt + 1), false);
    return [...kept.slice(0, quoteKept), quoteAt, ...rest.map((at) => quoteAt + 1 + at)];
  }
  if (depth > 0) {
    // The outermost `(` never closed: a literal, and what follows is read afresh.
    const rest = uncommented(raw.slice(opened + 1), quotes);
    return [...kept, opened, ...rest.map((at) => opened + 1 + at)];
  }
  return kept;
}

/**
 * The index of every character of `raw` outside its `(comments)`; an
 * unclosed `(` or `"` is text, as a decoded display name may hold one, and a
 * quoted string closed before such a `"` still holds its `(`.
 */
function uncommentedOffsets(raw: string): number[] {
  return uncommented(raw, true);
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
 * lies in a quoted string; a `"` never closed quotes nothing from it on, and a
 * quoted string closed before it still does. A `<`, `>`, `,`, `;` or `:` is
 * syntax only outside a quoted string (`mp_core::parse::uncommented_chars`).
 */
function uncommentedChars(raw: string): UncommentedChar[] {
  let quoted = false;
  let escaped = false;
  // The index in `chars` of the `"` opening the quoted string still open.
  let opened = 0;
  const chars: UncommentedChar[] = [];
  for (const at of uncommentedOffsets(raw)) {
    const c = raw[at];
    let inside = quoted;
    if (escaped) {
      escaped = false;
      inside = true;
    } else if (quoted && c === "\\") {
      escaped = true;
      inside = true;
    } else if (c === '"') {
      if (!quoted) opened = chars.length;
      quoted = !quoted;
      inside = true;
    }
    chars.push({ at, c, quoted: inside });
  }
  if (quoted) for (const char of chars.slice(opened)) char.quoted = false;
  return chars;
}

/**
 * `name` with its quoted strings opened, a `\` escape in one undone; taken as
 * written from a `"` never closed on (`mp_core::parse::unquoted_display_name`).
 */
function unquotedName(name: string): string {
  let out = "";
  let quoted = false;
  let escaped = false;
  // Where the open quoted string starts, in `name` and in `out`.
  let openedAt = 0;
  let openedOut = 0;
  for (let at = 0; at < name.length; at++) {
    const c = name[at];
    if (escaped) {
      out += c;
      escaped = false;
    } else if (quoted && c === "\\") {
      escaped = true;
    } else if (c === '"') {
      if (!quoted) {
        openedAt = at;
        openedOut = out.length;
      }
      quoted = !quoted;
    } else {
      out += c;
    }
  }
  if (quoted) out = out.slice(0, openedOut) + name.slice(openedAt);
  return out.trim();
}

/**
 * A display name, `name` unquoted and unescaped, with the `address` after it
 * (`mp_core::parse::named_mailbox`). A comment stays in the name
 * (`Alice (Team) <a@x>`). The name is empty when there is none or it only
 * repeats the address, as Graph's often does (`"a@x" <a@x>`).
 */
function namedMailbox(name: string, address: string): { name: string; address: string } {
  const unquoted = unquotedName(name);
  return { name: unquoted.toLowerCase() === address.toLowerCase() ? "" : unquoted, address };
}

/**
 * The display name and address of the mailbox of `from` a reply goes to
 * (`replyMailbox`), the name its text before its `<`: `A <a@x>, B <b@y>` is
 * `A` and `a@x` (`mp_core::parse::display_mailbox`). Null when no mailbox of
 * it holds a `<...>` outside a comment and a quoted string.
 */
function displayMailbox(from: string): { name: string; address: string } | null {
  const m = replyMailbox(from);
  return m ? namedMailbox(from.slice(m.start, m.open), m.address) : null;
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

/** One mailbox of a From (`mailboxParts`). */
interface MailboxPart {
  /** The index in the From its text starts at, its comments included. */
  start: number;
  chars: UncommentedChar[];
}

/**
 * The mailboxes of a From given as `uncommentedChars` answers it: cut at every
 * `,` and `;` outside a quoted string, each with the index its text starts at
 * (`mp_core::parse::mailbox_parts`).
 */
function mailboxParts(chars: UncommentedChar[]): MailboxPart[] {
  const parts: MailboxPart[] = [{ start: 0, chars: [] }];
  for (const char of chars) {
    if (!char.quoted && (char.c === "," || char.c === ";")) parts.push({ start: char.at + 1, chars: [] });
    else parts[parts.length - 1].chars.push(char);
  }
  return parts;
}

/**
 * The last `<...>` of one mailbox outside a quoted string, the `>` searched
 * only after it: the index in the From its `<` is at, and the address it
 * holds, trimmed; null when there is none, it is never closed or it holds
 * nothing (`mp_core::parse::bracketed_in`).
 */
function bracketedIn(part: UncommentedChar[]): { open: number; address: string } | null {
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
  return address ? { open: part[open].at, address } : null;
}

/**
 * The first mailbox of `from` whose last `<...>` names an address: the index
 * its text starts at, that of its `<`, and the address; null when no mailbox
 * holds one (`mp_core::parse::reply_mailbox`).
 */
function replyMailbox(from: string): { start: number; open: number; address: string } | null {
  for (const part of mailboxParts(uncommentedChars(from))) {
    const bracketed = bracketedIn(part.chars);
    if (bracketed) return { start: part.start, ...bracketed };
  }
  return null;
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
    const bracketed = bracketedIn(part);
    return bracketed === null ? [] : [bracketed.address];
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
    for (const address of partAddresses(part.chars)) {
      if (!addresses.includes(address)) addresses.push(address);
    }
  }
  return addresses;
}

/**
 * The address a reply to `from` goes to (`mp_core::parse::extract_email_address`):
 * that of the first mailbox holding a `<...>`, else the first address it names,
 * else the field trimmed. `"Bob <bob@x>" <alice@example.com>` and
 * `x@y, Alice <alice@example.com>` to `alice@example.com`,
 * `x@example.com (Cron Daemon)` to `x@example.com`.
 */
export function senderAddress(from: string): string {
  return replyMailbox(from)?.address ?? mailboxAddresses(from)[0] ?? from.trim();
}

/**
 * A From as a reader is shown it, whole: `Name <address>` with the address
 * its last `<...>` holds and the name everything before it, unquoted and
 * unescaped (`A <a@x>, B <b@y>` is shown as it is), the bare address when
 * there is no name or it repeats the address. For display only: the name is
 * no longer quoted (`mp_core::parse::readable_sender`).
 */
export function readableSender(from: string): string {
  const bracketed = bracketedIn(uncommentedChars(from));
  if (!bracketed) return from.trim();
  const m = namedMailbox(from.slice(0, bracketed.open), bracketed.address);
  return m.name ? `${m.name} <${m.address}>` : m.address;
}

/**
 * A comma-separated address list split at every `,` outside a quoted name and
 * a comment, each part kept whole with its comments; a `(` or `"` never closed
 * is a literal, and a quoted string closed before such a `"` still holds its
 * `,` (`mp_core::addresses::split_addresses`).
 */
export function splitAddresses(list: string): string[] {
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

// A From read as the daemon reads it (`mp_core::parse::mailbox_addresses`):
// once comments are dropped, a header holding a `<` outside a quoted string
// names one mailbox, the address in its last such `<...>`, its display name
// everything before; one with none names its first listed address, else its
// last bare token holding an `@`, never one inside a quoted string. The daemon
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

/**
 * The addresses of a From with no `<` outside a quoted string read as a list
 * (`mp_core::parse::listed_addresses`): the text outside quoted strings cut at
 * every `,` and `;`, each part with what comes up to its last `:` dropped (a
 * group's name), trimmed and any `"` of a quote never closed trimmed off, the
 * empty parts dropped; null when there is no member, a member holds no `@` or
 * holds whitespace, or a group's name holds an `@`.
 */
function listedAddresses(chars: UncommentedChar[]): string[] | null {
  const members: string[] = [];
  let part = "";
  const push = () => {
    const member = part.trim().replace(/^"+|"+$/g, "");
    if (member) members.push(member);
    part = "";
  };
  for (const { c, quoted } of chars) {
    if (quoted) {
      part += " ";
    } else if (c === "," || c === ";") {
      push();
    } else if (c === ":") {
      if (part.includes("@")) return null;
      part = "";
    } else {
      part += c;
    }
  }
  push();
  const listed = members.length > 0 && members.every((member) => member.includes("@") && !/\s/.test(member));
  return listed ? members : null;
}

/**
 * The address of a From with no `<...>`, as `mailbox_addresses` reads it
 * first: comments dropped, its first listed address (`a@x, b@y`, a group's
 * members), else its last bare token holding an `@` outside a quoted string;
 * null when it holds none, or a `<` outside a quoted string (`John <john@x`,
 * which names no mailbox).
 */
function bareAddress(from: string): string | null {
  const chars = uncommentedChars(from);
  if (chars.some((char) => char.c === "<" && !char.quoted)) return null;
  const listed = listedAddresses(chars);
  if (listed) return listed[0];
  const tokens = chars
    .map((char) => (char.quoted ? " " : char.c))
    .join("")
    .split(/[\s,;]+/)
    .map((token) => token.replace(/^["<>]+|["<>]+$/g, ""))
    .filter((token) => token.includes("@"));
  return tokens.length > 0 ? tokens[tokens.length - 1] : null;
}

/**
 * `"Bob <bob@x>" <alice@example.com>` to `alice@example.com`,
 * `x@example.com (Cron Daemon)` to `x@example.com`; a field naming no address stays, trimmed.
 */
export function senderAddress(from: string): string {
  return displayMailbox(from)?.address ?? bareAddress(from) ?? from.trim();
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
 * A comma-separated address list split outside quoted names, a `\` in one
 * escaping the character after it (`mp_core::addresses::split_addresses`).
 */
function splitAddresses(list: string): string[] {
  const parts: string[] = [];
  let current = "";
  let quoted = false;
  let escaped = false;
  for (const c of list) {
    if (escaped) {
      escaped = false;
    } else if (quoted && c === "\\") {
      escaped = true;
    } else if (c === '"') {
      quoted = !quoted;
    }
    if (c === "," && !quoted) {
      if (current.trim()) parts.push(current.trim());
      current = "";
    } else {
      current += c;
    }
  }
  if (current.trim()) parts.push(current.trim());
  return parts;
}

/** A To or Cc as a reader is shown it: each recipient as `readableSender` shows a sender. */
export function readableRecipients(list: string): string {
  return splitAddresses(list).map(readableSender).join(", ");
}

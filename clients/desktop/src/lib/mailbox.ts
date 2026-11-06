// A From read as the daemon reads it (`mp_core::parse::mailbox_addresses`):
// once comments are dropped, a header holding a `<` names one mailbox, the
// address in its last `<...>`, its display name everything before. The store
// holds a From decoded and not always quoted again (`"Bob <bob@x>" <alice@y>`,
// `bob@x, <alice@y>`), so a row, the reader and Copy sender read it here, and
// name the mailbox a block matches and a reply goes to.

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
 * holds no `<...>` outside a comment. The address is read without comments,
 * the name keeps them (`Alice (Team) <a@x>`). The name is empty when there is
 * none or it only repeats the address, as Graph's often does (`"a@x" <a@x>`).
 */
function displayMailbox(from: string): { name: string; address: string } | null {
  const kept = uncommentedOffsets(from);
  let open = -1;
  for (let i = kept.length - 1; i >= 0; i--) {
    if (from[kept[i]] === "<") {
      open = i;
      break;
    }
  }
  if (open < 0) return null;
  // `>` is searched only after the `<`: a name may hold one (`Alerts > Prod <a@x>`).
  let close = -1;
  for (let i = open + 1; i < kept.length; i++) {
    if (from[kept[i]] === ">") {
      close = i;
      break;
    }
  }
  if (close < 0) return null;
  const address = kept
    .slice(open + 1, close)
    .map((at) => from[at])
    .join("")
    .trim();
  if (!address) return null;
  const name = unquotedName(from.slice(0, kept[open]));
  return { name: name.toLowerCase() === address.toLowerCase() ? "" : name, address };
}

/** `"Doe, Jane" <jane@example.com>` to `Doe, Jane`; a bare address stays. */
export function senderName(from: string): string {
  const f = from.trim();
  if (!f) return "(no sender)";
  const m = displayMailbox(f);
  return m ? m.name || m.address : f;
}

/** `"Bob <bob@x>" <alice@example.com>` to `alice@example.com`; a field with no `<...>` stays, trimmed. */
export function senderAddress(from: string): string {
  return displayMailbox(from)?.address ?? from.trim();
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

// Generate the command reference page from `mp help`.
//
// The source of truth for every command and flag is clap's help output, so
// the page is rebuilt from it rather than written by hand:
//
//   pnpm gen:commands                        # uses `mp` on PATH
//   MP_BIN=../target/debug/mp pnpm gen:commands
//
// Writes src/content/docs/reference/commands.md. Commit the result.

import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const MP = process.env.MP_BIN ?? 'mp';
const here = dirname(fileURLToPath(import.meta.url));
const OUT = join(here, '..', 'src', 'content', 'docs', 'reference', 'commands.md');

// Commands grouped for the page's table of contents. A command missing from
// this map still appears, under "Other", so a new command is never dropped.
const GROUPS: [string, string[]][] = [
  ['Drafts', ['new', 'list', 'validate', 'edit', 'path', 'mark-approved', 'mark-draft', 'reply', 'forward']],
  ['Sending', ['send', 'send-approved', 'outbox']],
  ['Reading and syncing', ['sync', 'list-messages', 'show', 'search', 'fetch', 'watch', 'list-mailboxes']],
  ['Acting on received mail', ['archive', 'delete', 'open', 'save']],
  ['Contacts and calendar', ['contacts', 'invite', 'calendar']],
  ['Configuration and accounts', ['config', 'account']],
  ['Daemon and automation', ['daemon', 'hooks', 'completions']],
  ['Maintenance', ['store', 'cutover', 'dump-keys', 'dump-mailbox']],
];

type Entry = { name: string; desc: string };
type Help = {
  about: string[];
  usage: string;
  sections: Map<string, Entry[]>;
};

function run(args: string[]): string {
  return execFileSync(MP, args, { encoding: 'utf8', env: { ...process.env, NO_COLOR: '1' } });
}

/** Parse clap's `--help` output, short or long layout. */
function parseHelp(text: string): Help {
  const lines = text.replace(/\s+$/, '').split('\n');
  const usageAt = lines.findIndex((l) => l.startsWith('Usage: '));
  if (usageAt < 0) throw new Error(`no Usage line in:\n${text}`);
  const about = lines.slice(0, usageAt);
  const usage = lines[usageAt].slice('Usage: '.length).trim();
  const sections = new Map<string, Entry[]>();
  let current: Entry[] | null = null;
  let last: Entry | null = null;
  for (const line of lines.slice(usageAt + 1)) {
    const header = line.match(/^([A-Z][A-Za-z ]*):$/);
    if (header) {
      current = [];
      sections.set(header[1], current);
      last = null;
      continue;
    }
    if (!current) continue;
    if (line.trim() === '') continue;
    // An entry starts at column 2, or at column 6 for a long-only flag;
    // the long layout's description lines are indented further.
    const item = line.match(/^(?: {2}(?=\S)| {6}(?=--))(\S.*?)(?: {2,}(\S.*))?$/);
    if (item) {
      last = { name: item[1].trim(), desc: (item[2] ?? '').trim() };
      current.push(last);
    } else if (last) {
      last.desc = `${last.desc} ${line.trim()}`.trim();
    }
  }
  for (const entries of sections.values()) for (const e of entries) e.desc = tidy(e.desc);
  return { about, usage, sections };
}

/** clap's trailing value notes as prose: `[default: 10]` reads "Default: `10`." */
function tidy(desc: string): string {
  const notes: string[] = [];
  const base = desc
    .replace(/\s*\[default: \]/g, '')
    .replace(/\s*\[default: ([^\]]+)\]/g, (_, v) => (notes.push(`Default: \`${v}\`.`), ''))
    .replace(/\s*\[possible values: ([^\]]+)\]/g, (_, v) => {
      notes.push(`Values: ${v.split(', ').map((x: string) => `\`${x}\``).join(', ')}.`);
      return '';
    })
    .trim();
  if (notes.length === 0) return base;
  if (base === '') return notes.join(' ');
  return `${base}${/[.!?]$/.test(base) ? '' : '.'} ${notes.join(' ')}`;
}

/**
 * Escape Markdown outside backtick code spans and keep the spans as written.
 * A bare `--flag` in the prose becomes a code span, so it reads as a flag.
 */
function md(text: string): string {
  return text
    .replace(/(`[^`]*`)|(?<![\w`-])(--[a-z][a-z0-9-]*)/g, (m, span, flag) => (span ? span : `\`${flag}\``))
    .split(/(`[^`]*`)/)
    .map((part, i) =>
      i % 2 === 1
        ? part.replace(/\|/g, '\\|')
        : part
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/\\/g, '\\\\')
            .replace(/([*_|[\]])/g, '\\$1'),
    )
    .join('');
}

function code(text: string): string {
  return `\`${text.replace(/\|/g, '\\|')}\``;
}

/**
 * The about text. A paragraph with aligned columns or indented lines (a
 * grammar table, examples) stays verbatim in a text block; prose stays prose.
 */
function aboutBlock(about: string[]): string {
  const paras: string[][] = [];
  let buf: string[] = [];
  for (const l of about) {
    if (l.trim() === '') {
      if (buf.length) paras.push(buf);
      buf = [];
    } else buf.push(l);
  }
  if (buf.length) paras.push(buf);
  const out: string[] = [];
  let verbatim: string[][] = [];
  const flush = () => {
    if (verbatim.length) out.push('```text\n' + verbatim.map((p) => p.join('\n')).join('\n\n') + '\n```');
    verbatim = [];
  };
  for (const p of paras) {
    const preformatted = p.some((l) => /^\s/.test(l) || /\S {2,}\S/.test(l)) || /:$/.test(p[0]);
    if (preformatted) verbatim.push(p);
    else {
      flush();
      out.push(md(p.map((l) => l.trim()).join(' ').replace(/\/ /g, '/')));
    }
  }
  flush();
  return out.join('\n\n');
}

function table(head: string, rows: Entry[], anchor?: (name: string) => string): string {
  if (rows.length === 0) return '';
  const cell = (name: string) => (anchor ? `[${code(name)}](#${anchor(name)})` : code(name));
  const body = rows.map((r) => `| ${cell(r.name)} | ${md(r.desc)} |`).join('\n');
  return `| ${head} | Description |\n| --- | --- |\n${body}`;
}

const top = parseHelp(run(['help']));
const globalNames = new Set(
  (top.sections.get('Options') ?? []).map((o) => o.name).filter((n) => !n.includes('--version')),
);
const commands = (top.sections.get('Commands') ?? []).filter((c) => c.name !== 'help');

function commandBlock(path: string[], level: number): string {
  const help = parseHelp(run([...path, '--help']));
  const hashes = '#'.repeat(level);
  const parts: string[] = [`${hashes} mp ${path.join(' ')}`];
  const about = aboutBlock(help.about);
  if (about) parts.push(about);
  parts.push('```sh\n' + help.usage + '\n```');
  const args = help.sections.get('Arguments') ?? [];
  const opts = (help.sections.get('Options') ?? []).filter((o) => !globalNames.has(o.name));
  const subs = (help.sections.get('Commands') ?? []).filter((c) => c.name !== 'help');
  if (subs.length) parts.push(table('Subcommand', subs, (n) => ['mp', ...path, n].join('-')));
  if (args.length) parts.push(table('Argument', args));
  if (opts.length) parts.push(table('Option', opts));
  for (const s of subs) parts.push(commandBlock([...path, s.name], Math.min(level + 1, 6)));
  return parts.filter(Boolean).join('\n\n');
}

const known = new Set(GROUPS.flatMap(([, names]) => names));
const present = new Set(commands.map((c) => c.name));
const groups = GROUPS.map(([title, names]) => [title, names.filter((n) => present.has(n))] as [string, string[]]);
const other = commands.map((c) => c.name).filter((n) => !known.has(n));
if (other.length) groups.push(['Other', other]);
for (const n of known) if (!present.has(n)) console.warn(`gen-commands: '${n}' is grouped but no longer exists`);

const version = run(['--version']).trim();
const summary = commands
  .map((c) => `| [${code(`mp ${c.name}`)}](#mp-${c.name}) | ${md(c.desc)} |`)
  .join('\n');

const frontmatter = `---
title: Commands
description: Every mp command and flag, generated from mp help.
---
`;

const body = [
  `<!-- Generated by website/scripts/gen-commands.ts from \`mp help\` (${version}). Do not edit by hand: run \`pnpm gen:commands\`. -->`,
  `Generated from \`mp help\` of ${md(version)}. Run \`mp <command> --help\` for the same text in your terminal.`,
  `Running \`mp\` with no command opens the terminal UI. Running \`mp <selector>\` previews a draft without sending it.`,
  '## Global options',
  'These work with every command.',
  table('Option', (top.sections.get('Options') ?? []).filter((o) => globalNames.has(o.name) && !o.name.includes('--help'))),
  '## All commands',
  `| Command | Description |\n| --- | --- |\n${summary}`,
  ...groups.flatMap(([title, names]) => [`## ${title}`, ...names.map((n) => commandBlock([n], 3))]),
];

const out = frontmatter + '\n' + body.join('\n\n') + '\n';
writeFileSync(OUT, out);
console.log(`gen-commands: wrote ${OUT} (${commands.length} commands, ${version})`);

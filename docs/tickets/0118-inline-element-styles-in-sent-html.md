---
id: 0118
title: Stamp the style block onto every element so Apple Mail stops dropping the body font
type: bug
priority: now
status: done
created: 2026-09-14
---

## Problem

A sent message with an unordered list renders with the list items at the configured
font size and every paragraph a size smaller in the Apple Mail reading pane.
Outlook renders the same message uniformly.

Reproduced from the stored Sent copy of
`<PVwnWTagKVBEukJerjLAvh8zH2CpRu7ztfuS@sylvains-MacBook-Pro-TUM.local>`.
The emitted HTML is well-formed and the `<ul>` sits inside the styled wrapper
`<div>`; WebKit renders it uniformly when given the message as-is, in quirks
mode, and inside a document carrying Mail's own
`MUIWebDocument.css`. Measured from the screenshot: list items at 16px
(the configured `12pt`), paragraphs and signature at 12.8px, and the wrapper's
`line-height: 1.6` applied nowhere.

So Apple Mail keeps the wrapper's inherited font for the list and drops it for
the paragraphs. The defect is ours: the whole message leans on inheritance from a
single wrapper `<div>` plus a `<head><style>` block, and clients break both.
`docs/lessons-learned.md` already records the same class of failure, when the
signature rendered larger than the body and the inline wrapper was added to fix it.
Mail now breaks inheritance one level deeper.

## Approach

Stamp the declarations of the `<head><style>` block onto every element the
Markdown converter emits, as an inline `style` attribute, instead of relying on
inheritance. The `<style>` block and the wrapper `<div>` stay for the clients
that honour them.

Only the Markdown-derived fragments are stamped. The quoted original HTML of a
reply or forward keeps its own styling verbatim.

Switch the default `font_size` from `12pt` to `16px` (the same computed size).
`pt` units are mangled by some clients and remove nothing by staying.

## Acceptance criteria

- `<p>`, `<ul>`, `<ol>`, `<li>`, `<blockquote>`, `<h1>`-`<h6>` and table elements
  in the sent HTML each carry the font family, size and line height.
- An existing `style` attribute (pulldown-cmark emits `text-align` on aligned
  table cells) is preserved and still wins.
- `<pre>` and `<code>` keep their monospace family and take only size and line height.
- The quoted section of a reply is byte-identical to what it was.
- The `{{SIGNATURE}}` marker still splits the reply from the quote.

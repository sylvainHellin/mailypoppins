---
id: 0146
title: Graph search fails with InefficientFilter when a query mixes free text, an address and a date
type: bug
priority: next
status: open
created: 2026-10-21
---

## Symptom

On a Microsoft Graph account (`evoqs`), `mp -A evoqs search 'from:ovh mailypoppins after:2026-10-01'` fails:

```
Error: Graph search (filter) failed (HTTP 400 Bad Request): {"error":{"code":"InefficientFilter","message":"The restriction or sort order is too complex for this operation."}}
```

The same query on an IMAP account (`proton`) returns results.

## Cause, as read from the code

Three separate defects stack up in `GraphClient::search_messages` (`src/graph.rs`) and `to_graph` (`crates/mp-core/src/search.rs`).

1. The first request sends `$search` together with `$filter` and `$orderby=receivedDateTime desc` (`search_url`). Graph does not accept `$orderby` with `$search` on messages, and supports `$search` with `$filter` only in narrow cases, so the request fails and the code falls back to `search_messages_filter_only`.
2. The fallback keeps `$orderby=receivedDateTime desc` while the filter is `from/emailAddress/address eq 'ovh' and receivedDateTime ge 2026-10-01`. Graph requires every `$orderby` property to appear in `$filter` first and in the same order, so a filter that does not start with `receivedDateTime` gets `InefficientFilter`. This is the error the user sees.
3. The fallback silently drops the free-text half (`mailypoppins`), so even when it succeeds it returns a wider set than the query asked for.

A fourth mismatch with IMAP behaviour: `from:ovh` renders as `eq 'ovh'`, an exact address match, so it never matches `support@services.ovhcloud.com`; IMAP `FROM` is a substring match. The search grammar promises one meaning on every backend.

## Proposed approach

- Never send `$orderby` together with `$search`; sort the returned page client-side by `receivedDateTime` instead.
- In a filter-only request, put the `receivedDateTime` clause first in `$filter` whenever `$orderby=receivedDateTime` is sent, or drop `$orderby` and sort client-side.
- When a query has both halves, send the free text as `$search` and apply the address, date and attachment terms client-side to the returned page (or as KQL `from:`, `received>=` inside `$search`, which Graph supports), rather than falling back and losing the free text.
- Render `from:` / `to:` / `cc:` with a partial value (no `@`) as KQL `from:ovh` inside `$search`, which matches on substrings, and keep `eq` only for a full address.
- Do not fall back silently: if a half of the query cannot be honoured, say so in the error.

## Acceptance criteria

- `mp -A evoqs search 'from:ovh mailypoppins after:2026-10-01'` returns the same messages as the IMAP account would for the same mailbox content.
- `from:<partial>` matches on Graph as it does on IMAP.
- Unit tests on `to_graph` and `search_url` pin the parameter combinations: no `$orderby` beside `$search`, `receivedDateTime` first in a filter sorted by it.
- A live check on a Graph account for each of: free text only, address only, date only, address plus date, free text plus address plus date.

## Context

Found on 2026-10-21 while looking for an OVH invoice during the Partner Center enrolment (`.agents/handoff/2026-10-21-graph-evoqs-setup.md`).

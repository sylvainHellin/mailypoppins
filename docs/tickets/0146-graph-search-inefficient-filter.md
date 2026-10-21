---
id: 0146
title: Graph server search fails or returns unfiltered results (InefficientFilter, the $search fallback, in:sent)
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
- `mp -A evoqs search 'subject:"<word>"'` returns only messages whose subject contains the word.
- `mp -A evoqs search 'message-id:<id>'` returns the one message.
- `mp -A evoqs search 'in:sent <word>'` searches Sent Items.

## Further findings from the PERSO-144 live test (2026-10-21)

The live test on the EVOQS account (`.agents/research/2026-10-21-perso-144-live-test.md`, items 3 to 5) and a second session the same day widened the scope: no Graph server search works reliably, whatever the query.

- Defect 1 hits every query with a `$search` half, not only mixed ones: each search in the test logged `Graph $search failed (HTTP 400), falling back to $filter only`, so `$search` has not succeeded once on this account.
- Defect 3 is worse than a wider result set when every term of a query lands in `$search` (`subject:`, `body:`, `filename:`, free text): the fallback then has no `$filter` at all and lists the folder's newest `limit` messages.
  `mp -A evoqs search 'subject:"PERSO-144"'` returned 20 rows, most of them unrelated (for example "Rechnung für die Bestellung #1478103"), and no error.
  Every subject, body and free-text search on a Graph account is therefore wrong today.
- Defect 2 hits every filter-only query, not only mixed ones.
  In another session the same day, every server search on the evoqs account failed with HTTP 400 `InefficientFilter`, a bare `mp -A evoqs search 'from:amazon'` included.
  `from:amazon` renders as `from/emailAddress/address eq 'amazon'`, and `search_url` sends `$orderby=receivedDateTime desc` beside it, so the filter does not start with `receivedDateTime` and Graph refuses it.
- `message-id:` alone fails with `InefficientFilter`.
  `to_graph` renders it as the whole `$filter` (`internetMessageId eq '<...>'`) and `search_url` adds `$orderby=receivedDateTime desc`, which is defect 2 on a single clause.
  `message_lookup_url` in `src/graph.rs` sends the same filter without `$orderby`, which Graph accepts.
- A fifth defect: `in:sent` fails with Graph `ErrorInvalidIdMalformed`, while `in:"Sent Items"` works.
  The CLI's server leg passes the `in:` value to `GraphClient::search_messages` unresolved (`src/main.rs`, `mailbox_name` around line 4383).
  `folder_path` then finds `sent` neither in `well_known_folder` (`src/graph.rs`, line 549, which knows `sentitems` and `sent items` but none of mp's role names) nor among the top-level folders, and sends it as a folder id.
  The daemon's `message.search_server` resolves names against the configured mailboxes (`pick_target`), so the fix is to resolve a role name to the account's configured server folder in the CLI leg as well, or to teach `well_known_folder` the role names `sent`, `trash` and `junk`.

## Context

Found on 2026-10-21 while looking for an OVH invoice during the Partner Center enrolment (`.agents/handoff/2026-10-21-graph-evoqs-setup.md`).

# Decision log

Durable decisions settled with Sylvain, newest at the bottom.
Architecture records with their own evidence live under `docs/baselines/decisions/`.

## 2026-10-19 - The conversation list filter matches a conversation by its participants and newest message only

Why: members carry no subject of their own and the five listed senders cover most cases; the gap is a conversation whose only match is an older member from a sixth sender.
Rejected: matching expanded members as well (results would depend on what is expanded); having the daemon return every member's sender and subject for filtering (complete, but a protocol change for a rare case).
Source: chat, PERSO-146 follow-ups.
Status: accepted

## 2026-10-19 - The Graph sync backend stays parked; Graph threading and Graph reply headers wait for it

Why: IMAP covers both accounts in daily use, so PERSO-144 (Graph sends carry no In-Reply-To or References) and PERSO-147 (thread Graph and Gmail rows by server conversation id) are postponed rather than unparking #0042 and #0055 for them.
Rejected: unparking the Graph backend now.
Source: chat.
Status: superseded by 2026-10-21

## 2026-10-21 - The Graph backend is unparked with the EVOQS Exchange account as its first live account

Why: EVOQS mail stays on Microsoft 365 (the Proton move, PERSO-73, is cancelled), Sylvain administers that tenant and can consent to Graph himself, and the Graph paths have never met a live server, which only a real account fixes; the gaps it shows (no scheduled tick, no reply headers on Graph sends, no Graph threading, no invitation answers) are unbuilt, not inherent to Graph, and PERSO-144 comes early.
Rejected: OAuth2 over IMAP and SMTP for EVOQS (works with the tested path today but leaves Graph untested); Exchange Web Services (being retired in Exchange Online).
Source: chat.
Status: accepted

## 2026-10-21 - mailypoppins gets its own Microsoft Entra tenant and one multi-tenant app registration, published by EVOQS GmbH

Why: an app registration cannot move between tenants, so a dedicated tenant keeps the client ID with mailypoppins whatever happens to EVOQS; one registration for any Entra tenant plus personal Microsoft accounts lets users sign in without registering their own app, and EVOQS GmbH is the legal entity for publisher verification.
Rejected: registering the app inside the EVOQS tenant (fastest, but tied to EVOQS for good); every user registering their own app (no verification work, but the whole portal procedure for each user).
Source: chat.
Status: accepted

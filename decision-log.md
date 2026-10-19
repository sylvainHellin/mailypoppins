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
Status: accepted

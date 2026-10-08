---
title: Selectors
description: How mp:// selectors name a received message or a draft.
---

Every command that names a message takes a selector, never a file path.

```text
mp://<account>/<mailbox>/<key>
```

| Part | Received mail | Drafts |
| --- | --- | --- |
| `account` | The account's `name` | The account's `name` |
| `mailbox` | The mailbox role (`inbox`, `archive`, `sent`) or an extra mailbox's server name | Always `drafts` |
| `key` | The Message-ID, without its angle brackets | The `id:` field of the draft |

A draft's selector does not change when you rename its file.

## Short forms

The leading parts can be left off.
The account falls back to `-A` or the first account in your configuration.
The mailbox falls back to `--mailbox` or the command's own scope.

These three are the same command:

```sh
mp send mp://work/drafts/9f2c1ab30d5e4471
mp -A work send drafts/9f2c1ab30d5e4471
mp -A work send 9f2c1ab30d5e4471
```

A key that matches messages in two mailboxes is reported with both full selectors, rather than resolved by guesswork.
`--mailbox` picks one.

## Where selectors come from

Every command that creates or lists something prints the full form, which pastes straight into the next command:

- `mp new`, `mp reply` and `mp forward` print the new draft's selector.
- `mp list` lists drafts, and `mp list-messages` lists received mail, each with its selector.
- <kbd>y</kbd> in the terminal UI copies the selector of the message under the cursor.

`mp path <selector>` is the one way back from a draft's selector to its file.

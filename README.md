# mailypoppins

I live in the terminal and write everything in Neovim, except my email.
No mainstream client let me read and edit a message in Neovim, and the system-wide Vim layers on the Mac never worked well.
I also wanted my AI agents to help with email: tell me which messages still wait for an answer, and check my drafts for typos, missing points and language before I send them.
With Copilot and the AI extensions of existing clients, that meant pasting my text into a chat, pasting the corrections back, and never seeing what the agent had changed.
So I wrote my own client.
In mailypoppins every draft is a Markdown file that I write in Neovim and my agent edits in place.
Because it is a plain file, I see the agent's changes as a diff, and nothing is sent before I approve it.

![The mailypoppins desktop app](docs/screenshots/desktop-app.png)

## How it works

mailypoppins is a daemon with three clients.
The daemon owns the accounts, syncs mail into a local store and runs the send queue.
The clients talk to it over a Unix socket and offer the same operations:

- the `mp` command line, for scripts and AI agents;
- the terminal UI, which `mp` opens when run without arguments;
- the desktop app for macOS.

A change made in one client shows up in the others at once.

## Features

- IMAP and SMTP accounts, and Microsoft 365 through OAuth2 or the Graph API.
- Drafts as Markdown files with YAML frontmatter, opened in your `$EDITOR`; the desktop app runs Neovim inside its window.
- An approval step before sending, an undo-send window and a durable outbox.
- Conversations that gather a thread from Inbox, Sent, Archive and Drafts.
- One search syntax for the server and the local store.
- Calendar invitations: send them, answer them and see them in an agenda.
- A contacts index built from your mail, signatures as Markdown files, and hooks that run a command on matching incoming mail.
- Vim keys, a which-key popup and a command palette in the terminal UI and the desktop app.

## Getting started

Install the command line and the terminal UI with Homebrew, on macOS or Linux:

```sh
brew tap sylvainHellin/mailypoppins
brew install mailypoppins
```

The desktop app is a DMG for Apple silicon on the [releases page](https://github.com/sylvainHellin/mailypoppins/releases); it carries its own `mp`.
The app is not signed yet, so macOS blocks its first launch: the [getting started guide](https://mailypoppins.com/getting-started/#mac-app) shows how to allow it and how to use the app's `mp` from a terminal.
To build the command line and the terminal UI from source, run `cargo install --path .` in a clone; [clients/desktop/README.md](clients/desktop/README.md) covers the app.

Then add an account and open the terminal UI:

```sh
mp config init
mp
```

## Documentation

- [mailypoppins.com](https://mailypoppins.com) covers [getting started](https://mailypoppins.com/getting-started/), the [commands](https://mailypoppins.com/commands/), the [configuration](https://mailypoppins.com/config/), the [draft format](https://mailypoppins.com/draft-format/) and an [FAQ](https://mailypoppins.com/faq/).
- `mp --help` lists every command, and `?` in the terminal UI every key.
- [docs/](docs/) holds the architecture, the daemon protocol, the secrets model and the release process.
- [clients/desktop/README.md](clients/desktop/README.md) covers the desktop app's build.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE)

# gmail-tui

A minimal Gmail client for the terminal. Browse your mailboxes, read messages, mark them
read or unread, search a folder, and send mail — all from the keyboard, without leaving
the shell.

It talks to Gmail directly over IMAP (reading) and SMTP (sending) using a Gmail **app
password**. There is no browser, no OAuth dance, and no server in the middle.

## Features

- Three-pane layout: mailboxes, message list, and reading pane
- Unread counts per folder, with Gmail's folders (Inbox, Starred, Sent, …) ordered first
- Plain-text reading pane; `text/html` messages are reduced to text automatically
- Compose and send messages (Gmail files them into **Sent** for you)
- Mark messages read / unread
- Search within the current folder via IMAP `SEARCH`
- Remembers your address and credential backend between runs
- Automatic reconnect with backoff if the connection drops

## Requirements

- **Rust 1.85 or newer** (this crate uses the 2024 edition) to build from source.
- A **Gmail account with 2-Step Verification enabled** — app passwords can only be created
  on such accounts.
- On Linux, the OpenSSL development headers are needed to build: install `libssl-dev` and
  `pkg-config` (or your distribution's equivalents). Windows and macOS need nothing extra.

## Installation

Build from a checkout of this repository:

```sh
cargo build --release
```

The binary is written to `target/release/gmail-tui`. Copy it somewhere on your `PATH`, or
run it in place with `cargo run --release --`.

## Setup

Gmail does not accept your normal account password over IMAP. You need an **app password**:

1. Turn on 2-Step Verification for your Google account if it is not already on.
2. Visit <https://myaccount.google.com/apppasswords> and create an app password.
3. Start the client with your address:

   ```sh
   gmail-tui --user you@gmail.com
   ```

4. When prompted, paste the app password. Google shows it as four space-separated groups;
   the spaces are stripped for you, so paste it either way.

By default the password is saved to your operating system's credential store, so you are
only asked once. The chosen backend and your address are remembered in `config.toml`.

## Command-line options

```
gmail-tui [OPTIONS]

  --user <EMAIL>        Gmail address to sign in as (remembered in config.toml)
  --store <BACKEND>     Where to keep the app password (remembered):
                          auto   (default) OS keyring, else encrypted file
                          os     OS keyring only
                          file   passphrase-encrypted file only
                          prompt ask every launch, store nothing
  --forget[=<TARGET>]   Delete the stored credential and exit
                          os | file | all (default: all)
  -h, --help            Show this message
```

Both `--flag value` and `--flag=value` forms are accepted.

### Where credentials are stored

| Backend  | What it does                                                                 |
|----------|------------------------------------------------------------------------------|
| `auto`   | Tries the OS keyring first; falls back to the encrypted file if unusable.     |
| `os`     | OS keyring only (Windows Credential Manager, macOS Keychain, Linux Secret Service). Fails if the keyring is unavailable. |
| `file`   | A passphrase-encrypted file. You choose a passphrase (at least 8 characters) and are asked for it once per launch. |
| `prompt` | Asks for the app password every launch and stores nothing.                    |

`auto` is the sensible default. On headless Linux, in containers, or under WSL, the
keyring often reports itself as *present* but does not actually work — `auto` detects this
and quietly uses the encrypted file instead.

To remove a stored credential:

```sh
gmail-tui --forget          # delete from both the keyring and the file
gmail-tui --forget=os       # keyring only
gmail-tui --forget=file     # encrypted file only
```

## Environment variables

| Variable     | Purpose                                                            |
|--------------|--------------------------------------------------------------------|
| `GMAIL_USER` | Gmail address, used when `config.toml` has none.                    |
| `GMAIL_PASS` | App password for this run only. **Never written to disk**, and overrides any stored credential. |

Set both to run non-interactively (for example, over SSH or in a script) without any
stored credential:

```sh
GMAIL_USER=you@gmail.com GMAIL_PASS='abcd efgh ijkl mnop' gmail-tui
```

## Files

| File              | Contents                                        |
|-------------------|-------------------------------------------------|
| `config.toml`     | Your Gmail address and credential-backend choice |
| `credentials.enc` | The app password, encrypted with your passphrase (only used by the `file` backend) |

Their locations follow each platform's conventions:

| Platform | `config.toml`                        | `credentials.enc`                     |
|----------|--------------------------------------|---------------------------------------|
| Windows  | `%APPDATA%\gmail-tui\config.toml`    | `%LOCALAPPDATA%\gmail-tui\credentials.enc` |
| macOS    | `~/Library/Application Support/gmail-tui/config.toml` | same directory        |
| Linux    | `~/.config/gmail-tui/config.toml`    | same directory                        |

On Windows the encrypted file is deliberately kept out of the roaming profile, so a
credential never travels between machines.

## Keyboard shortcuts

The bottom bar always shows the keys that matter for the current screen and pane. Press
`Tab` to move focus between the mailboxes, the message list, and the reading pane; the
focused pane has a yellow border.

### Normal mode

| Key                | Action                                            |
|--------------------|---------------------------------------------------|
| `↑` / `k`          | Move up (folder, message, or scroll the reader)   |
| `↓` / `j`          | Move down                                         |
| `Tab`              | Cycle focus between mailboxes, messages, body     |
| `Enter`            | Open the focused item / (re)load the message      |
| `c`                | Compose a new message                             |
| `/`                | Search the current folder                         |
| `r`                | Refresh the focused pane                          |
| `m`                | Toggle read / unread on the selected message      |
| `PgUp` / `PgDn`    | Scroll the reading pane one page                  |
| `g` / `Home`       | Jump to the top of the message                    |
| `G` / `End`        | Jump to the bottom of the message                 |
| `q` / `Ctrl+C`     | Quit                                              |

### Compose mode

| Key        | Action                                    |
|------------|-------------------------------------------|
| `Tab`      | Move to the next field (To / Subject / Body) |
| `Shift+Tab`| Move to the previous field                |
| `Ctrl+S`   | Send                                      |
| `Esc`      | Close the composer / discard the draft    |

The message body uses a full text editor: arrow keys, selection, and undo all work as you
would expect.

### Search mode

| Key     | Action              |
|---------|---------------------|
| `Enter` | Run the search      |
| `Esc`   | Cancel and close    |

## Behaviour and limits

- Opening a message does **not** mark it read. Only pressing `m` changes the read/unread
  flag, so you can browse without disturbing your unread counts.
- Each folder shows its **50 newest** messages, and search returns at most 50 hits.
- Messages are shown as plain text. HTML is converted to text with formatting dropped,
  and attachments are neither downloaded nor displayed.
- Reading and sending only — there is no delete, archive, star, or label management.
- Gmail permits only a small number of simultaneous IMAP connections and locks the account
  for about a day if that is abused. The client reconnects gently with a backoff and never
  hammers the server, so leave it running rather than restarting it repeatedly.

## Troubleshooting

**"Gmail rejected the app password."**
Regenerate one at <https://myaccount.google.com/apppasswords>, then run
`gmail-tui --forget` and start again.

**"The OS credential store is unavailable."**
On headless Linux, in containers, or under WSL the Secret Service cannot be reached. Use
`--store=file` or `--store=prompt`, or set `GMAIL_PASS` for the run. (`auto` already falls
back to the encrypted file on its own.)

**"Stdin is not a terminal, so it cannot be asked for."**
You are running without a TTY (a script, SSH without `-t`, or CI). Provide `GMAIL_USER` and
`GMAIL_PASS` so nothing needs to prompt.

**"Could not find system library 'openssl'."**
A Linux build is missing the OpenSSL development files. Install `libssl-dev` and
`pkg-config`.

# AGENTS.md

`gmail-tui` is a single-binary Rust terminal client for Gmail (IMAP read + SMTP send).
Edition 2024 (needs Rust >= 1.85); no workspace, no library target, no CI.

## Commands

- Build: `cargo build` / `cargo build --release`
- Test: `cargo test` — 50 pure unit tests, fully offline (no Gmail account or network needed)
- Single test: `cargo test <substring>` (e.g. `cargo test pick_text_section`)
- Lint: `cargo clippy --all-targets` (currently warning-clean)
- **Do not run `cargo fmt`.** The tree is deliberately hand-formatted and is *not* rustfmt-clean —
  every file in `src/` differs from `rustfmt`. Running it rewrites essentially the whole repo and
  buries your change. Match the surrounding style by hand.

Windows/PowerShell note: `cargo` writes progress to stderr, which PowerShell surfaces as a
`NativeCommandError` with a non-zero `$LASTEXITCODE` even when everything passed. Trust the
`test result: ok` line, not the exit code.

## Architecture

Everything is wired in `src/main.rs`; read it first. Two rules drive the design:

1. **The UI never does I/O.** `app.rs` + `ui/` only mutate in-memory state. They talk to the single
   background worker in `net.rs` over two mpsc channels: `NetCmd` (UI -> net) and `NetMsg`
   (net -> UI). `AppState` is the only writer of UI state, so no lock is ever held across `.await`.
   Keep it that way — never add blocking/network calls to the render path.
2. **Anything that prompts or blocks runs before `ratatui::init()` and before tokio.** Args parsing,
   `config.toml` load, and credential resolution (`credentials/`) happen first because they may
   read stdin, talk to D-Bus, or run Argon2 — none of which belong in raw mode or on a worker thread.

Module map:
- `net.rs` — one worker does IMAP (read) and SMTP (send). Reconnects with exponential backoff,
  never tight-loops (Gmail locks the account for ~a day above 10 simultaneous IMAP connections).
- `app.rs` — `AppState`, modes (`Normal`/`Compose`/`Search`), and all key handling.
- `ui/` — pure rendering only (`mailbox`, `compose`, `search`, `status`).
- `mime.rs` — RFC 2047 / MIME parsing (`mailparse`); `message.rs` — plain shared data types.
- `credentials/` — `os.rs` (keyring) + `file.rs` (passphrase-encrypted file); `config.rs` — args + `config.toml`; `fsx.rs` — atomic file writes.

## Conventions & gotchas

- Opening a message uses `BODY.PEEK[]` on purpose so it is *not* marked `\Seen`; only the `m` key
  flips the flag. Preserve this.
- `ratatui` needs the `unstable-rendered-line-info` feature (for `Paragraph::line_count`); without it
  the reader's scroll clamping does not compile. Treat as a hard requirement.
- All async deps are pinned to the tokio runtime via features (`runtime-tokio`, `tokio1-native-tls`)
  specifically to avoid linking a second runtime or TLS stack. Don't flip these features casually.
- TLS is `native-tls`/SChannel on Windows; a Linux build additionally needs `libssl-dev` + `pkg-config`.
- Credentials: `Store::Auto` probes the OS keyring and falls back to the encrypted file when the store
  is present-but-unusable (headless Linux, containers, WSL). `--store=os` deliberately *fails* instead
  of falling back. On Windows `credentials.enc` lives under `data_local_dir` (not the roaming config
  dir) — intentional, don't "fix" it.
- The credential file is a versioned format (`GTUI01` magic, Argon2 params self-described in the
  header). Changing it invalidates existing files. Tests use cheap `Kdf::TESTING` params; keep one
  test covering the real `Kdf::CURRENT`.
- CLI flags are hand-parsed in `Args::parse` (no clap) and support both `--flag value` and
  `--flag=value`. Adding/renaming a flag means updating the parser, its tests, and the `HELP` string.
- Headless/scripted runs: set `GMAIL_USER` + `GMAIL_PASS` (`GMAIL_PASS` is never persisted). Interactive
  prompts require a real TTY and will error out otherwise.

## Testing

- All tests are inline `#[cfg(test)] mod tests`; there is no `tests/` dir and no network usage.
- `AppState::new` can be built in a plain `#[test]` by making an `mpsc::channel` and dropping the
  receiver — channel construction needs no runtime.
- Add tests next to the code they cover, matching the existing style (small focused `#[test]` fns).

//! Gmail TUI — Phase 3.
//!
//! Program shape:
//!
//! ```text
//!   main
//!    ├── args, config, credentials      (before raw mode; may prompt)
//!    └── tokio runtime
//!         ├── render loop ── crossterm events ──┐
//!         │                                     ├── AppState
//!         └── net::spawn (IMAP task) ─── mpsc ──┘
//! ```
//!
//! The UI never performs network I/O. It pushes `NetCmd`s down one channel and
//! drains `NetMsg`s from another, so a slow IMAP round-trip can never freeze
//! rendering. SMTP shares that worker: reading is IMAP, sending is SMTP.

mod app;
mod config;
mod credentials;
mod event;
mod fsx;
mod message;
mod mime;
mod net;
mod ui;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::app::AppState;
use crate::config::{Args, Config, HELP};
use crate::event::EventHandler;
use crate::net::{NetCmd, NetMsg};

fn main() -> ExitCode {
    match start() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("gmail-tui: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<()> {
    let args = Args::parse(std::env::args())?;
    if args.help {
        print!("{HELP}");
        return Ok(());
    }

    // Everything up to the runtime is deliberately outside both the alternate
    // screen and tokio: the config may prompt for an address, and resolving the
    // credential may prompt for a password, run Argon2, or talk to D-Bus. None
    // of that belongs in raw mode, and none of it belongs on a worker thread.
    let config = Config::load(&args)?;

    if let Some(target) = args.forget {
        return credentials::forget(&config, target);
    }

    let secret = credentials::resolve(&config, &args)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        // UI -> network commands, and network -> UI updates. There is a single
        // writer to the UI state, so no lock is ever held across an `.await`.
        let (net_tx, net_rx) = mpsc::channel::<NetCmd>(32);
        let (msg_tx, mut msg_rx) = mpsc::channel::<NetMsg>(64);

        net::spawn(msg_tx, net_rx, config.user.clone(), secret);

        let mut app = AppState::new(net_tx);

        // `ratatui::init()` enables raw mode, switches to the alternate screen
        // and installs a panic hook that restores the terminal, so a panic
        // anywhere below cannot leave the shell in raw mode.
        let mut terminal = ratatui::init();
        let result = render(&mut terminal, &mut app, &mut msg_rx).await;
        ratatui::restore();

        result
    })
}

async fn render(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut AppState,
    msg_rx: &mut mpsc::Receiver<NetMsg>,
) -> Result<()> {
    let mut events = EventHandler::new(Duration::from_millis(250));

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, app))?;

        tokio::select! {
            // Keyboard / resize / tick.
            maybe = events.next() => match maybe {
                Some(event) => app.on_event(event),
                None => break,
            },
            // Anything the network task wants to tell us.
            Some(msg) = msg_rx.recv() => app.on_net(msg),
        }
    }

    Ok(())
}

mod app;
mod github;
mod ui;

use std::io;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use app::{App, Msg};
use github::Client;

const TOKEN_VARS: [&str; 3] = ["GITHUB_PAT", "GITHUB_TOKEN", "GH_TOKEN"];
const DEFAULT_INTERVAL_SECS: u64 = 15;

const USAGE: &str = "\
GitHub PR Check — live CI status of your open GitHub pull requests

USAGE:
    github-pr-check [--interval <seconds>] [--once]

OPTIONS:
    -i, --interval <seconds>   Auto-refresh interval (default 15, minimum 5)
        --once                 Print a plain-text snapshot and exit (no TUI)
    -V, --version              Show version
    -h, --help                 Show this help

The token is read from GITHUB_PAT, GITHUB_TOKEN or GH_TOKEN, either from the
environment or from a .env file in the current directory.";

fn main() -> ExitCode {
    load_dotenv(".env");

    let (interval, once) = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let Some(token) = TOKEN_VARS
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
    else {
        eprintln!("error: no GitHub token found. Set GITHUB_PAT (or GITHUB_TOKEN / GH_TOKEN) or add it to .env");
        return ExitCode::FAILURE;
    };

    let mut client = Client::new(token.trim().to_string());

    if once {
        return print_once(&mut client);
    }

    let (refresh_tx, refresh_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    app::spawn_worker(client, interval, refresh_rx, msg_tx);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, App::new(interval), &msg_rx, &refresh_tx);
    ratatui::restore();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(terminal: &mut DefaultTerminal, mut app: App, msgs: &Receiver<Msg>, refresh: &Sender<()>) -> io::Result<()> {
    loop {
        while let Ok(msg) = msgs.try_recv() {
            app.on_msg(msg);
        }
        terminal.draw(|f| ui::draw(f, &app))?;

        // Short poll timeout keeps spinners, durations and the countdown ticking.
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('r') => {
                let _ = refresh.send(());
            }
            KeyCode::Down | KeyCode::Char('j') => app.select_next(),
            KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
            KeyCode::Home | KeyCode::Char('g') => app.select_first(),
            KeyCode::End | KeyCode::Char('G') => app.select_last(),
            KeyCode::Enter | KeyCode::Char('d') => app.show_details = !app.show_details,
            KeyCode::Char('o') => {
                if let Some(pr) = app.selected_pr() {
                    open_url(&pr.url);
                }
            }
            _ => {}
        }
    }
}

/// Plain-text snapshot for scripts or checking a token: one fetch, no TUI.
fn print_once(client: &mut Client) -> ExitCode {
    let snap = match client.fetch() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("@{} — {} open PR(s)", snap.login, snap.prs.len());
    for pr in &snap.prs {
        let c = &pr.counts;
        println!(
            "\n{}#{} {}\n  {:?}: {}/{} done (passed {}, failed {}, running {}, queued {}, skipped {})",
            pr.repo, pr.number, pr.title, c.overall(), c.finished(), c.total(),
            c.passed, c.failed, c.running, c.pending, c.skipped
        );
        for check in &pr.checks {
            let detail = check.detail.as_deref().map(|d| format!(" · {d}")).unwrap_or_default();
            let estimate = ui::estimate_text(check).map(|e| format!(" · {e}")).unwrap_or_default();
            println!("    {:<8} {}{}{}", format!("{:?}", check.state), check.name, detail, estimate);
        }
    }
    for w in &snap.warnings {
        eprintln!("warning: {w}");
    }
    ExitCode::SUCCESS
}

/// Returns `Ok(None)` when help was requested.
fn parse_args() -> Result<Option<(Duration, bool)>, String> {
    let mut secs = DEFAULT_INTERVAL_SECS;
    let mut once = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "-V" | "--version" => {
                println!("github-pr-check {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--once" => once = true,
            "-i" | "--interval" => {
                let v = args.next().ok_or("--interval needs a value")?;
                secs = v.parse().map_err(|_| format!("invalid interval: {v}"))?;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Some((Duration::from_secs(secs.max(5)), once)))
}

/// Minimal .env loader: KEY=VALUE lines, optional quotes, `#` comments. Never overrides real env vars.
fn load_dotenv(path: &str) {
    let Ok(contents) = std::fs::read_to_string(path) else { return };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.trim_start_matches("export ").split_once('=') else { continue };
        let key = key.trim();
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
        }
    }
}

fn open_url(url: &str) {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

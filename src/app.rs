//! Application state and the background refresh worker.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

use crate::github::{Client, PullRequest, Snapshot};

pub enum Msg {
    Loading,
    Loaded(Result<Snapshot, String>),
}

/// Fetches immediately, then again every `interval` or whenever something is sent on `refresh_rx`.
/// Exits when the UI drops its end of either channel.
pub fn spawn_worker(mut client: Client, interval: Duration, refresh_rx: Receiver<()>, tx: Sender<Msg>) {
    thread::spawn(move || loop {
        if tx.send(Msg::Loading).is_err() {
            return;
        }
        if tx.send(Msg::Loaded(client.fetch())).is_err() {
            return;
        }
        match refresh_rx.recv_timeout(interval) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => while refresh_rx.try_recv().is_ok() {},
            Err(RecvTimeoutError::Disconnected) => return,
        }
    });
}

pub struct App {
    pub login: Option<String>,
    pub prs: Vec<PullRequest>,
    pub selected: usize,
    pub show_details: bool,
    pub loading: bool,
    pub loaded_once: bool,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub last_update: Option<DateTime<Local>>,
    pub next_refresh: Instant,
    pub interval: Duration,
    pub started: Instant,
}

impl App {
    pub fn new(interval: Duration) -> Self {
        Self {
            login: None,
            prs: Vec::new(),
            selected: 0,
            show_details: true,
            loading: true,
            loaded_once: false,
            error: None,
            warnings: Vec::new(),
            last_update: None,
            next_refresh: Instant::now() + interval,
            interval,
            started: Instant::now(),
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Loading => self.loading = true,
            Msg::Loaded(result) => {
                self.loading = false;
                self.next_refresh = Instant::now() + self.interval;
                match result {
                    Ok(snap) => {
                        // Keep the same PR selected across refreshes.
                        let selected_url = self.selected_pr().map(|p| p.url.clone());
                        self.prs = snap.prs;
                        self.selected = selected_url
                            .and_then(|url| self.prs.iter().position(|p| p.url == url))
                            .unwrap_or(self.selected)
                            .min(self.prs.len().saturating_sub(1));
                        self.login = Some(snap.login);
                        self.warnings = snap.warnings;
                        self.last_update = Some(snap.fetched_at);
                        self.loaded_once = true;
                        self.error = None;
                    }
                    // Keep showing the last good data alongside the error.
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    pub fn selected_pr(&self) -> Option<&PullRequest> {
        self.prs.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.prs.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.prs.len().saturating_sub(1);
    }
}

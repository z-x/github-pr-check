//! Rendering.

use std::time::Instant;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::App;
use crate::github::{Check, CheckState, Counts, Overall, PullRequest};

const BAR_WIDTH: usize = 24;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &App) {
    let show_details = app.show_details && app.selected_pr().is_some();
    let [header, main, details, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Percentage(if show_details { 45 } else { 0 }),
        Constraint::Length(2),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    draw_pr_table(f, app, main);
    if show_details {
        if let Some(pr) = app.selected_pr() {
            draw_details(f, app, pr, details);
        }
    }
    draw_footer(f, app, footer);
}

fn spinner(app: &App) -> &'static str {
    SPINNER[(app.started.elapsed().as_millis() / 100) as usize % SPINNER.len()]
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(" GitHub PR Check ", Style::new().bold().black().on_cyan()), Span::raw("  ")];
    if let Some(login) = &app.login {
        spans.push(Span::styled(format!("@{login}"), Style::new().bold()));
        spans.push(Span::raw(format!("  ·  {} open PR{}", app.prs.len(), if app.prs.len() == 1 { "" } else { "s" })));

        let mut tally = [0usize; 4];
        for pr in &app.prs {
            tally[pr.counts.overall() as usize] += 1;
        }
        for (overall, label, color) in [
            (Overall::Failing, "failing", Color::Red),
            (Overall::Running, "running", Color::Yellow),
            (Overall::Passed, "passed", Color::Green),
        ] {
            let n = tally[overall as usize];
            if n > 0 {
                spans.push(Span::raw("  ·  "));
                spans.push(Span::styled(format!("{n} {label}"), Style::new().fg(color)));
            }
        }
    }
    f.render_widget(Line::from(spans), area);
}

fn overall_symbol(app: &App, overall: Overall) -> Span<'static> {
    match overall {
        Overall::Passed => Span::styled("✓", Style::new().green().bold()),
        Overall::Failing => Span::styled("✗", Style::new().red().bold()),
        Overall::Running => Span::styled(spinner(app), Style::new().yellow().bold()),
        Overall::NoChecks => Span::styled("–", Style::new().dark_gray()),
    }
}

/// A bar whose segments are proportional to passed / failed / skipped / running / pending.
fn progress_bar(c: &Counts, width: usize) -> Line<'static> {
    let total = c.total() as usize;
    if total == 0 {
        return Line::from(Span::styled("·".repeat(width), Style::new().dark_gray()));
    }
    let segments = [
        (c.passed, "█", Color::Green),
        (c.failed, "█", Color::Red),
        (c.skipped, "█", Color::DarkGray),
        (c.running, "▓", Color::Yellow),
        (c.pending, "░", Color::DarkGray),
    ];
    let mut spans = Vec::new();
    let (mut cumulative, mut drawn) = (0usize, 0usize);
    for (n, glyph, color) in segments {
        cumulative += n as usize;
        let end = (cumulative * width + total / 2) / total;
        if end > drawn {
            spans.push(Span::styled(glyph.repeat(end - drawn), Style::new().fg(color)));
            drawn = end;
        }
    }
    Line::from(spans)
}

fn count_spans(c: &Counts) -> Line<'static> {
    let mut spans = Vec::new();
    for (n, label, color) in [
        (c.passed, "✓", Color::Green),
        (c.failed, "✗", Color::Red),
        (c.running, "▶", Color::Yellow),
        (c.pending, "◌", Color::Gray),
        (c.skipped, "⊘", Color::DarkGray),
    ] {
        if n > 0 {
            if !spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(format!("{label}{n}"), Style::new().fg(color)));
        }
    }
    Line::from(spans)
}

fn draw_pr_table(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().dark_gray())
        .title(Span::styled(" Open pull requests ", Style::new().bold().white()));

    if app.prs.is_empty() {
        let msg = if !app.loaded_once {
            format!("{} Loading pull requests…", spinner(app))
        } else {
            "No open pull requests.".to_string()
        };
        let p = Paragraph::new(msg).alignment(Alignment::Center).block(block).dark_gray();
        f.render_widget(p, area);
        return;
    }

    let repo_width = app
        .prs
        .iter()
        .map(|p| p.repo.chars().count() + 1 + p.number.to_string().len())
        .max()
        .unwrap_or(10)
        .min(40) as u16;

    let rows = app.prs.iter().map(|pr| {
        let c = &pr.counts;
        let mut title = Vec::new();
        if pr.is_draft {
            title.push(Span::styled("[draft] ", Style::new().dark_gray()));
        }
        title.push(Span::raw(pr.title.clone()));

        let progress = if c.total() == 0 {
            Line::from(Span::styled("no checks", Style::new().dark_gray()))
        } else {
            let pct = c.finished() * 100 / c.total();
            Line::from(format!("{:>3}/{:<3} {:>3}%", c.finished(), c.total(), pct))
        };

        let eta = match pr.eta() {
            Some(rem) if rem.num_seconds() > 0 => Span::styled(format!("~{}", format_duration(rem)), Style::new().yellow()),
            Some(_) => Span::styled("overdue", Style::new().magenta()),
            None => Span::raw(""),
        };

        Row::new(vec![
            Cell::from(overall_symbol(app, c.overall())),
            Cell::from(Span::styled(format!("{}#{}", pr.repo, pr.number), Style::new().cyan())),
            Cell::from(Line::from(title)),
            Cell::from(progress_bar(c, BAR_WIDTH)),
            Cell::from(progress),
            Cell::from(eta),
            Cell::from(count_spans(c)),
        ])
    });

    let header = Row::new(["", "PR", "Title", "Checks", "Done", "ETA", "Breakdown"])
        .style(Style::new().dark_gray().add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(repo_width),
            Constraint::Fill(1),
            Constraint::Length(BAR_WIDTH as u16),
            Constraint::Length(13),
            Constraint::Length(8),
            Constraint::Length(22),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2)
    .row_highlight_style(Style::new().bg(Color::Rgb(40, 44, 60)).add_modifier(Modifier::BOLD))
    .highlight_symbol("▌");

    let mut state = TableState::default().with_selected(Some(app.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn format_duration(d: chrono::Duration) -> String {
    let s = d.num_seconds().max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// "~60% · 2m10s left", or how far past its usual duration a job is.
pub fn estimate_text(check: &Check) -> Option<String> {
    let (fraction, remaining) = check.estimate()?;
    Some(if remaining.num_seconds() > 0 {
        format!("~{:.0}% · {} left", (fraction * 100.0).min(99.0), format_duration(remaining))
    } else {
        format!("{} longer than usual", format_duration(-remaining))
    })
}

fn draw_details(f: &mut Frame, app: &App, pr: &PullRequest, area: Rect) {
    let mut title = format!(" {}#{} — {} ", pr.repo, pr.number, pr.title);
    let shown = pr.checks.len() as u32;
    if pr.counts.total() > shown {
        title.push_str(&format!("(showing {shown} of {} checks) ", pr.counts.total()));
    }
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().dark_gray())
        .title(Span::styled(title, Style::new().bold().white()));

    if pr.checks.is_empty() {
        let p = Paragraph::new("No CI checks reported for the latest commit.").block(block).dark_gray();
        f.render_widget(p, area);
        return;
    }

    let rows = pr.checks.iter().map(|check| {
        let (symbol, label, style) = match check.state {
            CheckState::Passed => ("✓", "passed", Style::new().green()),
            CheckState::Failed => ("✗", "failed", Style::new().red()),
            CheckState::Running => (spinner(app), "running", Style::new().yellow()),
            CheckState::Pending => ("◌", "queued", Style::new().gray()),
            CheckState::Skipped => ("⊘", "skipped", Style::new().dark_gray()),
        };
        let duration = check.duration().map(format_duration).unwrap_or_default();
        let mut name = vec![Span::raw(check.name.clone())];
        if let Some(detail) = &check.detail {
            name.push(Span::styled(format!("  · {detail}"), Style::new().yellow().dim()));
        }
        if let Some(text) = estimate_text(check) {
            name.push(Span::styled(format!("  · {text}"), Style::new().yellow()));
        }
        Row::new(vec![
            Cell::from(Span::styled(symbol, style.bold())),
            Cell::from(Line::from(name)),
            Cell::from(Span::styled(label, style)),
            Cell::from(Span::styled(duration, Style::new().dark_gray())),
        ])
    });

    let table = Table::new(
        rows,
        [Constraint::Length(1), Constraint::Fill(1), Constraint::Length(8), Constraint::Length(8)],
    )
    .block(block)
    .column_spacing(2);
    f.render_widget(table, area);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let [status_line, keys_line] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    let status = if let Some(err) = &app.error {
        Line::from(Span::styled(format!(" ✗ {err}"), Style::new().red()))
    } else if let Some(w) = app.warnings.first() {
        let more = if app.warnings.len() > 1 { format!(" (+{} more)", app.warnings.len() - 1) } else { String::new() };
        Line::from(Span::styled(format!(" ⚠ {w}{more}"), Style::new().yellow()))
    } else {
        Line::default()
    };
    f.render_widget(status, status_line);

    let key = |k: &'static str| Span::styled(k, Style::new().cyan().bold());
    let desc = |d: &'static str| Span::styled(d, Style::new().dark_gray());
    let keys = Line::from(vec![
        Span::raw(" "),
        key("↑↓"),
        desc(" select  "),
        key("enter"),
        desc(" details  "),
        key("o"),
        desc(" open in browser  "),
        key("r"),
        desc(" refresh  "),
        key("q"),
        desc(" quit"),
    ]);
    f.render_widget(keys, keys_line);

    let refresh = if app.loading {
        format!("{} refreshing… ", spinner(app))
    } else {
        let secs = app.next_refresh.saturating_duration_since(Instant::now()).as_secs();
        let updated = app.last_update.map(|t| t.format("%H:%M:%S").to_string()).unwrap_or_else(|| "never".into());
        // Pad to the interval's digit count so the right-aligned text doesn't shift as the countdown shrinks.
        let width = app.interval.as_secs().to_string().len();
        format!("updated {updated} · next in {secs:>width$}s ")
    };
    f.render_widget(Line::from(refresh).alignment(Alignment::Right).dark_gray(), keys_line);
}

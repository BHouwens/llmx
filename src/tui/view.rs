//! Rendering. No I/O, no decisions — purely state to widgets.

use crate::backend::BackendStatus;
use crate::logging::Level;
use crate::logging::LogLine;
use crate::system::MemoryVerdict;
use crate::tui::state::format_tokens;
use crate::tui::state::tilde;
use crate::tui::state::PickerState;
use crate::tui::state::StartupState;
use crate::tui::state::StepState;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui::Frame;

const DIM: Color = Color::DarkGray;

fn status_style(status: &BackendStatus) -> Style {
    match status {
        BackendStatus::Ready => Style::default().fg(Color::Green),
        BackendStatus::Starting | BackendStatus::Connecting => Style::default().fg(Color::Yellow),
        BackendStatus::Error(_) => Style::default().fg(Color::Red),
        BackendStatus::Stopped => Style::default().fg(DIM),
    }
}

pub fn draw_picker(frame: &mut Frame, state: &PickerState, log_tail: &[LogLine]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(if state.show_logs { 10 } else { 11 }),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_model_list(frame, chunks[0], state);

    if state.show_logs {
        draw_logs(frame, chunks[1], log_tail);
    } else {
        draw_detail(frame, chunks[1], state);
    }

    let hints = "[Enter] launch  [↑/↓ or j/k] select  [r] refresh  [l] logs  [q] quit";
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(hints, Style::default().fg(DIM)))),
        chunks[2],
    );
}

fn draw_model_list(frame: &mut Frame, area: Rect, state: &PickerState) {
    let title = if state.refreshing {
        format!(" Models — {} (checking…) ", tilde(&state.config_path))
    } else {
        format!(" Models — {} ", tilde(&state.config_path))
    };

    // Align the name column to the longest name so the backend and status
    // columns line up regardless of how the profiles are named.
    let name_width = state
        .rows
        .iter()
        .map(|r| r.profile.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(12, 40);
    let label_width = state
        .rows
        .iter()
        .map(|r| r.profile.label().chars().count())
        .max()
        .unwrap_or(0)
        .clamp(5, 12);

    let items: Vec<ListItem> = state
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let selected = index == state.selected;
            let marker = if selected { "> " } else { "  " };
            let name_style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let mut spans = vec![
                Span::styled(marker, Style::default().fg(DIM)),
                Span::styled(
                    format!("{:<width$}", row.profile.name, width = name_width),
                    name_style,
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<width$}", row.profile.label(), width = label_width),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<10}", row.status.label()),
                    status_style(&row.status),
                ),
            ];

            if matches!(
                row.memory,
                MemoryVerdict::Tight(_) | MemoryVerdict::Insufficient(_)
            ) {
                spans.push(Span::styled("  ⚠", Style::default().fg(Color::Yellow)));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(list, area);
}

fn draw_detail(frame: &mut Frame, area: Rect, state: &PickerState) {
    let Some(row) = state.selected_row() else {
        frame.render_widget(
            Paragraph::new("No models configured.")
                .block(Block::default().borders(Borders::ALL).title(" Details ")),
            area,
        );
        return;
    };

    let profile = &row.profile;
    let mut lines = vec![
        field("Selected", &profile.name),
        field(
            "Backend",
            &format!("{} · {}", profile.backend.as_str(), profile.label()),
        ),
        field("Endpoint", &profile.endpoint.url),
        field(
            "Model id",
            profile
                .model_id
                .as_deref()
                .unwrap_or("(discover at launch)"),
        ),
        field(
            "Context",
            &format!(
                "{} in / {} out",
                format_tokens(profile.context_window),
                format_tokens(profile.max_output_tokens)
            ),
        ),
        Line::from(vec![
            Span::styled(format!("{:<12}", "Status"), Style::default().fg(DIM)),
            Span::styled(row.status.label().to_string(), status_style(&row.status)),
        ]),
    ];

    let memory_line = match (&row.memory, state.total_ram_gb) {
        (MemoryVerdict::Insufficient(detail), _) => Some(Line::from(vec![
            Span::styled(format!("{:<12}", "Memory"), Style::default().fg(DIM)),
            Span::styled(
                format!("WARNING: {detail}"),
                Style::default().fg(Color::Red),
            ),
        ])),
        (MemoryVerdict::Tight(detail), _) => Some(Line::from(vec![
            Span::styled(format!("{:<12}", "Memory"), Style::default().fg(DIM)),
            Span::styled(
                format!("tight: {detail}"),
                Style::default().fg(Color::Yellow),
            ),
        ])),
        (MemoryVerdict::Comfortable, Some(total)) => {
            let estimate = profile
                .resources
                .as_ref()
                .and_then(|r| r.estimated_ram_gb)
                .map(|e| format!("est. {e:.0} GB of {total:.0} GB"))
                .unwrap_or_else(|| format!("{total:.0} GB system RAM"));
            Some(field("Memory", &estimate))
        }
        (_, Some(total)) if profile.backend == crate::config::BackendKind::Local => {
            Some(field("Memory", &format!("{total:.0} GB system RAM")))
        }
        _ => Some(field("Memory", "remote (not this machine)")),
    };
    lines.extend(memory_line);

    lines.push(field("Project", &tilde(&state.cwd)));

    if let BackendStatus::Error(message) = &row.status {
        lines.push(Line::from(Span::styled(
            format!("  {message}"),
            Style::default().fg(Color::Red),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" Details ")),
        area,
    );
}

fn draw_logs(frame: &mut Frame, area: Rect, tail: &[LogLine]) {
    let height = area.height.saturating_sub(2) as usize;
    let start = tail.len().saturating_sub(height);
    let lines: Vec<Line> = tail[start..]
        .iter()
        .map(|entry| {
            let color = match entry.level {
                Level::Error => Color::Red,
                Level::Warn => Color::Yellow,
                Level::Info => Color::Reset,
                Level::Debug => DIM,
            };
            Line::from(Span::styled(entry.text.clone(), Style::default().fg(color)))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Logs — [l] back to details "),
        ),
        area,
    );
}

pub fn draw_startup(frame: &mut Frame, state: &StartupState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(1)])
        .split(frame.area());

    let mut lines = vec![
        Line::from(Span::styled(
            state.profile_name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            state.backend_description.clone(),
            Style::default().fg(DIM),
        )),
        Line::from(""),
    ];

    for step in &state.steps {
        let (marker, style, detail) = match &step.state {
            StepState::Pending => ("  ", Style::default().fg(DIM), String::new()),
            StepState::Active(detail) => ("→ ", Style::default().fg(Color::Yellow), detail.clone()),
            StepState::Done(detail) => ("✓ ", Style::default().fg(Color::Green), detail.clone()),
            StepState::Failed(detail) => ("✗ ", Style::default().fg(Color::Red), detail.clone()),
        };
        let mut spans = vec![
            Span::styled(marker, style),
            Span::styled(format!("{:<28}", step.title), style),
        ];
        if !detail.is_empty() {
            spans.push(Span::styled(detail, Style::default().fg(DIM)));
        }
        lines.push(Line::from(spans));
    }

    if let Some(choices) = &state.model_choices {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "This endpoint serves several models. Choose one:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for (index, choice) in choices.iter().enumerate().take(9) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  [{}] ", index + 1),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(choice.clone()),
            ]));
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Starting ")),
        chunks[0],
    );

    let hint = if state.model_choices.is_some() {
        "press a number to choose  ·  [q] cancel"
    } else {
        "[q] or Ctrl-C to cancel"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{}s  ", state.elapsed_secs),
                Style::default().fg(DIM),
            ),
            Span::styled(hint, Style::default().fg(DIM)),
        ])),
        chunks[1],
    );
}

fn field<'a>(label: &str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), Style::default().fg(DIM)),
        Span::raw(value.to_string()),
    ])
}

use std::fmt::Display;

use codeatlas_core::{
    ClaimKind, Diagram, DiagramDecision, DiagramKind, EntryPointKind, Evidence, EvidenceId,
    Language, ProgressPhase, TargetResolution,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};

use crate::app::{
    Activity, AnswerView, ClaimNumbers, ConversationEntry, EvidenceViewer, HistoryView, InputMode,
    LayoutMode, Panel, ToolTraceStatus, TuiApp, evidence_line_range, evidence_number,
};
use crate::highlight::highlight_source_lines;

const ACCENT: Color = Color::Rgb(72, 196, 196);
const MUTED: Color = Color::Rgb(116, 128, 141);
const CODE: Color = Color::Rgb(180, 210, 202);
const FACT: Color = Color::Rgb(104, 211, 145);
const INFERENCE: Color = Color::Rgb(235, 184, 89);
const UNKNOWN: Color = Color::Rgb(197, 142, 255);
const ERROR: Color = Color::Rgb(245, 101, 101);
const MAX_DIAGRAM_EVIDENCE_REFERENCES: usize = 8;

impl TuiApp {
    /// Renders the current state into any ratatui backend.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if self.error_details_visible() && self.error().is_some() {
            render_error_details(frame, self, area);
            return;
        }
        if self.evidence_viewer().is_some() {
            render_evidence_viewer(frame, self, area);
            return;
        }
        if self.history_view().is_some() {
            render_history(frame, self, area);
            return;
        }
        let mode = self.layout_mode(area.width, area.height);
        if mode == LayoutMode::Compact {
            render_compact(frame, self, area);
            return;
        }

        let input_height = if area.height >= 12 { 3 } else { 2 };
        let status_height = if area.height >= 10 { 2 } else { 1 };
        let [header, main, input, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(status_height),
        ])
        .areas(area);

        render_header(frame, self, header, mode);
        match mode {
            LayoutMode::Wide => render_wide(frame, self, main),
            LayoutMode::Tabbed => render_tabbed(frame, self, main),
            LayoutMode::Compact => {}
        }
        render_input(frame, self, input);
        render_status(frame, self, status);
    }
}

fn render_history(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    let history = app
        .history_view()
        .cloned()
        .expect("history render requires visible history state");
    let indexed_repository = app.repository().repository_id;
    let session_count = history.sessions().len();
    let task_count = history
        .sessions()
        .iter()
        .map(|session| session.tasks.len())
        .sum::<usize>();
    let block = Block::new()
        .borders(Borders::ALL)
        .title(Line::styled(
            format!(" Session History | {session_count} sessions / {task_count} tasks "),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::new().fg(ACCENT));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let footer_height = 2.min(inner.height.saturating_sub(1));
    let metadata_height = 6.min(inner.height.saturating_sub(footer_height + 1));
    let [metadata, sessions, footer] = Layout::vertical([
        Constraint::Length(metadata_height),
        Constraint::Min(1),
        Constraint::Length(footer_height),
    ])
    .areas(inner);

    if !metadata.is_empty() {
        frame.render_widget(
            Paragraph::new(history_metadata_lines(&history, indexed_repository))
                .wrap(Wrap { trim: false }),
            metadata,
        );
    }

    if !sessions.is_empty() {
        let list = List::new(history_items(&history, indexed_repository))
            .highlight_symbol("> ")
            .highlight_style(Style::new().fg(Color::White).bg(Color::Rgb(32, 52, 58)));
        let mut state = ListState::default().with_selected(history.selected_session());
        frame.render_stateful_widget(list, sessions, &mut state);
        app.set_history_scroll(u16::try_from(state.offset()).unwrap_or(u16::MAX));
    }

    if !footer.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    "j/k select | PgUp/PgDn | Home/End | Enter load entire session",
                    Style::new().fg(MUTED),
                ),
                Line::styled(
                    "N new empty session | Sessions auto-save on Ask | Esc/h close | q quit",
                    Style::new().fg(MUTED),
                ),
            ])
            .wrap(Wrap { trim: false }),
            footer,
        );
    }
}

fn history_metadata_lines(
    history: &HistoryView,
    indexed_repository: Option<codeatlas_core::RepositoryId>,
) -> Vec<Line<'static>> {
    selected_history_session(history).map_or_else(
        || {
            vec![Line::styled(
                if history.loading() {
                    "Loading saved sessions..."
                } else {
                    "No persisted sessions found. Completed answers are saved automatically."
                },
                Style::new().fg(MUTED),
            )]
        },
        |session| {
            vec![
                labeled_line("SESSION", session.session_id.to_string()),
                labeled_line("REPO", session.repository_id.to_string()),
                labeled_line(
                    "MODE",
                    if indexed_repository == Some(session.repository_id) {
                        "ready to continue".to_owned()
                    } else {
                        "read-only; index this repository to continue".to_owned()
                    },
                ),
                labeled_line("CREATED", unix_time_label(session.created_at_unix_ms)),
                labeled_line("UPDATED", unix_time_label(session.updated_at_unix_ms)),
                labeled_line("JSON", terminal_safe_text(&session.json_path)),
            ]
        },
    )
}

fn history_items(
    history: &HistoryView,
    indexed_repository: Option<codeatlas_core::RepositoryId>,
) -> Vec<ListItem<'static>> {
    history
        .sessions()
        .iter()
        .map(|session| {
            let current = indexed_repository == Some(session.repository_id);
            let title = session.tasks.first().map_or("<empty session>", |task| {
                task.question
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("<empty question>")
            });
            ListItem::new(Line::from(vec![
                Span::styled(
                    if current {
                        "[current] "
                    } else {
                        "[read-only] "
                    },
                    Style::new().fg(if current { FACT } else { MUTED }),
                ),
                Span::styled(
                    format!("#{:<8} ", short_id(session.session_id)),
                    Style::new().fg(MUTED),
                ),
                Span::styled(
                    format!(
                        "{} task{} ",
                        session.tasks.len(),
                        if session.tasks.len() == 1 { "" } else { "s" }
                    ),
                    Style::new().fg(ACCENT),
                ),
                Span::styled(
                    format!("updated {} | ", unix_time_label(session.updated_at_unix_ms)),
                    Style::new().fg(INFERENCE),
                ),
                Span::raw(terminal_safe_text(title)),
            ]))
        })
        .collect()
}

fn selected_history_session(history: &HistoryView) -> Option<&codeatlas_core::SessionSummary> {
    history.sessions().get(history.selected_session()?)
}

fn unix_time_label(unix_ms: u64) -> String {
    format!("unix:{}s", unix_ms / 1_000)
}

fn render_error_details(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    let block = Block::new()
        .borders(Borders::ALL)
        .title(Line::styled(
            " Error Details ",
            Style::new().fg(ERROR).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::new().fg(ERROR));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let footer_height = if app.clipboard_status().is_some() {
        3
    } else {
        2
    };
    let [body, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(footer_height.min(inner.height)),
    ])
    .areas(inner);
    let error = app.error().expect("error details require an error");
    let request = error
        .request_id
        .map_or_else(|| "none".to_owned(), |request_id| request_id.to_string());
    let mut lines = vec![
        labeled_line("CODE", error.code.clone()),
        labeled_line("REQUEST", request),
        labeled_line("RETRY", error.retryable.to_string()),
        Line::default(),
        Line::styled(
            "MESSAGE",
            Style::new().fg(ERROR).add_modifier(Modifier::BOLD),
        ),
    ];
    append_text_lines(
        &mut lines,
        &error.message,
        Style::new().fg(Color::White),
        "  ",
    );
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let line_count = paragraph.line_count(body.width);
    let scroll = bounded_scroll(line_count, body, app.error_scroll(), false);
    app.set_error_scroll(scroll);
    frame.render_widget(paragraph.scroll((scroll, 0)), body);

    let mut footer_lines = vec![Line::styled(
        "j/k or PgUp/PgDn scroll | y copy full error | Esc/x close | q quit",
        Style::new().fg(MUTED),
    )];
    if let Some(status) = app.clipboard_status() {
        footer_lines.push(Line::styled(status.to_owned(), Style::new().fg(ACCENT)));
    }
    frame.render_widget(
        Paragraph::new(footer_lines).wrap(Wrap { trim: false }),
        footer,
    );
}

fn render_evidence_viewer(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    let viewer = app
        .evidence_viewer()
        .cloned()
        .expect("evidence viewer render requires viewer state");
    let number = viewer.evidence_index().saturating_add(1);
    let total = app.evidence().len();
    let block = Block::new()
        .borders(Borders::ALL)
        .title(Line::styled(
            format!(" Source Evidence E{number}/{total} "),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::new().fg(ACCENT));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let horizontal = if viewer.wrap() {
        0
    } else {
        viewer
            .horizontal_scroll()
            .min(source_horizontal_limit(&viewer, inner.width))
    };
    let wrap_status = if viewer.wrap() {
        "wrap:on".to_owned()
    } else {
        format!("wrap:off col:{}", horizontal.saturating_add(1))
    };
    let mut footer_lines = vec![
        Line::styled(
            "j/k scroll | PgUp/PgDn 8 | Home/End | n/p evidence",
            Style::new().fg(MUTED),
        ),
        Line::styled(
            format!("w {wrap_status} | h/l or arrows horizontal | y copy | Esc/v close | q quit"),
            Style::new().fg(MUTED),
        ),
    ];
    if let Some(status) = app.clipboard_status() {
        footer_lines.push(Line::styled(status.to_owned(), Style::new().fg(ACCENT)));
    }
    let footer_paragraph = Paragraph::new(footer_lines).wrap(Wrap { trim: false });
    let footer_wanted = u16::try_from(footer_paragraph.line_count(inner.width)).unwrap_or(u16::MAX);
    let footer_height = footer_wanted.min(inner.height.saturating_sub(1));
    let remaining = inner.height.saturating_sub(footer_height);
    let metadata_height = 3.min(remaining.saturating_sub(1));
    let [metadata, body, footer] = Layout::vertical([
        Constraint::Length(metadata_height),
        Constraint::Min(1),
        Constraint::Length(footer_height),
    ])
    .areas(inner);

    if !metadata.is_empty() {
        let symbol = viewer
            .symbol_id()
            .map_or_else(|| "<none>".to_owned(), |symbol_id| symbol_id.to_string());
        let metadata_lines = vec![
            labeled_line(
                "PATH",
                source_location(viewer.path(), viewer.start_line(), viewer.end_line()),
            ),
            labeled_line("SYMBOL", symbol),
            source_status_line(
                &viewer,
                app.repository().repository_id.is_some()
                    && app.repository().repository_id == app.session_repository_id(),
            ),
        ];
        frame.render_widget(
            Paragraph::new(metadata_lines).wrap(Wrap { trim: false }),
            metadata,
        );
    }

    let viewer_lines = evidence_viewer_lines(app, &viewer, horizontal);
    let paragraph = if viewer.wrap() {
        Paragraph::new(viewer_lines).wrap(Wrap { trim: false })
    } else {
        Paragraph::new(viewer_lines)
    };
    let line_count = paragraph.line_count(body.width);
    let vertical = bounded_scroll(line_count, body, viewer.vertical_scroll(), false);
    app.set_evidence_viewer_scroll(vertical, horizontal);
    if !body.is_empty() {
        frame.render_widget(paragraph.scroll((vertical, 0)), body);
    }

    if !footer.is_empty() {
        frame.render_widget(footer_paragraph, footer);
    }
}

fn source_status_line(viewer: &EvidenceViewer, repository_available: bool) -> Line<'static> {
    let displayed = if viewer.content().is_empty() {
        "no source text".to_owned()
    } else {
        format!(
            "showing {}-{}",
            viewer.content_start_line(),
            viewer.content_end_line()
        )
    };
    let status = if viewer.loading() {
        format!(
            "loading requested {}-{}; {displayed} excerpt",
            viewer.start_line(),
            viewer.end_line()
        )
    } else if let Some(error) = viewer.source_error() {
        format!(
            "source load failed; {displayed} excerpt remains: {}",
            terminal_safe_text(error)
        )
    } else if viewer.source_loaded() {
        format!("loaded {displayed}")
    } else if repository_available {
        format!("{displayed} excerpt; source request unavailable")
    } else {
        format!("{displayed} excerpt; matching repository is not indexed")
    };
    labeled_line("SOURCE", status)
}

fn evidence_viewer_lines(
    app: &TuiApp,
    viewer: &EvidenceViewer,
    horizontal: u16,
) -> Vec<Line<'static>> {
    let contexts = app.evidence_claim_contexts(viewer.evidence_id());
    let mut lines = vec![section_line("CLAIMS")];
    if contexts.is_empty() {
        lines.push(Line::styled(
            "  No completed claim cites this evidence",
            Style::new().fg(MUTED),
        ));
    } else {
        for context in contexts {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("[{}] ", context.label),
                    claim_style(context.kind).add_modifier(Modifier::BOLD),
                ),
                Span::raw(terminal_safe_text(&context.text)),
            ]));
        }
    }
    lines.push(Line::default());
    lines.push(section_line("SOURCE"));
    lines.extend(viewer_source_lines(viewer, horizontal));
    lines
}

fn viewer_source_lines(viewer: &EvidenceViewer, horizontal: u16) -> Vec<Line<'static>> {
    if viewer.content().is_empty() {
        return vec![Line::styled("<source unavailable>", Style::new().fg(MUTED))];
    }

    numbered_source_lines(
        viewer.path().as_str(),
        viewer.content(),
        viewer.content_start_line(),
        1,
        if viewer.wrap() { 0 } else { horizontal },
    )
}

fn numbered_source_lines(
    path: &str,
    source: &str,
    start_line: u32,
    minimum_number_width: usize,
    horizontal: u16,
) -> Vec<Line<'static>> {
    let highlighted =
        highlight_source_lines(std::path::Path::new(path), source, Style::new().fg(CODE));
    let last_line = start_line
        .saturating_add(u32::try_from(highlighted.len().saturating_sub(1)).unwrap_or(u32::MAX));
    let number_width = last_line.to_string().len().max(minimum_number_width);

    highlighted
        .into_iter()
        .enumerate()
        .map(|(offset, spans)| {
            let line_number = start_line.saturating_add(u32::try_from(offset).unwrap_or(u32::MAX));
            let mut visible = clip_source_spans(spans, usize::from(horizontal));
            visible.insert(
                0,
                Span::styled(
                    format!("{line_number:>number_width$} | "),
                    Style::new().fg(MUTED),
                ),
            );
            Line::from(visible)
        })
        .collect()
}

fn clip_source_spans(spans: Vec<Span<'static>>, horizontal: usize) -> Vec<Span<'static>> {
    let mut remaining = horizontal;
    spans
        .into_iter()
        .filter_map(|span| {
            let safe = terminal_safe_text(span.content.as_ref());
            if remaining == 0 {
                return Some(Span::styled(safe, span.style));
            }
            let character_count = safe.chars().count();
            if remaining >= character_count {
                remaining -= character_count;
                return None;
            }
            let visible = safe.chars().skip(remaining).collect::<String>();
            remaining = 0;
            Some(Span::styled(visible, span.style))
        })
        .collect()
}

fn source_horizontal_limit(viewer: &EvidenceViewer, width: u16) -> u16 {
    let line_count = viewer.content().lines().count();
    let last_line = viewer
        .content_start_line()
        .saturating_add(u32::try_from(line_count.saturating_sub(1)).unwrap_or(u32::MAX));
    let gutter_width = last_line.to_string().len().saturating_add(3);
    let source_width = usize::from(width).saturating_sub(gutter_width);
    let longest = viewer
        .content()
        .lines()
        .map(|line| line.trim_end_matches('\r').chars().count())
        .max()
        .unwrap_or(0);
    u16::try_from(longest.saturating_sub(source_width)).unwrap_or(u16::MAX)
}

fn render_compact(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    let mut lines = vec![Line::styled(
        format!("CodeAtlas [{}]", activity_label(app.activity())),
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
    )];
    if area.height > 1 {
        lines.push(Line::styled(
            "Terminal too small; input remains active.",
            Style::new().fg(MUTED),
        ));
    }
    if area.height > 2 {
        let detail = app
            .error()
            .map_or("Resize to at least 24x8.", |error| error.message.as_str());
        lines.push(Line::from(detail.to_owned()));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_header(frame: &mut Frame<'_>, app: &TuiApp, area: Rect, mode: LayoutMode) {
    if area.is_empty() {
        return;
    }
    if mode == LayoutMode::Tabbed {
        let titles = ["Repository / Code Map", "Conversation", "Source Evidence"]
            .into_iter()
            .map(Line::from);
        let tabs = Tabs::new(titles)
            .select(app.focused_panel().index())
            .divider(" | ")
            .style(Style::new().fg(MUTED))
            .highlight_style(
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            );
        frame.render_widget(tabs, area);
        return;
    }

    let repository = if app.repository().path.is_empty() {
        "<not indexed>"
    } else {
        app.repository().path.as_str()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" CodeAtlas ", Style::new().fg(Color::Black).bg(ACCENT)),
            Span::styled(" repository: ", Style::new().fg(MUTED)),
            Span::styled(repository.to_owned(), Style::new().fg(Color::White)),
            Span::styled(
                format!(
                    "  focus: {:?}  [Tab] panel  [i] index  [a] ask  [N] new  [h] history",
                    app.focused_panel()
                ),
                Style::new().fg(MUTED),
            ),
        ])),
        area,
    );
}

fn render_wide(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let [repository, conversation, evidence] = Layout::horizontal([
        Constraint::Percentage(27),
        Constraint::Percentage(46),
        Constraint::Percentage(27),
    ])
    .areas(area);
    render_repository(frame, app, repository);
    render_conversation(frame, app, conversation);
    render_evidence(frame, app, evidence);
}

fn render_tabbed(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    match app.focused_panel() {
        Panel::Repository => render_repository(frame, app, area),
        Panel::Conversation => render_conversation(frame, app, area),
        Panel::Evidence => render_evidence(frame, app, area),
    }
}

fn panel_block(title: impl std::fmt::Display, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(ACCENT)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Block::new()
        .borders(Borders::ALL)
        .title(Line::styled(
            format!(" {title} "),
            Style::new()
                .fg(if focused { ACCENT } else { Color::Gray })
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(style)
}

fn render_repository(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let task = app.selected_task_position().map_or_else(
        || "no task".to_owned(),
        |(current, total)| format!("T{current}/{total}"),
    );
    let block = panel_block(
        format!("Repository / Code Map | {task}"),
        app.focused_panel() == Panel::Repository,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let lines = repository_lines(app);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let line_count = paragraph.line_count(inner.width);
    let scroll = bounded_scroll(line_count, inner, app.repository_scroll(), false);
    app.set_repository_scroll(scroll);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn repository_lines(app: &TuiApp) -> Vec<Line<'static>> {
    let repository = app.repository();
    let mut lines = vec![
        labeled_line(
            "ROOT",
            if repository.path.is_empty() {
                "<not indexed>".to_owned()
            } else {
                repository.path.clone()
            },
        ),
        labeled_line(
            "MODEL",
            repository.repository_id.map_or_else(
                || "pending".to_owned(),
                |id| {
                    format!(
                        "{} files / {} symbols  #{}",
                        repository.file_count,
                        repository.symbol_count,
                        short_id(id)
                    )
                },
            ),
        ),
        labeled_line("SESSION", session_status(app)),
    ];

    append_repository_map(&mut lines, app);

    lines.push(section_line("EXPLORATION"));

    let progress = app.selected_progress().collect::<Vec<_>>();
    if progress.is_empty() {
        lines.push(Line::styled(
            "  No runtime progress yet",
            Style::new().fg(MUTED),
        ));
    } else {
        for entry in progress {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<10}", phase_label(entry.progress.phase)),
                    Style::new().fg(ACCENT),
                ),
                Span::raw(entry.progress.message.clone()),
            ]));
        }
    }

    lines.push(section_line("TOOL TRACE"));
    let tools = app.selected_tools().collect::<Vec<_>>();
    if tools.is_empty() {
        lines.push(Line::styled("  No tool calls", Style::new().fg(MUTED)));
    }
    for (index, tool) in tools {
        let selected = app.selected_tool_index() == Some(index);
        let marker = match tool.status {
            ToolTraceStatus::Running => ">",
            ToolTraceStatus::Completed => "+",
            ToolTraceStatus::Failed => "!",
        };
        let style = match tool.status {
            ToolTraceStatus::Running => Style::new().fg(ACCENT),
            ToolTraceStatus::Completed => Style::new().fg(FACT),
            ToolTraceStatus::Failed => Style::new().fg(ERROR),
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { "> " } else { "  " }, Style::new().fg(ACCENT)),
            Span::styled(format!("{marker} "), style),
            Span::styled(
                tool.name.clone().map_or_else(
                    || "<unknown tool>".to_owned(),
                    |name| terminal_safe_text(&name),
                ),
                if selected {
                    Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::White)
                },
            ),
            Span::styled(
                format!(" #{}", short_id(tool.call_id)),
                Style::new().fg(MUTED),
            ),
        ]));
        if selected && app.preferences().show_tool_payloads {
            if let Some(arguments) = &tool.arguments {
                lines.push(payload_line("args", arguments));
            }
            if let Some(output) = &tool.output {
                lines.push(payload_line("out", output));
            }
        } else if selected {
            lines.push(Line::styled(
                "      [t] show arguments and output",
                Style::new().fg(MUTED),
            ));
        }
    }

    append_call_paths(&mut lines, app);
    lines
}

fn append_repository_map(lines: &mut Vec<Line<'static>>, app: &TuiApp) {
    let repository = app.repository();
    if repository.repository_id.is_none() {
        return;
    }
    lines.extend([
        section_line("CODE MAP"),
        labeled_line("NAME", repository.repository_map.name.clone()),
        labeled_line("LANG", repository_languages(app)),
        labeled_line(
            "STRUCT",
            format!(
                "{} modules / {} calls",
                repository.repository_map.module_count, repository.repository_map.call_count
            ),
        ),
        labeled_line(
            "LINKS",
            format!(
                "{} linked / {} unresolved",
                repository
                    .repository_map
                    .call_count
                    .saturating_sub(repository.repository_map.unresolved_call_count),
                repository.repository_map.unresolved_call_count
            ),
        ),
        section_line("MODULES"),
    ]);
    if repository.repository_map.modules.is_empty() {
        lines.push(Line::styled(
            "  No modules detected",
            Style::new().fg(MUTED),
        ));
    } else {
        for module in &repository.repository_map.modules {
            let name = module_display_name(&module.name, module.path.as_str());
            let compact_path = compact_repository_path(module.path.as_str());
            let mut spans = vec![
                Span::styled("  + ", Style::new().fg(FACT)),
                Span::styled(name.clone(), Style::new().fg(Color::White)),
            ];
            if !compact_path.starts_with(&format!("{name}/")) {
                spans.push(Span::styled(
                    format!("  {compact_path}"),
                    Style::new().fg(MUTED),
                ));
            }
            lines.push(Line::from(spans));
        }
        if repository.repository_map.modules_truncated {
            lines.push(Line::styled("  ... more modules", Style::new().fg(MUTED)));
        }
    }

    lines.push(section_line("ENTRY POINTS"));
    if repository.repository_map.entry_points.is_empty() {
        lines.push(Line::styled(
            "  No entry points detected",
            Style::new().fg(MUTED),
        ));
        return;
    }
    for entry in &repository.repository_map.entry_points {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<8}", entry_point_label(&entry.kind)),
                Style::new().fg(INFERENCE),
            ),
            Span::styled(
                entry_point_detail(&entry.kind, &entry.label, entry.path.as_str(), entry.line),
                Style::new().fg(Color::White),
            ),
        ]));
    }
    if repository.repository_map.entry_points_truncated {
        lines.push(Line::styled(
            "  ... more entry points",
            Style::new().fg(MUTED),
        ));
    }
}

fn session_status(app: &TuiApp) -> String {
    match (app.repository().repository_id, app.session_repository_id()) {
        (Some(indexed), Some(session_repository)) if indexed != session_repository => format!(
            "#{} [read-only; needs repo #{}]",
            short_id(app.session_id()),
            short_id(session_repository)
        ),
        _ => format!("#{}", short_id(app.session_id())),
    }
}

fn append_call_paths(lines: &mut Vec<Line<'static>>, app: &TuiApp) {
    let paths: Vec<_> = app
        .selected_answers()
        .flat_map(|answer| answer.call_paths.iter())
        .collect();
    if paths.is_empty() {
        return;
    }
    lines.push(section_line("CALL PATHS"));
    for path in paths {
        let label = path.label.as_deref().unwrap_or("trace");
        let completion = if path.complete { "complete" } else { "partial" };
        lines.push(Line::from(vec![
            Span::styled(format!("  {label}"), Style::new().fg(INFERENCE)),
            Span::styled(format!(" [{completion}]"), Style::new().fg(MUTED)),
        ]));
        for (index, step) in path.steps.iter().enumerate() {
            let target = match &step.target {
                TargetResolution::Resolved(symbol_id) => step
                    .evidence_ids
                    .iter()
                    .find_map(|id| app.evidence().iter().find(|evidence| evidence.id == *id))
                    .or_else(|| {
                        app.evidence()
                            .iter()
                            .find(|evidence| evidence.symbol_id == Some(*symbol_id))
                    })
                    .map_or_else(
                        || format!("symbol #{}", short_id(symbol_id)),
                        |evidence| {
                            format!(
                                "{}:{}  #{}",
                                evidence.path,
                                evidence.span.start().line(),
                                short_id(symbol_id)
                            )
                        },
                    ),
                TargetResolution::Unresolved(target) => target.reason.as_ref().map_or_else(
                    || format!("{} [unresolved]", target.name),
                    |reason| format!("{} [unresolved: {reason}]", target.name),
                ),
            };
            lines.push(Line::styled(
                format!("    {:02} {target}", index + 1),
                Style::new().fg(CODE),
            ));
        }
    }
}

fn render_conversation(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let block = panel_block("Conversation", app.focused_panel() == Panel::Conversation);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let lines = conversation_lines(app);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let line_count = paragraph.line_count(inner.width);
    let requested_scroll = app.conversation_scroll();
    let max_scroll = scroll_limit(line_count, inner);
    let follow_tail = app.conversation_follows_tail() || requested_scroll >= max_scroll;
    let scroll = bounded_scroll(line_count, inner, requested_scroll, follow_tail);
    app.set_conversation_scroll(scroll);
    app.set_conversation_follow_tail(follow_tail);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn conversation_lines(app: &TuiApp) -> Vec<Line<'static>> {
    if app.conversation().is_empty() {
        return vec![
            Line::styled("Ask how the codebase works.", Style::new().fg(Color::White)),
            Line::styled(
                "Answers separate facts, inferences, unknowns, and evidence.",
                Style::new().fg(MUTED),
            ),
        ];
    }

    let mut lines = Vec::new();
    let mut claim_numbers = ClaimNumbers::default();
    for entry in app.conversation() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        match entry {
            ConversationEntry::Question { request_id, text } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "QUESTION ",
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("#{}", short_id(request_id)), Style::new().fg(MUTED)),
                ]));
                append_text_lines(&mut lines, text, Style::new().fg(Color::White), "  ");
            }
            ConversationEntry::Answer(answer) => {
                append_answer(&mut lines, app, answer, &mut claim_numbers);
            }
        }
    }
    lines
}

fn append_answer(
    lines: &mut Vec<Line<'static>>,
    app: &TuiApp,
    answer: &AnswerView,
    claim_numbers: &mut ClaimNumbers,
) {
    let state = if answer.complete {
        "complete"
    } else {
        "streaming"
    };
    lines.push(Line::from(vec![
        Span::styled(
            "CODEATLAS ",
            Style::new().fg(FACT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "#{} [{state}{}]",
                short_id(answer.request_id),
                if app.selected_task() == Some(answer.request_id) {
                    " selected"
                } else {
                    ""
                }
            ),
            Style::new().fg(MUTED),
        ),
    ]));
    if answer.text.is_empty() {
        lines.push(Line::styled(
            "  Waiting for answer...",
            Style::new().fg(MUTED),
        ));
    } else {
        append_markdown_lines(lines, &answer.text, "  ");
    }

    if answer.complete {
        for claim in &answer.claims {
            let label = claim_numbers.next_label(claim.kind);
            let references = format_evidence_references(app, &claim.evidence_ids);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("[{label}]"),
                    claim_style(claim.kind).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {references} "), Style::new().fg(MUTED)),
                Span::raw(claim.text.clone()),
            ]));
        }
    }
    append_diagram(lines, app, answer.request_id, answer.diagram.as_ref());
    if !answer.call_paths.is_empty() {
        lines.push(Line::styled(
            format!(
                "[TRACE] {} call path(s); details in Repository",
                answer.call_paths.len()
            ),
            Style::new().fg(INFERENCE),
        ));
    }
}

fn append_diagram(
    lines: &mut Vec<Line<'static>>,
    app: &TuiApp,
    request_id: codeatlas_core::RequestId,
    decision: Option<&DiagramDecision>,
) {
    let Some(decision) = decision else {
        return;
    };
    match decision {
        DiagramDecision::NotNeeded { reason } => lines.push(Line::styled(
            format!("[DIAGRAM] not needed: {}", terminal_safe_text(reason)),
            Style::new().fg(MUTED),
        )),
        DiagramDecision::Needed { reason, diagram } => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("[DIAGRAM: {}] ", diagram_kind_label(diagram.kind)),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    terminal_safe_text(&diagram.title),
                    Style::new().fg(Color::White),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  WHY ", Style::new().fg(MUTED)),
                Span::styled(terminal_safe_text(reason), Style::new().fg(Color::White)),
            ]));
            lines.push(Line::styled(
                format!(
                    "  {} nodes / {} edges",
                    diagram.nodes.len(),
                    diagram.edges.len()
                ),
                Style::new().fg(MUTED),
            ));
            if let Some(artifact) = &diagram.artifact {
                lines.push(Line::from(vec![
                    Span::styled("  SVG ", Style::new().fg(MUTED)),
                    Span::styled(
                        format!("#{} ready", short_id(artifact.id)),
                        Style::new().fg(CODE),
                    ),
                    Span::styled("  [o] open  [y] copy path", Style::new().fg(ACCENT)),
                ]));
                if let Some(status) = app.diagram_open_status(request_id) {
                    lines.push(Line::styled(
                        format!("  {}", terminal_safe_text(status)),
                        Style::new().fg(MUTED),
                    ));
                }
            } else {
                lines.push(Line::styled("  SVG unavailable", Style::new().fg(MUTED)));
            }

            let evidence_ids = diagram_evidence_ids(diagram);
            if !evidence_ids.is_empty() {
                lines.push(Line::styled(
                    format!(
                        "  grounded by {}",
                        format_evidence_references(app, &evidence_ids)
                    ),
                    Style::new().fg(MUTED),
                ));
            }
        }
    }
}

fn diagram_evidence_ids(diagram: &Diagram) -> Vec<EvidenceId> {
    let mut ids = Vec::with_capacity(MAX_DIAGRAM_EVIDENCE_REFERENCES);
    let references = diagram
        .nodes
        .iter()
        .flat_map(|node| &node.evidence_ids)
        .chain(diagram.edges.iter().flat_map(|edge| &edge.evidence_ids));
    for &id in references {
        if !ids.contains(&id) {
            ids.push(id);
            if ids.len() == MAX_DIAGRAM_EVIDENCE_REFERENCES {
                break;
            }
        }
    }
    ids
}

fn terminal_safe_text(value: &str) -> String {
    let mut safe = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            safe.extend(character.escape_default());
        } else {
            safe.push(character);
        }
    }
    safe
}

const fn diagram_kind_label(kind: DiagramKind) -> &'static str {
    match kind {
        DiagramKind::Architecture => "ARCHITECTURE",
        DiagramKind::Flow => "FLOW",
        DiagramKind::Relationship => "RELATIONSHIP",
    }
}

fn format_evidence_references(app: &TuiApp, ids: &[codeatlas_core::EvidenceId]) -> String {
    if ids.is_empty() {
        return "[no evidence]".to_owned();
    }
    let references = ids
        .iter()
        .map(|id| {
            evidence_number(app.evidence(), *id).map_or_else(
                || format!("#{}", short_id(id)),
                |number| format!("E{number}"),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{references}]")
}

fn render_evidence(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let block = panel_block(
        format!(
            "Source Evidence | {} cited [Enter/v view]",
            app.evidence().len()
        ),
        app.focused_panel() == Panel::Evidence,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    if app.evidence().is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("No source evidence yet.", Style::new().fg(MUTED)),
                Line::styled(
                    "Only evidence cited by the completed answer appears here.",
                    Style::new().fg(MUTED),
                ),
            ])
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    if inner.height < 6 {
        render_evidence_detail(frame, app, inner, false);
        return;
    }
    let wanted = u16::try_from(app.evidence().len().saturating_add(1)).unwrap_or(u16::MAX);
    let list_height = wanted.min((inner.height / 3).max(3));
    let [list_area, detail_area] = Layout::new(
        Direction::Vertical,
        [Constraint::Length(list_height), Constraint::Min(1)],
    )
    .areas(inner);
    render_evidence_list(frame, app, list_area);
    render_evidence_detail(frame, app, detail_area, true);
}

fn render_evidence_list(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let items = app.evidence().iter().enumerate().map(|(index, evidence)| {
        let contexts = app.evidence_claim_contexts(evidence.id);
        let labels = if contexts.is_empty() {
            "TRACE".to_owned()
        } else {
            contexts
                .iter()
                .map(|context| context.label.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        ListItem::new(Line::from(vec![
            Span::styled(format!("[{labels}] "), Style::new().fg(FACT)),
            Span::styled(format!("E{} ", index + 1), Style::new().fg(ACCENT)),
            Span::raw(evidence_location(evidence)),
        ]))
    });
    let list = List::new(items).highlight_symbol("> ").highlight_style(
        Style::new()
            .fg(Color::White)
            .bg(Color::Rgb(32, 52, 58))
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default().with_selected(app.selected_evidence_index());
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_evidence_detail(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect, bordered: bool) {
    let Some(evidence) = app.selected_evidence().cloned() else {
        return;
    };
    let block = if bordered {
        Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(Color::DarkGray))
            .title(Line::styled(
                " selected excerpt - Enter/v view ",
                Style::new().fg(MUTED),
            ))
    } else {
        Block::new()
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let mut lines = vec![Line::styled(
        evidence_location(&evidence),
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
    )];
    if let Some(symbol_id) = evidence.symbol_id {
        lines.push(Line::from(vec![
            Span::styled("symbol ", Style::new().fg(MUTED)),
            Span::styled(
                format!("#{}", short_id(symbol_id)),
                Style::new().fg(INFERENCE),
            ),
        ]));
    }
    let contexts = app.evidence_claim_contexts(evidence.id);
    for context in contexts {
        lines.push(Line::from(vec![
            Span::styled(format!("[{}] ", context.label), claim_style(context.kind)),
            Span::raw(terminal_safe_text(&context.text)),
        ]));
    }
    lines.push(Line::default());
    match &evidence.excerpt {
        Some(excerpt) if !excerpt.is_empty() => {
            lines.extend(numbered_source_lines(
                evidence.path.as_str(),
                excerpt,
                evidence.span.start().line(),
                5,
                0,
            ));
        }
        _ => lines.push(Line::styled(
            "<excerpt unavailable>",
            Style::new().fg(MUTED),
        )),
    }

    let wraps = app.preferences().wrap_evidence;
    let paragraph = if wraps {
        Paragraph::new(lines).wrap(Wrap { trim: false })
    } else {
        Paragraph::new(lines)
    };
    let line_count = paragraph.line_count(inner.width);
    let scroll = bounded_scroll(line_count, inner, app.evidence_scroll(), false);
    app.set_evidence_scroll(scroll);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn render_input(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    let (title, content, active) = match app.input_mode() {
        InputMode::RepositoryPath => (
            "Repository path - Enter: Index / Esc: close",
            app.repository_input(),
            true,
        ),
        InputMode::Question => (
            "Question - Enter: Ask / Esc: close",
            app.question_input(),
            true,
        ),
        InputMode::Navigation => ("Keys", app.question_input(), false),
    };

    if area.height < 3 {
        let text = if active {
            content.visible_with_cursor(usize::from(area.width))
        } else {
            navigation_help(app, true)
        };
        frame.render_widget(Paragraph::new(text).style(Style::new().fg(MUTED)), area);
        return;
    }

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(if active { ACCENT } else { Color::DarkGray }))
        .title(Line::styled(
            format!(" {title} "),
            Style::new().fg(if active { ACCENT } else { MUTED }),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = if active {
        content.visible_with_cursor(usize::from(inner.width))
    } else {
        navigation_help(app, false)
    };
    frame.render_widget(Paragraph::new(text), inner);
}

fn render_status(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    if area.height == 1
        && let Some(error) = app.error()
    {
        frame.render_widget(
            Paragraph::new(format!(
                " ERROR {}: {} [x details]",
                error.code, error.message
            ))
            .style(Style::new().fg(ERROR)),
            area,
        );
        return;
    }
    let progress = app.last_progress();
    let progress_count = progress.map_or_else(String::new, |progress| {
        match (progress.completed, progress.total) {
            (Some(completed), Some(total)) if total > 0 => format!(
                " {completed}/{total} {}%",
                completed.saturating_mul(100) / total
            ),
            (Some(completed), _) => format!(" {completed}"),
            _ => String::new(),
        }
    });
    let usage = app.total_token_usage();
    let compact_cost = app.total_cost().map_or_else(
        || "Cost:-".to_owned(),
        |cost| {
            if cost.amount.is_finite() {
                format!(
                    "Cost:{}{:.4}{}",
                    if cost.estimated { "~" } else { "" },
                    cost.amount,
                    cost.currency
                )
            } else {
                "Cost:?".to_owned()
            }
        },
    );
    let status = if area.width >= 110 {
        format!(
            " {:<11}{} | files {} | symbols {} | tokens {} (in {} / out {} / cached {}) | {} ",
            activity_label(app.activity()),
            progress_count,
            app.repository().file_count,
            app.repository().symbol_count,
            compact_count(usage.total_tokens),
            compact_count(usage.input_tokens),
            compact_count(usage.output_tokens),
            compact_count(usage.cached_input_tokens),
            compact_cost,
        )
    } else {
        format!(
            " {}{} | F{} S{} | T{} I{} O{} C{} | {} ",
            activity_label(app.activity()),
            progress_count,
            compact_count(app.repository().file_count),
            compact_count(app.repository().symbol_count),
            compact_count(usage.total_tokens),
            compact_count(usage.input_tokens),
            compact_count(usage.output_tokens),
            compact_count(usage.cached_input_tokens),
            compact_cost,
        )
    };
    let mut lines = vec![Line::styled(
        status,
        Style::new()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
    )];
    if area.height > 1 {
        lines.push(status_detail_line(app, progress));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn status_detail_line(app: &TuiApp, progress: Option<&codeatlas_core::Progress>) -> Line<'static> {
    if let Some(error) = app.error() {
        let retry = if error.retryable { " [retryable]" } else { "" };
        Line::styled(
            format!(
                " ERROR {}: {}{retry} [x details]",
                error.code, error.message
            ),
            Style::new().fg(ERROR),
        )
    } else if let Some(status) = app.clipboard_status() {
        Line::styled(format!(" > {status}"), Style::new().fg(ACCENT))
    } else if let Some(progress) = progress {
        Line::from(vec![
            Span::styled(" > ", Style::new().fg(ACCENT)),
            Span::raw(progress.message.clone()),
        ])
    } else {
        Line::styled(
            " Ready: [i] index, [a] ask, [N] new session, [h] history, [Tab] focus",
            Style::new().fg(MUTED),
        )
    }
}

fn bounded_scroll(line_count: usize, area: Rect, requested: u16, follow_tail: bool) -> u16 {
    let max_scroll = scroll_limit(line_count, area);
    if follow_tail {
        max_scroll
    } else {
        requested.min(max_scroll)
    }
}

fn scroll_limit(line_count: usize, area: Rect) -> u16 {
    let max_scroll = line_count.saturating_sub(usize::from(area.height));
    u16::try_from(max_scroll).unwrap_or(u16::MAX)
}

fn claim_style(kind: ClaimKind) -> Style {
    match kind {
        ClaimKind::Fact => Style::new().fg(FACT),
        ClaimKind::Inference => Style::new().fg(INFERENCE),
        ClaimKind::Unknown => Style::new().fg(UNKNOWN),
    }
}

fn evidence_location(evidence: &Evidence) -> String {
    let (start, end) = evidence_line_range(evidence);
    source_location(&evidence.path, start, end)
}

fn source_location(path: impl Display, start: u32, end: u32) -> String {
    if start == end {
        format!("{path}:{start}")
    } else {
        format!("{path}:{start}-{end}")
    }
}

fn navigation_help(app: &TuiApp, compact: bool) -> String {
    match app.focused_panel() {
        Panel::Repository => {
            if compact {
                "i index | a/? ask | t details | [/] task | j/k scroll | n/p tool | q quit"
                    .to_owned()
            } else {
                "i index | a/? ask | N new | h history | t details | [/] task | Tab panels | j/k scroll | n/p tool | q quit".to_owned()
            }
        }
        Panel::Conversation => {
            if compact {
                "a/? ask | N new | h history | o diagram | [/] task | Tab panels | j/k scroll | q quit"
                    .to_owned()
            } else {
                "a/? ask | N new | h history | o/y diagram | [/] task | c cancel | Tab panels | j/k scroll | q quit".to_owned()
            }
        }
        Panel::Evidence => {
            if compact {
                "Enter/v view | a/? ask | N new | h history | [/] task | j/k select | q quit"
                    .to_owned()
            } else {
                "Enter/v view | o/y diagram | a/? ask | N new | h history | [/] task | c cancel | Tab panels | j/k select | q quit".to_owned()
            }
        }
    }
}

fn repository_languages(app: &TuiApp) -> String {
    let languages = &app.repository().repository_map.languages;
    if languages.is_empty() {
        return "none detected".to_owned();
    }
    languages
        .iter()
        .map(|language| {
            format!(
                "{} {}",
                language_label(&language.language),
                language.file_count
            )
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

fn language_label(language: &Language) -> &str {
    match language {
        Language::Rust => "Rust",
        Language::Python => "Python",
        Language::C => "C",
        Language::Cpp => "C++",
        Language::Java => "Java",
        Language::JavaScript => "JavaScript",
        Language::TypeScript => "TypeScript",
        Language::Go => "Go",
        Language::Other(name) => name,
    }
}

fn entry_point_label(kind: &EntryPointKind) -> &str {
    match kind {
        EntryPointKind::Executable => "bin",
        EntryPointKind::Library => "lib",
        EntryPointKind::Test => "test",
        EntryPointKind::Benchmark => "bench",
        EntryPointKind::WebRoute => "route",
        EntryPointKind::BackgroundTask => "worker",
        EntryPointKind::Other(name) => name,
    }
}

fn compact_repository_path(path: &str) -> &str {
    path.strip_prefix("crates/").unwrap_or(path)
}

fn module_display_name(name: &str, path: &str) -> String {
    if name == "src" {
        return compact_repository_path(path)
            .split('/')
            .next()
            .unwrap_or(name)
            .to_owned();
    }
    name.to_owned()
}

fn entry_point_detail(kind: &EntryPointKind, label: &str, path: &str, line: u32) -> String {
    let location = format!("{}:{line}", compact_repository_path(path));
    if matches!(kind, EntryPointKind::Library)
        && path.starts_with("crates/")
        && path.ends_with("/src/lib.rs")
    {
        return compact_repository_path(path)
            .split('/')
            .next()
            .unwrap_or(&location)
            .to_owned();
    }
    if matches!(label, "library crate" | "executable") {
        location
    } else {
        format!("{label}  {location}")
    }
}

fn labeled_line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<7}"), Style::new().fg(MUTED)),
        Span::styled(value, Style::new().fg(Color::White)),
    ])
}

fn section_line(label: &str) -> Line<'static> {
    Line::styled(
        format!("-- {label} --"),
        Style::new().fg(MUTED).add_modifier(Modifier::BOLD),
    )
}

fn payload_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("      {label}: "), Style::new().fg(MUTED)),
        Span::styled(terminal_safe_text(value), Style::new().fg(CODE)),
    ])
}

fn append_text_lines(lines: &mut Vec<Line<'static>>, text: &str, style: Style, prefix: &str) {
    for line in text.lines() {
        lines.push(Line::from(vec![
            Span::raw(prefix.to_owned()),
            Span::styled(line.to_owned(), style),
        ]));
    }
}

fn append_markdown_lines(lines: &mut Vec<Line<'static>>, markdown: &str, prefix: &str) {
    let source: Vec<&str> = markdown.lines().collect();
    let mut index = 0;
    let mut code_fence = None;

    while index < source.len() {
        let raw = source[index].trim_end_matches('\r');
        let trimmed = raw.trim_start();

        if let Some(marker) = code_fence {
            if fence(trimmed)
                .is_some_and(|(candidate, info)| candidate == marker && info.is_empty())
            {
                code_fence = None;
            } else {
                lines.push(Line::from(vec![
                    Span::raw(prefix.to_owned()),
                    Span::styled(
                        format!("  {raw}"),
                        Style::new().fg(CODE).bg(Color::Rgb(24, 31, 34)),
                    ),
                ]));
            }
            index += 1;
            continue;
        }

        if let Some((marker, language)) = fence(trimmed) {
            code_fence = Some(marker);
            if !language.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw(prefix.to_owned()),
                    Span::styled(format!("[{language}]"), Style::new().fg(MUTED)),
                ]));
            }
            index += 1;
            continue;
        }

        if let Some((table, consumed)) = markdown_table(&source[index..]) {
            append_markdown_table(lines, prefix, &table);
            index += consumed;
            continue;
        }

        if trimmed.is_empty() {
            lines.push(Line::default());
        } else if let Some((level, heading)) = markdown_heading(trimmed) {
            let style = if level <= 2 {
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::new().fg(FACT).add_modifier(Modifier::BOLD)
            };
            lines.push(markdown_line(prefix, "", heading, style));
        } else if let Some((marker, item)) = markdown_list_item(raw) {
            lines.push(markdown_line(
                prefix,
                &marker,
                item,
                Style::new().fg(Color::White),
            ));
        } else if let Some((depth, quote)) = markdown_quote(trimmed) {
            let marker = "| ".repeat(depth);
            lines.push(markdown_line(
                prefix,
                &marker,
                quote,
                Style::new().fg(MUTED).add_modifier(Modifier::ITALIC),
            ));
        } else if is_horizontal_rule(trimmed) {
            lines.push(Line::from(vec![
                Span::raw(prefix.to_owned()),
                Span::styled("------------------------", Style::new().fg(MUTED)),
            ]));
        } else if let Some(code) = raw.strip_prefix("    ") {
            lines.push(Line::from(vec![
                Span::raw(prefix.to_owned()),
                Span::styled(
                    format!("  {code}"),
                    Style::new().fg(CODE).bg(Color::Rgb(24, 31, 34)),
                ),
            ]));
        } else {
            lines.push(markdown_line(
                prefix,
                "",
                raw,
                Style::new().fg(Color::White),
            ));
        }
        index += 1;
    }
}

fn fence(line: &str) -> Option<(char, &str)> {
    let marker = line.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let marker_count = line
        .chars()
        .take_while(|candidate| *candidate == marker)
        .count();
    (marker_count >= 3).then(|| (marker, line[marker_count..].trim()))
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    let content = line[level + 1..].trim_end();
    let without_closing_hashes = content.trim_end_matches('#');
    let content = if without_closing_hashes.len() < content.len()
        && without_closing_hashes
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    {
        without_closing_hashes.trim_end()
    } else {
        content
    };
    Some((level, content))
}

fn markdown_list_item(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    let indentation = " ".repeat(line.len().saturating_sub(trimmed.len()).min(8));
    for prefix in ["- ", "* ", "+ "] {
        if let Some(item) = trimmed.strip_prefix(prefix) {
            let (task, item) = markdown_task(item);
            return Some((format!("{indentation}- {task}"), item));
        }
    }

    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        let suffix = &trimmed[digits..];
        if let Some(item) = suffix
            .strip_prefix(". ")
            .or_else(|| suffix.strip_prefix(") "))
        {
            return Some((format!("{indentation}{}. ", &trimmed[..digits]), item));
        }
    }
    None
}

fn markdown_task(item: &str) -> (&'static str, &str) {
    if let Some(item) = item.strip_prefix("[ ] ") {
        ("[ ] ", item)
    } else if let Some(item) = item
        .strip_prefix("[x] ")
        .or_else(|| item.strip_prefix("[X] "))
    {
        ("[x] ", item)
    } else {
        ("", item)
    }
}

fn markdown_quote(mut line: &str) -> Option<(usize, &str)> {
    let mut depth = 0;
    while let Some(quote) = line.strip_prefix('>') {
        depth += 1;
        line = quote.strip_prefix(' ').unwrap_or(quote);
    }
    (depth > 0).then_some((depth, line))
}

fn is_horizontal_rule(line: &str) -> bool {
    let compact: String = line
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    compact.len() >= 3
        && compact.chars().next().is_some_and(|marker| {
            matches!(marker, '-' | '*' | '_') && compact.chars().all(|c| c == marker)
        })
}

fn markdown_table<'a>(source: &'a [&'a str]) -> Option<(Vec<Vec<&'a str>>, usize)> {
    let header = table_cells(source.first()?)?;
    let separator = table_cells(source.get(1)?)?;
    if separator.len() != header.len()
        || !separator.iter().all(|cell| {
            let rule = cell.trim().trim_matches(':');
            rule.len() >= 3 && rule.bytes().all(|byte| byte == b'-')
        })
    {
        return None;
    }

    let column_count = header.len();
    let mut rows = vec![header];
    let mut consumed = 2;
    for line in &source[2..] {
        let Some(row) = table_cells(line) else {
            break;
        };
        if row.len() != column_count {
            break;
        }
        rows.push(row);
        consumed += 1;
    }
    Some((rows, consumed))
}

fn table_cells(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let row = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or_else(|| trimmed.strip_prefix('|').unwrap_or(trimmed));
    let cells: Vec<&str> = row.split('|').map(str::trim).collect();
    (cells.len() >= 2).then_some(cells)
}

fn append_markdown_table(lines: &mut Vec<Line<'static>>, prefix: &str, rows: &[Vec<&str>]) {
    let Some(header) = rows.first() else {
        return;
    };
    lines.push(markdown_table_line(prefix, header, true));
    let mut separator = vec![Span::raw(prefix.to_owned())];
    for (index, cell) in header.iter().enumerate() {
        if index > 0 {
            separator.push(Span::styled("-+-", Style::new().fg(MUTED)));
        }
        let width = cell.chars().count().clamp(3, 24);
        separator.push(Span::styled("-".repeat(width), Style::new().fg(MUTED)));
    }
    lines.push(Line::from(separator));
    for row in &rows[1..] {
        lines.push(markdown_table_line(prefix, row, false));
    }
}

fn markdown_table_line(prefix: &str, cells: &[&str], header: bool) -> Line<'static> {
    let mut spans = vec![Span::raw(prefix.to_owned())];
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" | ", Style::new().fg(MUTED)));
        }
        let style = if header {
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::White)
        };
        append_markdown_inline(&mut spans, cell, style);
    }
    Line::from(spans)
}

fn markdown_line(prefix: &str, marker: &str, text: &str, style: Style) -> Line<'static> {
    let mut spans = vec![Span::raw(prefix.to_owned())];
    if !marker.is_empty() {
        spans.push(Span::styled(marker.to_owned(), Style::new().fg(MUTED)));
    }
    append_markdown_inline(&mut spans, text, style);
    Line::from(spans)
}

fn append_markdown_inline(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    let mut index = 0;
    let mut plain = String::new();
    while index < text.len() {
        let rest = &text[index..];
        if let Some(escaped) = rest.strip_prefix('\\').and_then(|tail| tail.chars().next()) {
            plain.push(escaped);
            index += escaped.len_utf8() + 1;
            continue;
        }

        if let Some((content, consumed, content_style)) = inline_delimited(rest, style) {
            flush_markdown_plain(spans, &mut plain, style);
            append_markdown_inline(spans, content, content_style);
            index += consumed;
            continue;
        }

        if let Some((label, target, consumed)) = markdown_link(rest) {
            flush_markdown_plain(spans, &mut plain, style);
            append_markdown_inline(
                spans,
                label,
                style.fg(ACCENT).add_modifier(Modifier::UNDERLINED),
            );
            spans.push(Span::styled(format!(" ({target})"), Style::new().fg(MUTED)));
            index += consumed;
            continue;
        }

        let character = rest.chars().next().expect("non-empty markdown remainder");
        plain.push(character);
        index += character.len_utf8();
    }
    flush_markdown_plain(spans, &mut plain, style);
}

fn inline_delimited(rest: &str, style: Style) -> Option<(&str, usize, Style)> {
    for (delimiter, modifier) in [
        ("**", Modifier::BOLD),
        ("~~", Modifier::CROSSED_OUT),
        ("*", Modifier::ITALIC),
    ] {
        if let Some(tail) = rest.strip_prefix(delimiter) {
            if let Some(end) = tail.find(delimiter).filter(|end| *end > 0) {
                let consumed = delimiter.len() * 2 + end;
                return Some((&tail[..end], consumed, style.add_modifier(modifier)));
            }
        }
    }

    let tail = rest.strip_prefix('`')?;
    let end = tail.find('`').filter(|end| *end > 0)?;
    Some((
        &tail[..end],
        end + 2,
        Style::new().fg(CODE).bg(Color::Rgb(24, 31, 34)),
    ))
}

fn markdown_link(rest: &str) -> Option<(&str, &str, usize)> {
    let label_end = rest.strip_prefix('[')?.find("](")?;
    let label = &rest[1..=label_end];
    let target_start = label_end + 3;
    let target_end = rest[target_start..].find(')')? + target_start;
    Some((label, &rest[target_start..target_end], target_end + 1))
}

fn flush_markdown_plain(spans: &mut Vec<Span<'static>>, plain: &mut String, style: Style) {
    if !plain.is_empty() {
        spans.push(Span::styled(std::mem::take(plain), style));
    }
}

fn short_id(id: impl Display) -> String {
    id.to_string().chars().take(8).collect()
}

const fn phase_label(phase: ProgressPhase) -> &'static str {
    match phase {
        ProgressPhase::Scanning => "scanning",
        ProgressPhase::Parsing => "parsing",
        ProgressPhase::Indexing => "indexing",
        ProgressPhase::Searching => "searching",
        ProgressPhase::Tracing => "tracing",
        ProgressPhase::Reading => "reading",
        ProgressPhase::Verifying => "verifying",
        ProgressPhase::Explaining => "explaining",
    }
}

const fn activity_label(activity: Activity) -> &'static str {
    match activity {
        Activity::Idle => "IDLE",
        Activity::IndexQueued => "INDEX QUEUED",
        Activity::AskQueued => "ASK QUEUED",
        Activity::Running(phase) => match phase {
            ProgressPhase::Scanning => "SCANNING",
            ProgressPhase::Parsing => "PARSING",
            ProgressPhase::Indexing => "INDEXING",
            ProgressPhase::Searching => "SEARCHING",
            ProgressPhase::Tracing => "TRACING",
            ProgressPhase::Reading => "READING",
            ProgressPhase::Verifying => "VERIFYING",
            ProgressPhase::Explaining => "EXPLAINING",
        },
        Activity::Cancelling => "CANCELLING",
        Activity::Indexed => "INDEXED",
        Activity::AnswerReady => "ANSWER READY",
        Activity::Cancelled => "CANCELLED",
        Activity::Error => "ERROR",
    }
}

fn compact_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{}.{}m", value / 1_000_000, (value % 1_000_000) / 100_000)
    } else if value >= 1_000 {
        format!("{}.{}k", value / 1_000, (value % 1_000) / 100)
    } else {
        value.to_string()
    }
}

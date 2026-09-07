use super::forms::{visible_text, Field, FieldValue, TextInput};
use super::{path_label, App, Dialog, Focus, SaveAction};
use crate::project::{InitializationReview, PathAvailability};
use ratatui::{
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame,
};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
const SELECTED: Color = Color::Rgb(25, 53, 65);

pub(super) fn render(app: &mut App<'_>, frame: &mut Frame<'_>) {
    let area = frame.area();
    if area.width < 24 || area.height < 8 {
        frame.render_widget(Paragraph::new("idk\nA larger terminal is needed.\nYour draft is kept.\nEsc: close dialog / screen"), area);
        return;
    }
    let zones = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(2),
    ])
    .split(area);
    let name = app
        .current_project()
        .map(|view| visible_text(&view.project.name))
        .unwrap_or_else(|| "Connect your project".into());
    frame.render_widget(
        Tabs::new(["1 Terminal", "2 Git", "3 Tasks", "4 Results"])
            .select(app.tab)
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .divider("  ")
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .title(format!(" idk · {name} ")),
            ),
        zones[0],
    );
    if app.terminal_connected && !app.menu_visible {
        frame.render_widget(Paragraph::new("Terminal input is focused.\nCtrl+g: project controls · Ctrl+g again: send Ctrl+g to the terminal"), zones[1]);
    } else if zones[1].width >= 86 {
        let columns =
            Layout::horizontal([Constraint::Length(29), Constraint::Min(1)]).split(zones[1]);
        projects(app, frame, columns[0]);
        content(app, frame, columns[1]);
    } else if app.focus == Focus::Projects {
        projects(app, frame, zones[1]);
    } else {
        content(app, frame, zones[1]);
    }
    let help = if area.width < 65 {
        "Tab: focus · F1: all actions · q: close"
    } else {
        "Tab: focus · p: projects · 1–4: areas · F5: refresh · Ctrl+g: menu · q: close"
    };
    frame.render_widget(
        Paragraph::new(help)
            .style(Style::default().fg(MUTED))
            .wrap(Wrap { trim: false }),
        zones[3],
    );
    if let Some(mut dialog) = app.dialog.take() {
        let popup_area = Rect {
            height: area.height.saturating_sub(4),
            ..area
        };
        dialog_view(app, &mut dialog, frame, popup_area);
        app.dialog = Some(dialog);
    }
    if let Some(notice) = &app.notice {
        frame.render_widget(
            Paragraph::new(visible_text(&notice.message))
                .style(Style::default().fg(if notice.error {
                    Color::LightRed
                } else {
                    Color::LightGreen
                }))
                .wrap(Wrap { trim: false }),
            zones[2],
        );
    }
}

fn panel(title: impl Into<String>, focused: bool) -> Block<'static> {
    Block::bordered()
        .title(format!(" {} ", title.into()))
        .border_style(Style::default().fg(if focused { ACCENT } else { MUTED }))
}

fn projects(app: &App<'_>, frame: &mut Frame<'_>, area: Rect) {
    let parts = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(if area.width < 50 { 3 } else { 2 }),
    ])
    .split(area);
    if app.view.projects.is_empty() {
        frame.render_widget(
            Paragraph::new("Connect an existing folder.\n\nn  Connect project\n\nYour source, .csh and Git files stay in place.")
                .wrap(Wrap { trim: false }).block(panel("Projects · recent", app.focus == Focus::Projects)),
            parts[0],
        );
    } else {
        let items: Vec<ListItem<'static>> = app
            .view
            .projects
            .iter()
            .map(|view| {
                let selected =
                    app.view.selected_project.as_deref() == Some(view.project.id.as_str());
                let status = path_label(&view.root);
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            if selected { "● " } else { "  " },
                            Style::default().fg(ACCENT),
                        ),
                        Span::raw(visible_text(&view.project.name)),
                    ]),
                    Line::from(Span::styled(
                        format!("  {status} · {} terminals", view.project.terminals.len()),
                        path_style(&view.root),
                    )),
                    Line::from(Span::styled(
                        format!("  {}", visible_text(&view.project.root.to_string_lossy())),
                        Style::default().fg(MUTED),
                    )),
                ])
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(app.project_index));
        frame.render_stateful_widget(
            List::new(items)
                .block(panel("Projects · recent", app.focus == Focus::Projects))
                .highlight_style(Style::default().bg(SELECTED).add_modifier(Modifier::BOLD)),
            parts[0],
            &mut state,
        );
    }
    let hints = if area.width < 50 {
        "n Connect · Enter Select\ne Environment · r Rename\nm Reconnect · F1 All actions"
    } else {
        "n Connect · Enter Select · e Environment · r Rename\nm Reconnect · g Repository · t Review scripts · Del Remove"
    };
    frame.render_widget(
        Paragraph::new(hints)
            .style(Style::default().fg(ACCENT))
            .wrap(Wrap { trim: false }),
        parts[1],
    );
}

fn content(app: &App<'_>, frame: &mut Frame<'_>, area: Rect) {
    let Some(project) = app.current_project() else {
        frame.render_widget(Paragraph::new("n  Connect a project\n\nUse an existing folder, your csh/tcsh, and the initialization scripts you already use.")
            .wrap(Wrap { trim: false }).block(panel("Start here", true)), area);
        return;
    };
    match app.tab {
        0 => {
            let detail_height = if area.height >= 15 {
                8
            } else if area.height >= 10 {
                4
            } else {
                0
            };
            let parts = Layout::vertical([
                Constraint::Min(2),
                Constraint::Length(detail_height),
                Constraint::Length(2),
            ])
            .split(area);
            let terminals = app.terminals();
            if terminals.is_empty() {
                frame.render_widget(
                    Paragraph::new(
                        "n  Add a terminal\n\nSeveral terminals may start in the same folder.",
                    )
                    .wrap(Wrap { trim: false })
                    .block(panel("Terminal definitions", app.focus == Focus::Content)),
                    parts[0],
                );
            } else {
                let items: Vec<ListItem<'static>> = terminals
                    .iter()
                    .map(|terminal| {
                        let availability = app.terminal_path(terminal);
                        let default = project.project.default_terminal.as_deref()
                            == Some(terminal.id.as_str());
                        ListItem::new(Line::from(vec![
                            Span::styled(
                                if default { "★ " } else { "  " },
                                Style::default().fg(ACCENT),
                            ),
                            Span::raw(visible_text(&terminal.name)),
                            Span::styled(
                                if terminal.persistent {
                                    " · saved"
                                } else {
                                    " · temporary"
                                },
                                Style::default().fg(MUTED),
                            ),
                            Span::styled(
                                format!(" · {}", path_label(&availability)),
                                path_style(&availability),
                            ),
                        ]))
                    })
                    .collect();
                let mut state = ListState::default().with_selected(Some(app.terminal_index));
                frame.render_stateful_widget(
                    List::new(items)
                        .block(panel(
                            format!("Terminal definitions · {}", terminals.len()),
                            app.focus == Focus::Content,
                        ))
                        .highlight_style(
                            Style::default().bg(SELECTED).add_modifier(Modifier::BOLD),
                        ),
                    parts[0],
                    &mut state,
                );
            }
            if let Some(terminal) = app.current_terminal() {
                let availability = app.terminal_path(&terminal);
                let mut lines = vec![
                    format!("Start: {}", terminal.cwd.display()),
                    format!(
                        "Initialize: {} ({})",
                        project.project.shell.init_cwd.display(),
                        path_label(&project.init_cwd)
                    ),
                ];
                if detail_height >= 8 {
                    lines.push("Live folder: unknown · terminal not open".into());
                    lines.push(format!(
                        "Primary Git: {}",
                        project
                            .project
                            .repository
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "not connected".into())
                    ));
                    lines.push(
                        if project.project.shell.trusted_digest.is_some() {
                            "Initialization review saved; t rechecks current scripts."
                        } else {
                            "Initialization needs review · t Review scripts"
                        }
                        .into(),
                    );
                }
                if let PathAvailability::Unavailable { message } = availability {
                    lines.push(message);
                }
                frame.render_widget(
                    Paragraph::new(
                        lines
                            .into_iter()
                            .map(|line| Line::from(visible_text(&line)))
                            .collect::<Vec<_>>(),
                    )
                    .wrap(Wrap { trim: false })
                    .block(panel("Selected definition", false)),
                    parts[1],
                );
            }
            let hints = if area.width < 65 {
                "n Add · e Edit · Enter Open · F1 Actions\ns Save temporary · d Default · Alt↑↓ Order"
            } else {
                "n Add · e Edit · c Copy · Enter Open · Del Remove\ns Save temporary · d Default · Alt↑↓ Order · t Review scripts"
            };
            frame.render_widget(
                Paragraph::new(hints)
                    .style(Style::default().fg(ACCENT))
                    .wrap(Wrap { trim: false }),
                parts[2],
            );
        }
        1 => {
            let parts = Layout::vertical([Constraint::Min(2), Constraint::Length(5)]).split(area);
            let roots = app.repositories();
            let items: Vec<ListItem<'static>> = roots
                .iter()
                .map(|root| {
                    let primary = project.project.repository.as_ref() == Some(root);
                    ListItem::new(vec![
                        Line::from(Span::styled(
                            if primary {
                                "Primary repository"
                            } else {
                                "Related repository"
                            },
                            Style::default().fg(ACCENT),
                        )),
                        Line::from(visible_text(&root.to_string_lossy())),
                    ])
                })
                .collect();
            if items.is_empty() {
                frame.render_widget(Paragraph::new("No Git repository is connected.\n\nn  Connect an existing repository\n\nProject and terminal definitions also work without Git.")
                    .wrap(Wrap { trim: false }).block(panel("Project Git target", app.focus == Focus::Content)), parts[0]);
            } else {
                let mut state = ListState::default()
                    .with_selected(Some(app.repository_index.min(items.len() - 1)));
                frame.render_stateful_widget(
                    List::new(items)
                        .block(panel("Project Git target", app.focus == Focus::Content))
                        .highlight_style(Style::default().bg(SELECTED)),
                    parts[0],
                    &mut state,
                );
            }
            frame.render_widget(Paragraph::new("n / g  Connect repository · Enter  Review selected target\nTerminal cd never changes this target.\n\nGit operations are not available in this build.")
                .wrap(Wrap { trim: false }), parts[1]);
        }
        2 | 3 => {
            let title = if app.tab == 2 { "Tasks" } else { "Results" };
            frame.render_widget(Paragraph::new(format!("{title} are not available in this build.\n\nNo work was started and no result has been recorded.\n\n1  Return to terminal definitions\np  Choose a project"))
                .wrap(Wrap { trim: false }).block(panel(title, app.focus == Focus::Content)), area);
        }
        _ => {}
    }
}

fn path_style(path: &PathAvailability) -> Style {
    Style::default().fg(match path {
        PathAvailability::Ready { .. } => Color::LightGreen,
        PathAvailability::Missing | PathAvailability::NotDirectory => Color::Yellow,
        PathAvailability::Denied | PathAvailability::Unavailable { .. } => Color::LightRed,
    })
}

fn popup(frame: &mut Frame<'_>, area: Rect, title: &str, width: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = area.height.saturating_sub(1).max(1);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = panel(title, true);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

fn dialog_view(app: &App<'_>, dialog: &mut Dialog, frame: &mut Frame<'_>, area: Rect) {
    match dialog {
        Dialog::Form(form) => {
            let inner = popup(frame, area, &form.title, 86);
            form_fields(
                frame,
                inner,
                &form.fields,
                form.selected,
                form.sources.len(),
                "F2 Review · Esc Keep draft · F4 Reset saved",
            );
        }
        Dialog::Source(source) => {
            let inner = popup(
                frame,
                area,
                if source.index.is_some() {
                    "Edit initialization script"
                } else {
                    "Add initialization script"
                },
                86,
            );
            form_fields(
                frame,
                inner,
                &source.fields,
                source.selected,
                0,
                "F2 Use · F3 Add arg · F4 Remove arg · Esc Cancel",
            );
        }
        Dialog::Sources(list) => {
            let inner = popup(frame, area, "Ordered initialization scripts", 86);
            let zones = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new(
                    "Scripts stay in their original locations. Each argument is a separate field.",
                )
                .wrap(Wrap { trim: false }),
                zones[0],
            );
            if list.sources.is_empty() {
                frame.render_widget(
                    Paragraph::new("No additional scripts.\n\na  Add a script"),
                    zones[1],
                );
            } else {
                let items: Vec<ListItem<'static>> = list
                    .sources
                    .iter()
                    .enumerate()
                    .map(|(index, source)| {
                        ListItem::new(vec![
                            Line::from(format!(
                                "{}. {}",
                                index + 1,
                                visible_text(&source.path.to_string_lossy())
                            )),
                            Line::from(Span::styled(
                                format!("   {} literal arguments", source.args.len()),
                                Style::default().fg(MUTED),
                            )),
                        ])
                    })
                    .collect();
                let mut state = ListState::default().with_selected(Some(list.selected));
                frame.render_stateful_widget(
                    List::new(items).highlight_style(Style::default().bg(SELECTED)),
                    zones[1],
                    &mut state,
                );
            }
            frame.render_widget(Paragraph::new("a Add · e / Enter Edit · Del Remove · u/d Order\nF2 Use list in draft · Esc Cancel list changes\nNothing is executed while editing.").wrap(Wrap { trim: false }), zones[2]);
        }
        Dialog::Review(review) => {
            let inner = popup(frame, area, &review.title, 90);
            let mut lines = review.lines.clone();
            let mut footer = "F2 Save definition · Esc Back · ↑↓ / PgUp/PgDn Scroll".to_owned();
            if let SaveAction::Connect(preview) = &review.action {
                if !preview.duplicates.is_empty() {
                    lines.push(format!(
                        "[{}] Keep a separate project for this folder (d toggles)",
                        if preview.allow_duplicate_root {
                            "x"
                        } else {
                            " "
                        }
                    ));
                    footer = "d Separate project · F2 Save · Esc Back · ↑↓ Scroll".into();
                }
            }
            scroll_text(frame, inner, lines, &mut review.scroll, &footer);
        }
        Dialog::Trust { review, scroll } => {
            let inner = popup(frame, area, "Review initialization", 94);
            scroll_text(
                frame,
                inner,
                trust_lines(app, review),
                scroll,
                "F2 Approve readable groups · Esc Cancel · ↑↓ Scroll",
            );
        }
        Dialog::TransientTrust(review) => {
            let inner = popup(frame, area, "Review temporary initialization", 94);
            let mut lines = vec![
                format!("Temporary terminal: {}", review.terminal.name),
                "Common initialization is already reviewed. This approval stays in memory.".into(),
                "No shell is started and this does not save the terminal for next time.".into(),
                String::new(),
            ];
            if let Some(error) = &review.scope.error {
                lines.push(error.clone());
            }
            for file in &review.scope.files {
                lines.push(format!("{} · {}", file.role, file.path.display()));
                for (index, argument) in file.args.iter().enumerate() {
                    lines.push(format!("  Argument {}: {argument:?}", index + 1));
                }
            }
            scroll_text(
                frame,
                inner,
                lines,
                &mut review.scroll,
                "F2 Approve temporary scripts · Esc Cancel · ↑↓ Scroll",
            );
        }
        Dialog::Help { scroll } => {
            let inner = popup(frame, area, "Project controls", 88);
            let lines = [
                "Projects: p or Tab focuses the recent-project list.",
                "  n Connect · Enter Select · r Rename · e Environment",
                "  m Reconnect moved folder · g Git binding · Del Remove",
                "",
                "Terminals: 1 opens terminal definitions.",
                "  n Add · e Edit · c Copy with a new identity · Del Remove",
                "  s Save temporary · d Choose default · Alt+↑/↓ Reorder",
                "  Enter requests open; unavailable runtime stays clearly unavailable.",
                "",
                "Git: 2 shows the primary and explicitly related repositories.",
                "  n / g Connect · Enter Review selected repository as primary",
                "  An empty primary path disconnects only its reference.",
                "",
                "t reviews the current shell/startup/scripts before approval.",
                "F5 refreshes definition and folder availability.",
                "Folder availability is separate from initialization approval.",
                "",
                "Forms: Tab/Shift+Tab or ↑/↓ changes field; F2 reviews.",
                "  Enter edits a script list or toggles a choice.",
                "  Esc keeps the main draft; F4 reloads saved values.",
                "  Home/End, Ctrl+A/E/U/K edit text; Korean text is preserved.",
                "Scripts: each argument is a literal field, not shell syntax.",
                "",
                "Temporary terminal definitions live only in this app until saved.",
                "A definition edit does not start/stop/cd an existing shell.",
                "",
                "When a real terminal is focused, keys go to that terminal.",
                "Ctrl+g opens controls; a second Ctrl+g forwards Ctrl+g.",
                "There is no terminal runtime attached in this build.",
                "",
                "q / Esc closes this screen. Closing controls is not killing a shell.",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            scroll_text(
                frame,
                inner,
                lines,
                scroll,
                "Esc / F1 Close help · ↑↓ / PgUp/PgDn Scroll",
            );
        }
    }
}

fn trust_lines(app: &App<'_>, review: &InitializationReview) -> Vec<String> {
    let mut lines = vec![
        "Only the readable initialization groups below can be approved.".into(),
        "Approval does not execute a script. Dynamic/nested sources cannot be fully tracked."
            .into(),
        format!("Startup home: {}", review.home.display()),
        String::new(),
    ];
    for scope in std::iter::once(&review.common).chain(&review.terminals) {
        let label = scope
            .terminal_id
            .as_ref()
            .map(|id| {
                app.terminals()
                    .iter()
                    .find(|terminal| &terminal.id == id)
                    .map(|terminal| terminal.name.clone())
                    .unwrap_or_else(|| "Terminal definition".into())
            })
            .unwrap_or_else(|| "Common environment".into());
        lines.push(format!(
            "{label} · {}",
            if scope.error.is_some() {
                "unavailable"
            } else if scope.trusted {
                "review still matches"
            } else {
                "review needed"
            }
        ));
        if let Some(error) = &scope.error {
            lines.push(format!("  {error}"));
        }
        for file in &scope.files {
            lines.push(format!(
                "  {} · {}{}",
                file.role,
                file.path.display(),
                if file.missing_optional {
                    " (optional; missing)"
                } else {
                    ""
                }
            ));
            if let Some(canonical) = &file.canonical {
                if canonical != &file.path {
                    lines.push(format!("    Resolves to {}", canonical.display()));
                }
            }
            for (index, argument) in file.args.iter().enumerate() {
                lines.push(format!("    Argument {}: {argument:?}", index + 1));
            }
        }
        lines.push(String::new());
    }
    lines
}

fn scroll_text(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: Vec<String>,
    offset: &mut usize,
    footer: &str,
) {
    let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);
    // Wrap before slicing: every reviewed byte remains reachable, even on a
    // narrow screen, and excessive Down presses cannot make Up feel stuck.
    let width = parts[0].width.max(1) as usize;
    let mut wrapped = Vec::new();
    for line in lines {
        let mut current = String::new();
        let mut columns = 0;
        for character in visible_text(&line).chars() {
            let cells = Line::from(character.to_string()).width();
            if columns + cells > width && !current.is_empty() {
                wrapped.push(std::mem::take(&mut current));
                columns = 0;
            }
            current.push(character);
            columns += cells;
        }
        wrapped.push(current);
    }
    *offset = (*offset).min(wrapped.len().saturating_sub(parts[0].height as usize));
    let visible = wrapped
        .into_iter()
        .skip(*offset)
        .take(parts[0].height as usize)
        .map(Line::from)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), parts[0]);
    frame.render_widget(
        Paragraph::new(footer)
            .style(Style::default().fg(ACCENT))
            .wrap(Wrap { trim: false }),
        parts[1],
    );
}

fn form_fields(
    frame: &mut Frame<'_>,
    area: Rect,
    fields: &[Field],
    selected: usize,
    sources: usize,
    footer: &str,
) {
    let parts = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(2),
    ])
    .split(area);
    let count = (parts[0].height / 3).max(1) as usize;
    let start = selected
        .saturating_add(1)
        .saturating_sub(count)
        .min(fields.len().saturating_sub(count));
    for (offset, (index, field)) in fields
        .iter()
        .enumerate()
        .skip(start)
        .take(count)
        .enumerate()
    {
        let rect = Rect {
            y: parts[0].y + offset as u16 * 3,
            height: 3.min(parts[0].height.saturating_sub(offset as u16 * 3)),
            ..parts[0]
        };
        if rect.height == 0 {
            break;
        }
        let active = index == selected;
        let block = panel(
            format!("{} · {}/{}", field.label, index + 1, fields.len()),
            active,
        );
        let inside = block.inner(rect);
        frame.render_widget(block, rect);
        match &field.value {
            FieldValue::Text(input) => {
                let (text, cursor) = input_window(input, inside.width);
                frame.render_widget(Paragraph::new(text), inside);
                if active && inside.width > 0 && inside.height > 0 {
                    frame.set_cursor_position(Position::new(inside.x + cursor, inside.y));
                }
            }
            FieldValue::Toggle(value) => frame.render_widget(
                Paragraph::new(if *value {
                    "[x] Yes · Space/Enter toggles"
                } else {
                    "[ ] No · Space/Enter toggles"
                }),
                inside,
            ),
            FieldValue::Sources => frame.render_widget(
                Paragraph::new(format!("{sources} scripts · Enter opens list")),
                inside,
            ),
        }
    }
    if let Some(field) = fields.get(selected) {
        frame.render_widget(
            Paragraph::new(field.help.as_str())
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: false }),
            parts[1],
        );
    }
    frame.render_widget(
        Paragraph::new(footer)
            .style(Style::default().fg(ACCENT))
            .wrap(Wrap { trim: false }),
        parts[2],
    );
}

fn input_window(input: &TextInput, width: u16) -> (String, u16) {
    if width == 0 {
        return (String::new(), 0);
    }
    let text = visible_text(&input.text);
    let cursor = visible_text(&input.text[..input.cursor]).len();
    let max_before = width.saturating_sub(1) as usize;
    let mut start = cursor;
    let mut columns = 0;
    for (index, character) in text[..cursor].char_indices().rev() {
        let cells = Line::from(character.to_string()).width();
        if columns + cells > max_before {
            break;
        }
        columns += cells;
        start = index;
    }
    let mut end = start;
    columns = 0;
    for (index, character) in text[start..].char_indices() {
        let cells = Line::from(character.to_string()).width();
        if columns + cells > width as usize {
            break;
        }
        columns += cells;
        end = start + index + character.len_utf8();
    }
    let column = Line::from(&text[start..cursor]).width().min(max_before) as u16;
    (text[start..end].to_owned(), column)
}

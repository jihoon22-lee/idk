//! Git views contain only escaped metadata and bounded, explicitly opened PTY screens.
use super::{
    forms::{visible_text, TextInput},
    git::{kind_label, GitDialog, Page},
    App,
};
use crate::git_wire::*;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
pub(super) fn render(app: &App<'_>, frame: &mut Frame<'_>, area: Rect) {
    let Some(target) = app.git_target() else {
        super::git_view::empty(frame,area,"No primary Git repository is connected.\nn / g: connect a repository · v: choose a related repository");
        return;
    };
    let Some(repo) = app.git.repos.get(&target) else {
        super::git_view::empty(frame, area, "Opening project Git repository…");
        return;
    };
    let parts = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(4),
    ])
    .split(area);
    let mut header = repo
        .snapshot
        .as_ref()
        .map(|reply| super::git_view::header(&reply.snapshot))
        .unwrap_or_else(|| {
            vec![
                Line::from(visible_text(&target.root.to_string_lossy())),
                Line::from("Reading Git status…"),
            ]
        });
    if let Some(first) = header.first_mut() {
        *first = Line::from(visible_text(&format!(
            "{} · {}",
            if repo
                .context
                .as_ref()
                .map(|context| context.primary)
                .unwrap_or_else(|| app.current_project().is_some_and(|project| project
                    .project
                    .repository
                    .as_ref()
                    == Some(&target.root)))
            {
                "Primary Git"
            } else {
                "Related Git"
            },
            target.root.display()
        )));
    }
    let gate = repo
        .snapshot
        .as_ref()
        .map(|snapshot| &snapshot.source_use)
        .or_else(|| repo.context.as_ref().map(|context| &context.source_use));
    let source = match gate {
        Some(gate) if !gate.provider_ready => "build activity unknown",
        Some(gate) if !gate.runs.is_empty() => "builds using worktree",
        _ => "shared worktree",
    };
    header.push(Line::from(format!(
        "{:?} · {source}{}",
        repo.page,
        if repo.busy > 0 { " · working…" } else { "" }
    )));
    frame.render_widget(
        Paragraph::new(header).style(Style::default().fg(Color::Cyan)),
        parts[0],
    );
    if let Some(error) = &repo.error {
        super::git_view::empty(frame, parts[1], error);
    } else {
        match repo.page {
            Page::Changes => {
                if let Some(snapshot) = &repo.snapshot {
                    super::git_view::changes(
                        frame,
                        parts[1],
                        &snapshot.snapshot,
                        repo.selected,
                        &repo.marked,
                    );
                }
            }
            Page::Diff => {
                if let Some(diff) = &repo.diff {
                    super::git_view::diff(frame, parts[1], diff, repo.diff_target, repo.scroll);
                }
            }
            Page::History => {
                super::git_view::history(frame, parts[1], &repo.history, repo.selected)
            }
            Page::Files => super::git_view::files(frame, parts[1], &repo.files, repo.scroll),
            Page::Branches => {
                super::git_view::branches(frame, parts[1], &repo.branches, repo.selected)
            }
            Page::Remotes => {
                super::git_view::remotes(frame, parts[1], &repo.remotes, repo.selected)
            }
            Page::Operations => {
                let items: Vec<ListItem<'static>> = app
                    .git
                    .operations
                    .iter()
                    .filter(|operation| {
                        operation.project_id == target.project
                            && operation.repository.root == target.root
                    })
                    .map(|operation| {
                        ListItem::new(format!(
                            "{:?} · {:?} · {}",
                            operation.kind, operation.state, operation.id
                        ))
                    })
                    .collect();
                let mut state = ListState::default().with_selected(Some(repo.selected));
                frame.render_stateful_widget(
                    List::new(items).highlight_style(Style::default().bg(Color::DarkGray)),
                    parts[1],
                    &mut state,
                );
            }
        }
    }
    let actions = match repo.page {
        Page::Changes => "Enter Diff · Space Select · s Stage · u Unstage · c Commit",
        Page::Diff => "t Staged/unstaged · ↑↓ Scroll · z Back",
        Page::History => "Enter Changed files · z Back",
        Page::Files => "↑↓ Scroll · z Back",
        Page::Branches => "n Create branch · Enter Review switch · z Back",
        Page::Remotes => "f Fetch · p Pull fast-forward · P Push · z Back",
        Page::Operations => "Enter Open operation terminal · z Back",
    };
    let navigation = if area.width < 65 {
        "c Commit · h History · b Branches · r Remotes\nv Repositories · o Operations · F5 Refresh"
    } else {
        "h History · b Branches · r Remotes · o Operations\nv Repositories · F5 Refresh"
    };
    frame.render_widget(
        Paragraph::new(format!("{actions}\n{navigation}")).wrap(Wrap { trim: false }),
        parts[2],
    );
}

fn popup(frame: &mut Frame<'_>, area: Rect, title: &str) -> Rect {
    let width = area.width.min(108);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y,
        width,
        area.height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::bordered().title(title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}
fn lines(frame: &mut Frame<'_>, area: Rect, text: Vec<String>, scroll: usize) {
    frame.render_widget(
        Paragraph::new(
            text.into_iter()
                .map(|line| Line::from(visible_text(&line)))
                .collect::<Vec<_>>(),
        )
        .scroll((scroll.min(u16::MAX as usize) as u16, 0))
        .wrap(Wrap { trim: false }),
        area,
    );
}
pub(super) fn render_dialog(
    app: &App<'_>,
    dialog: &mut GitDialog,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    match dialog {
        GitDialog::Operations { entries, selected } => {
            let inner = popup(
                frame,
                area,
                "All Git operations · Enter Open · F5 Refresh · Esc Close",
            );
            let items: Vec<ListItem<'static>> = entries
                .iter()
                .map(|operation| {
                    ListItem::new(vec![
                        Line::from(format!(
                            "{} · {:?}",
                            kind_label(operation.kind),
                            operation.state
                        )),
                        Line::from(visible_text(&operation.repository.root.to_string_lossy())),
                    ])
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items).highlight_style(Style::default().bg(Color::DarkGray)),
                inner,
                &mut state,
            );
        }
        GitDialog::Repositories {
            roots, selected, ..
        } => {
            let inner = popup(
                frame,
                area,
                "Choose Git repository · Enter Select · Esc Cancel",
            );
            let items: Vec<ListItem<'static>> = roots
                .iter()
                .map(|root| ListItem::new(visible_text(&root.to_string_lossy())))
                .collect();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items).highlight_style(Style::default().bg(Color::DarkGray)),
                inner,
                &mut state,
            );
        }
        GitDialog::Message { target } => {
            let inner = popup(
                frame,
                area,
                "Commit message · F2 Review staged changes · Esc Keep draft",
            );
            let repo = &app.git.repos[target];
            let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(inner);
            frame.render_widget(
                Paragraph::new(visible_text(&target.root.to_string_lossy())),
                parts[0],
            );
            let (row, column) = repo.draft.position();
            let top = row.saturating_sub(parts[1].height.saturating_sub(1) as usize);
            let left = column.saturating_sub(parts[1].width.saturating_sub(1) as usize);
            frame.render_widget(
                Paragraph::new(
                    repo.draft
                        .text
                        .split('\n')
                        .map(|line| Line::from(visible_text(line)))
                        .collect::<Vec<_>>(),
                )
                .scroll((top as u16, left as u16)),
                parts[1],
            );
            if parts[1].width > 0 && parts[1].height > 0 {
                frame.set_cursor_position((
                    parts[1].x + (column - left) as u16,
                    parts[1].y + (row - top) as u16,
                ));
            }
        }
        GitDialog::Waiting { target, title, .. } => {
            let inner = popup(frame, area, title);
            lines(
                frame,
                inner,
                vec![
                    target.root.display().to_string(),
                    "Working… Esc cancels this pending review. The message draft is kept.".into(),
                ],
                0,
            );
        }
        GitDialog::CommitReview {
            target,
            review,
            message,
            scroll,
            diff,
        } => {
            let inner = popup(
                frame,
                area,
                "Commit all staged changes · F2 Commit · Tab Files/diff · Esc Keep draft",
            );
            if *diff {
                super::git_view::diff(
                    frame,
                    inner,
                    &review.diff,
                    DiffTarget::Index,
                    (*scroll).min(u16::MAX as usize) as u16,
                );
            } else {
                let mut text = vec![
                    target.root.display().to_string(),
                    "All staged changes, including files staged outside idk:".into(),
                    String::new(),
                ];
                text.extend(message.split('\n').map(str::to_owned));
                text.push(String::new());
                text.extend(review.staged.iter().map(|entry| entry.path.display.clone()));
                lines(frame, inner, text, *scroll);
            }
        }
        GitDialog::Plan {
            preview, scroll, ..
        } => {
            let inner = popup(frame, area, "Review Git action · F2 Execute · Esc Cancel");
            let mut text = vec![
                format!("{:?}", preview.kind),
                preview.repository.root.display().to_string(),
            ];
            if let Some(branch) = &preview.branch {
                text.push(format!("Branch: {branch}"));
            }
            if let Some(oid) = &preview.target_oid {
                text.push(format!("Commit: {oid}"));
            }
            if let Some(remote) = &preview.remote {
                text.push(format!("Remote: {}", remote.remote.name));
                text.extend(
                    remote
                        .remote
                        .fetch_urls
                        .iter()
                        .map(|url| format!("Fetch URL: {url}")),
                );
                text.extend(
                    remote
                        .remote
                        .push_urls
                        .iter()
                        .map(|url| format!("Push URL: {url}")),
                );
                if let Some(destination) = &remote.destination {
                    text.push(format!("Destination: {destination}"));
                }
                text.push(format!("Last known tracking counts: ahead {:?}, behind {:?}. No fresh remote contact has happened.",remote.ahead,remote.behind));
            }
            if preview.requires_source_lease {
                text.push(
                    "Build activity and clean files are checked again before changing sources."
                        .into(),
                );
            }
            text.push("Existing hooks, signing and authentication may ask for input in a dedicated Git terminal.".into());
            lines(frame, inner, text, *scroll);
        }
        GitDialog::Branch { target, input } => {
            let inner = popup(
                frame,
                area,
                "Create branch at current commit · F2 Create · Esc Cancel",
            );
            lines(
                frame,
                inner,
                vec![
                    target.root.display().to_string(),
                    "Branch name:".into(),
                    input.text.clone(),
                ],
                0,
            );
            form_cursor(frame, inner, input, 2);
        }
        GitDialog::Remote {
            target,
            remote,
            push,
            input,
        } => {
            let inner = popup(
                frame,
                area,
                if *push {
                    "Push destination · F2 Review · Esc Cancel"
                } else {
                    "Fast-forward pull source · F2 Review · Esc Cancel"
                },
            );
            lines(
                frame,
                inner,
                vec![
                    target.root.display().to_string(),
                    format!("Remote: {remote}"),
                    "Remote branch (explicit):".into(),
                    input.text.clone(),
                ],
                0,
            );
            form_cursor(frame, inner, input, 3);
        }
        GitDialog::Takeover { name, .. } => {
            let inner = popup(
                frame,
                area,
                "Take over Git terminal input · F2 Take over · Esc Cancel",
            );
            lines(
                frame,
                inner,
                vec![
                    format!("{name} is attached by another client."),
                    "Transfer input and resize control to this UI.".into(),
                ],
                0,
            );
        }
    }
}

pub(super) fn render_operation(app: &App<'_>, frame: &mut Frame<'_>, area: Rect) {
    let Some(operation) = &app.git.operation else {
        super::git_view::empty(
            frame,
            area,
            "Git operation unavailable. Ctrl+g opens controls.",
        );
        return;
    };
    let writable = app.git_writable();
    let limited = app
        .git
        .screen
        .as_ref()
        .is_some_and(|screen| screen.output_limited);
    let title = format!(
        "{}Git {} · {:?}{} · {}{}",
        if limited { "Output limited · " } else { "" },
        kind_label(operation.kind),
        operation.state,
        operation
            .result
            .as_ref()
            .map(|result| format!(" · {:?}", result.outcome))
            .unwrap_or_default(),
        operation.repository.root.display(),
        if writable { "" } else { " · read only" }
    );
    frame.render_widget(
        Paragraph::new(visible_text(&title)).style(Style::default().fg(Color::Cyan)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if let Some(screen) = &app.git.screen {
        super::screen::render(frame, app.viewport, screen, writable);
    } else {
        let message = match operation.state {
            GitOperationState::Pending
            | GitOperationState::Running
            | GitOperationState::Cancelling => {
                "Git operation is running. Its dedicated terminal will appear when available."
            }
            GitOperationState::Complete => {
                "Git operation finished. Terminal output is no longer retained."
            }
            GitOperationState::Unknown => {
                "Git operation outcome is unknown. Inspect repository state before retrying."
            }
        };
        super::git_view::empty(frame, app.viewport, message);
    }
    let error = app
        .git
        .screen
        .as_ref()
        .and_then(|screen| screen.error.as_deref())
        .or(operation.error.as_deref());
    let result = operation
        .result
        .as_ref()
        .map(|result| {
            format!(
                "{:?} · exit {:?}{}{}",
                result.outcome,
                result.exit_code,
                result
                    .commit
                    .as_ref()
                    .map(|commit| format!(
                        " · commit {}",
                        commit.oid.get(..12).unwrap_or(&commit.oid)
                    ))
                    .unwrap_or_default(),
                if result.warnings.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", result.warnings.join("; "))
                }
            )
        })
        .unwrap_or_default();
    let footer = if let Some(error) = error {
        format!("{error} · Ctrl+g Controls")
    } else if limited {
        "Output limited; some output may be missing · Ctrl+g Controls".into()
    } else {
        format!("{result} · Ctrl+g Controls · z Back to Git · k Cancel operation")
    };
    frame.render_widget(
        Paragraph::new(visible_text(&footer)).style(Style::default().fg(if error.is_some() {
            Color::LightRed
        } else {
            Color::DarkGray
        })),
        Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
    );
}

fn form_cursor(frame: &mut Frame<'_>, area: Rect, input: &TextInput, row: u16) {
    if area.height > row && area.width > 0 {
        let (text, column) = super::draw::input_window(input, area.width);
        frame.render_widget(
            Paragraph::new(text),
            Rect::new(area.x, area.y + row, area.width, 1),
        );
        frame.set_cursor_position((area.x + column, area.y + row));
    }
}

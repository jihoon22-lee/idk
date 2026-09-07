//! Read-only Git widgets. Filename labels are never used to reconstruct Git paths.
use super::forms::visible_text;
use crate::git_wire::{
    Branch, CommitSummary, DiffTarget, DiffView, GitChange, GitSnapshot, Head, Remote,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use std::collections::BTreeSet;

pub(super) fn header(snapshot: &GitSnapshot) -> Vec<Line<'static>> {
    let head = match &snapshot.revision.head {
        Head::Unborn { reference } => format!(
            "{} · no commits yet",
            reference.trim_start_matches("refs/heads/")
        ),
        Head::Branch { reference, .. } => reference.trim_start_matches("refs/heads/").to_owned(),
        Head::Detached { oid } => format!("Detached HEAD {}", oid.get(..12).unwrap_or(oid)),
    };
    let tracking = match (&snapshot.upstream, snapshot.ahead, snapshot.behind) {
        (Some(upstream), Some(ahead), Some(behind)) => {
            format!(" · last known {upstream}: +{ahead}/-{behind}")
        }
        (Some(upstream), _, _) => format!(" · last known {upstream}: unknown"),
        _ => " · no upstream".into(),
    };
    vec![
        Line::from(visible_text(
            &snapshot.revision.repository.root.to_string_lossy(),
        )),
        Line::from(visible_text(&format!("{head}{tracking}"))),
    ]
}

pub(super) fn changes(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: &GitSnapshot,
    selected: usize,
    marked: &BTreeSet<usize>,
) {
    let notices = usize::from(snapshot.conversion_filters_disabled)
        + usize::from(snapshot.submodule_worktrees_unchecked);
    let areas =
        Layout::vertical([Constraint::Min(1), Constraint::Length(notices as u16)]).split(area);
    let items: Vec<ListItem<'static>> = snapshot
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let status = if entry.conflict {
                "CONFLICT".into()
            } else if entry.untracked {
                "untracked".into()
            } else {
                format!("{}{}", entry.index_status, entry.worktree_status)
            };
            let original = entry
                .original_path
                .as_ref()
                .map(|path| format!(" ← {}", path.display.clone()))
                .unwrap_or_default();
            ListItem::new(Line::from(vec![
                Span::raw(if marked.contains(&index) {
                    "✓ "
                } else {
                    "  "
                }),
                Span::styled(
                    format!("{status:<9} "),
                    Style::default().fg(if entry.conflict {
                        Color::LightRed
                    } else if entry.staged() {
                        Color::LightGreen
                    } else {
                        Color::Yellow
                    }),
                ),
                Span::raw(format!("{}{original}", entry.path.display.clone())),
            ]))
        })
        .collect();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No listed changes.\nFiles staged outside idk appear here too."),
            areas[0],
        );
    } else {
        let mut state = ListState::default().with_selected(Some(selected.min(items.len() - 1)));
        frame.render_stateful_widget(
            List::new(items).highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            areas[0],
            &mut state,
        );
    }
    let mut lines = Vec::new();
    if snapshot.conversion_filters_disabled {
        lines.push("Read preview does not run conversion filters.");
    }
    if snapshot.submodule_worktrees_unchecked {
        lines.push("Submodule working folders were not inspected.");
    }
    frame.render_widget(
        Paragraph::new(lines.join("\n")).style(Style::default().fg(Color::Yellow)),
        areas[1],
    );
}

pub(super) fn diff(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &DiffView,
    target: DiffTarget,
    scroll: u16,
) {
    let title = format!(
        "{}{}{}",
        if target == DiffTarget::Index {
            "Staged changes"
        } else {
            "Unstaged changes"
        },
        if view.binary { " · binary file" } else { "" },
        if view.truncated {
            " · preview limited"
        } else {
            ""
        }
    );
    let text = if view.text.is_empty() {
        "No tracked patch is available. Untracked files appear after staging."
    } else {
        &view.text
    };
    frame.render_widget(
        Paragraph::new(
            text.lines()
                .map(|line| Line::from(visible_text(line)))
                .collect::<Vec<_>>(),
        )
        .scroll((scroll, 0))
        .block(Block::bordered().title(title)),
        area,
    );
}

pub(super) fn history(
    frame: &mut Frame<'_>,
    area: Rect,
    commits: &[CommitSummary],
    selected: usize,
) {
    let items: Vec<ListItem<'static>> = commits
        .iter()
        .map(|commit| {
            ListItem::new(visible_text(&format!(
                "{}  {} · {}",
                commit.oid.get(..12).unwrap_or(&commit.oid),
                commit.subject,
                commit.author
            )))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().bg(Color::DarkGray)),
        area,
        &mut state,
    );
}
pub(super) fn files(frame: &mut Frame<'_>, area: Rect, entries: &[GitChange], scroll: u16) {
    frame.render_widget(
        Paragraph::new(
            entries
                .iter()
                .map(|entry| Line::from(entry.path.display.clone()))
                .collect::<Vec<_>>(),
        )
        .scroll((scroll, 0)),
        area,
    );
}
pub(super) fn branches(frame: &mut Frame<'_>, area: Rect, entries: &[Branch], selected: usize) {
    let items: Vec<ListItem<'static>> = entries
        .iter()
        .map(|branch| {
            ListItem::new(visible_text(&format!(
                "{} {} · {}",
                if branch.current { "●" } else { " " },
                branch.name,
                branch.oid.get(..12).unwrap_or(&branch.oid)
            )))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().bg(Color::DarkGray)),
        area,
        &mut state,
    );
}
pub(super) fn remotes(frame: &mut Frame<'_>, area: Rect, entries: &[Remote], selected: usize) {
    let items: Vec<ListItem<'static>> = entries
        .iter()
        .map(|remote| {
            ListItem::new(vec![
                Line::from(visible_text(&remote.name)),
                Line::from(visible_text(&format!(
                    "  Fetch: {}",
                    remote.fetch_urls.join(", ")
                ))),
                Line::from(visible_text(&format!(
                    "  Push: {}{}",
                    remote.push_urls.join(", "),
                    if remote.embedded_credentials {
                        " · embedded credentials need removal"
                    } else {
                        ""
                    }
                ))),
            ])
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().bg(Color::DarkGray)),
        area,
        &mut state,
    );
}

pub(super) fn empty(frame: &mut Frame<'_>, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(
            message
                .lines()
                .map(|line| Line::from(visible_text(line)))
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false }),
        area,
    );
}

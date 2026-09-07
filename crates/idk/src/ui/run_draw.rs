use super::{
    forms::visible_text,
    runs::{Page, RunDialog},
    App,
};
use crate::run_wire::RunInfo;
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
fn safe(value: impl AsRef<str>) -> String {
    crate::problems::display_text(value.as_ref().as_bytes(), 262144)
}
fn block(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(title)
}
pub(super) fn render(app: &App<'_>, frame: &mut Frame<'_>, area: Rect) {
    let mut lines = Vec::<String>::new();
    if app.run_pending_count() > 0 {
        lines.push(format!(
            "{} request(s) working… · x Cancel pending reads",
            app.run_pending_count()
        ));
    }
    let mut scroll = app.runs.scroll;
    let title = if app.tab == 2 {
        lines.push(
            "n New task · e Edit · Enter Review / approve / start · E Editor settings".into(),
        );
        if let Some((revision, tasks)) = app.current_id().and_then(|id| app.runs.tasks.get(&id)) {
            lines.push(format!("Saved revision {revision} · {} tasks", tasks.len()));
            for (index, task) in tasks.iter().enumerate() {
                lines.push(format!(
                    "{} {} · {} · {} steps · {:?} · {:?}",
                    if index == app.runs.task_index {
                        "▶"
                    } else {
                        " "
                    },
                    task.name,
                    task.id,
                    task.steps.len(),
                    task.failure_policy,
                    task.logging
                ));
            }
            if tasks.is_empty() {
                lines.push(
                    "No tasks registered. n opens a local draft; saving does not run it.".into(),
                );
            }
        } else {
            lines.push("Task definitions not loaded. 3 loads; F5 reconnects.".into());
        }
        scroll = 0;
        "Tasks"
    } else {
        match app.runs.page {
            Page::Runs => {
                lines.push(
                    if app.runs.all_projects {
                        "All project Runs, including removed connections"
                    } else {
                        "Selected project Runs"
                    }
                    .into(),
                );
                lines.push(
                    "Enter Run details · E Editor settings · F9 All projects · F5 Refresh".into(),
                );
                for (index, run) in app.runs.runs.iter().enumerate() {
                    lines.push(format!(
                        "{} {} · {:?} · {} · {}",
                        if index == app.runs.run_index {
                            "▶"
                        } else {
                            " "
                        },
                        run.name,
                        run.state,
                        run.run_id,
                        if run.cleanup_confirmed {
                            "cleanup confirmed"
                        } else {
                            "cleanup unconfirmed"
                        }
                    ));
                }
                if app.runs.runs.is_empty() {
                    lines.push("No recorded Runs loaded. 4 loads registered results.".into());
                }
                scroll = app
                    .runs
                    .run_index
                    .saturating_sub(area.height.saturating_sub(5) as usize)
                    as u16;
            }
            Page::Detail => {
                lines.push(
                    "a Attach Run terminal · r Review repeat · l Raw log · p Problems · / Search · z Runs".into(),
                );
                lines.push(
                    "k Cancel · K Force cancel · u Reconcile Unknown · E Editor settings".into(),
                );
                if let Some(run) = app.current_run() {
                    lines.extend(details(run));
                }
            }
            Page::Log => {
                if let Some(log) = &app.runs.log {
                    lines.push(format!(
                        "Raw log generation {} · next byte {} · follow {} · view limited {}",
                        log.generation, log.next, log.follow, log.limited
                    ));
                    lines.push(
                        "f Follow/pause · ↑↓ Scroll pauses follow · / Search · p Problems · z Runs"
                            .into(),
                    );
                    if let Some(run) = app.current_run() {
                        lines.push(format!(
                            "Log {:?}: {} stored / {} observed bytes · merged PTY",
                            run.log.state, run.log.bytes, run.log.observed_bytes
                        ));
                    }
                    let text = log.text();
                    lines.extend(text.lines().map(str::to_owned));
                    scroll = if log.follow {
                        lines
                            .len()
                            .saturating_sub(area.height.saturating_sub(2) as usize)
                            .min(u16::MAX as usize) as u16
                    } else {
                        log.scroll
                    };
                }
            }
            Page::Problems => {
                lines.push(
                    "Enter Review source location, or original log if no file · l Log · z Runs"
                        .into(),
                );
                if let Some(set) = &app.runs.problems {
                    lines.push(format!(
                        "Run {} · log generation {} · {} parsed bytes · limited {} · partial {}",
                        set.run_id, set.log_generation, set.parsed_bytes, set.limited, set.partial
                    ));
                    lines.push(format!("Source generation {:?} · changed {:?} · counts do not determine Run success",set.source_generation,set.source_changed));
                    for (index, problem) in set.problems.iter().enumerate() {
                        lines.push(format!(
                            "{} {:?} {}{} · {} · log byte {}",
                            if index == app.runs.problem_index {
                                "▶"
                            } else {
                                " "
                            },
                            problem.severity,
                            problem
                                .file
                                .as_ref()
                                .map(|p| p.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "[original log]".into()),
                            problem
                                .range
                                .as_ref()
                                .map(|r| format!(":{}:{}", r.line, r.column.unwrap_or(1)))
                                .unwrap_or_default(),
                            problem.message,
                            problem.log_offset
                        ));
                        if index == app.runs.problem_index {
                            lines.extend(problem.details.iter().map(|line| format!("  {line}")));
                        }
                    }
                    if set.problems.is_empty() {
                        lines.push("No parsed Problems. Consult the Run state and raw log.".into());
                    }
                } else {
                    lines.push("Reading bounded raw log for Problems…".into());
                }
                scroll = app
                    .runs
                    .problem_index
                    .saturating_sub(area.height.saturating_sub(7) as usize)
                    as u16;
            }
            Page::Search => {
                lines.push(
                    "Enter Jump to raw byte match · / New search · l Full log · z Runs".into(),
                );
                if let Some(search) = &app.runs.search {
                    lines.push(format!(
                        "Generation {} · {:?} · truncated {} · cancelled {}",
                        search.descriptor.generation,
                        search.descriptor.state,
                        search.truncated,
                        search.cancelled
                    ));
                    for (index, item) in search.matches.iter().enumerate() {
                        lines.push(format!(
                            "{} byte {} · line {} · {}",
                            if index == app.runs.match_index {
                                "▶"
                            } else {
                                " "
                            },
                            item.offset,
                            item.line,
                            item.text
                        ));
                    }
                    if search.matches.is_empty() {
                        lines.push("No matches in the available raw log.".into());
                    }
                } else {
                    lines.push("Searching raw log…".into());
                }
                scroll = app
                    .runs
                    .match_index
                    .saturating_sub(area.height.saturating_sub(6) as usize)
                    as u16;
            }
        }
        "Run results"
    };
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .map(|l| Line::from(safe(l)))
                .collect::<Vec<_>>(),
        )
        .block(block(title))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0)),
        area,
    );
}
fn details(run: &RunInfo) -> Vec<String> {
    let mut lines = vec![
        format!("{} · {} · {:?}", run.name, run.run_id, run.state),
        format!(
            "Task {} · project {} · operation {}",
            run.task_id, run.project_id, run.operation_id
        ),
        format!(
            "Exit {:?} · signal {:?} · cleanup confirmed {}",
            run.exit_code, run.signal, run.cleanup_confirmed
        ),
        format!(
            "Started {} · finished {:?} · timeout {:?}s / requested {}",
            run.started_at_ms, run.finished_at_ms, run.timeout_seconds, run.timeout_requested
        ),
        format!(
            "Cancel requested {} · source changed {:?}",
            run.cancel_requested, run.source_changed
        ),
        format!(
            "Definition revision {} · cwd {}",
            run.definition_revision,
            run.cwd.display()
        ),
        format!("Launch digest {}", run.launch_digest),
        format!("Initialization digest {}", run.initialization_digest),
        format!(
            "Source roots {:?} · build outputs {:?}",
            run.source_roots, run.build_outputs
        ),
        format!(
            "Source at start: generation {:?} · HEAD {:?} · dirty {:?} · error {:?}",
            run.source_start.generation,
            run.source_start.git_head,
            run.source_start.dirty,
            run.source_start.error
        ),
        format!("Source at end: {:?}", run.source_end),
        format!(
            "Artifact {:?} · from task {:?} · from Run {:?} · verified bytes {}",
            run.artifact.path,
            run.artifact.from_task,
            run.artifact.from_run,
            run.artifact.verified_bytes
        ),
        format!(
            "Log {:?} · generation {} · {} stored / {} observed bytes · limit {}",
            run.log.state,
            run.log.generation,
            run.log.bytes,
            run.log.observed_bytes,
            run.log.limit_bytes
        ),
    ];
    for step in &run.steps {
        lines.push(format!(
            "Step {} {} · exit {:?}",
            step.index + 1,
            step.name,
            step.exit_code
        ));
    }
    if let Some(error) = &run.error {
        lines.push(format!("Error: {error}"));
    }
    lines
}
pub(super) fn render_dialog(_app: &App<'_>, dialog: &RunDialog, frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(Clear, area);
    let mut lines = vec![];
    let mut scroll = 0;
    let title = match dialog {
        RunDialog::Form(form) => {
            lines.push("Tab / Shift+Tab Field · Enter New line · F2 Save · Esc Keep draft".into());
            lines.push("F4 Refresh revision (keeps draft) · F6 Add source · F7 Add step".into());
            lines.push(format!(
                "Project {} · revision {} · saving never runs a command",
                form.project, form.revision
            ));
            let field = &form.fields[form.selected];
            lines.push(format!(
                "Field {} / {}",
                form.selected + 1,
                form.fields.len()
            ));
            lines.push(field.label.clone());
            lines.push(String::new());
            lines.extend(field.value.text.split('\n').map(str::to_owned));
            let (row, column) = field.value.position();
            let inner = area.inner(ratatui::layout::Margin::new(1, 1));
            let cursor_row = 6 + row;
            scroll = cursor_row
                .saturating_sub(inner.height.saturating_sub(1) as usize)
                .min(u16::MAX as usize) as u16;
            if column < (inner.width as usize)
                && cursor_row >= scroll as usize
                && cursor_row - (scroll as usize) < (inner.height as usize)
            {
                frame.set_cursor_position((
                    inner.x + column as u16,
                    inner.y + (cursor_row - scroll as usize) as u16,
                ));
            }
            if form.task.is_some() {
                "Task draft"
            } else {
                "Editor configuration"
            }
        }
        RunDialog::TaskReview {
            review,
            parallel,
            scroll: s,
        } => {
            scroll = *s;
            lines.push(
                if review.approved {
                    "F2 Start reviewed task · F3 Toggle explicit parallel Run · Esc Cancel"
                } else {
                    "F2 Approve task (does not start) · Esc Cancel"
                }
                .into(),
            );
            lines.push(format!(
                "Project {} · task {} · revision {}",
                review.project_id, review.task.id, review.revision
            ));
            lines.push(format!(
                "Approved {} · project initialization approved {} · parallel {}",
                review.approved, review.common_approved, parallel
            ));
            lines.push(format!(
                "Name {} · cwd {} · HOME {}",
                review.task.name,
                review.task.cwd.display(),
                review.home.display()
            ));
            lines.push(format!("Digest {}", review.digest));
            lines.push(format!(
                "Failure {:?} · logging {:?} · interactive {} · timeout {:?}",
                review.task.failure_policy,
                review.task.logging,
                review.task.interactive,
                review.task.timeout_seconds
            ));
            for source in &review.task.sources {
                lines.push(format!(
                    "source {} argv {:?}",
                    source.path.display(),
                    source.args
                ));
            }
            lines.push(format!(
                "Build outputs {:?} · artifact {:?} · from task {:?}",
                review.task.build_outputs, review.task.artifact, review.task.artifact_from_task
            ));
            if review.task.steps.is_empty() {
                lines.push(review.task.command.clone());
            } else {
                for step in &review.task.steps {
                    lines.push(format!("Step {}\n{}", step.name, step.command));
                }
            }
            "Review task"
        }
        RunDialog::Confirm {
            title,
            lines: body,
            scroll: s,
            ..
        } => {
            scroll = *s;
            lines.push("F2 Confirm this exact action · Esc Cancel".into());
            lines.extend(body.clone());
            title
        }
        RunDialog::Editor {
            review, scroll: s, ..
        } => {
            scroll = *s;
            lines.push("F2 Open this exact reviewed location · Esc Cancel".into());
            lines.push(format!(
                "Run {} · Problem {} · project {}",
                review.run_id, review.problem_id, review.project_id
            ));
            lines.push(format!(
                "File {} · line {:?} · column {:?}",
                review.file.display(),
                review.line,
                review.column
            ));
            lines.push(format!("Executable {}", review.executable.display()));
            for (index, arg) in review.arguments.iter().enumerate() {
                lines.push(format!("argv[{index}] = {}", visible_text(arg)));
            }
            lines.push(format!(
                "External GUI {} · configuration changed since Run {}",
                review.external_gui, review.configuration_changed_since_run
            ));
            lines.push(format!(
                "Source changed during Run {:?} · source is a live path, not a saved snapshot",
                review.source_changed_during_run
            ));
            lines.push("Editor starts directly in its own terminal using the current launcher environment and reviewed project folder. Existing shells keep their state; their temporary aliases are not copied.".into());
            "Review editor launch"
        }
        RunDialog::Search(input) => {
            lines.push("Enter / F2 Search available raw bytes · Esc Cancel".into());
            lines.push(input.text.clone());
            "Raw log search"
        }
        RunDialog::Attach { session, owner } => {
            lines.push(format!("Terminal {session} is controlled by {owner}."));
            lines.push("F2 Take input ownership · Esc Cancel".into());
            "Review input takeover"
        }
    };
    let text = lines
        .into_iter()
        .flat_map(|l| {
            safe(l)
                .split('\n')
                .map(|s| Line::from(s.to_owned()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(text)
            .block(block(title))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

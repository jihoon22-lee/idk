#[path = "common/launcher.rs"]
mod launcher;
use base64::Engine;
use idk_workspace::model::*;
use idk_workspace::project::{LaunchEnvironment, ProjectService};
use idk_workspace::run::{BeginReservation, RunRegistry};
use idk_workspace::run_wire::*;
use idk_workspace::store::{FileLock, Store};
use idk_workspace::task::TaskService;
use idk_workspace::terminal::{TerminalExit, TerminalSession};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use tempfile::TempDir;
struct Fixture {
    temp: TempDir,
    store: Store,
    project: String,
    task: String,
    env: LaunchEnvironment,
}
impl Fixture {
    fn new(commands: &[&str], policy: FailurePolicy) -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source 한글 ' !");
        std::fs::create_dir(&root).unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let source = root.join("init.csh");
        std::fs::write(&source, "set task_local = SAME_SHELL\nalias task_alias 'echo alias-retained'\necho once >> init-count\n").unwrap();
        let env = LaunchEnvironment::from_variables(BTreeMap::from([
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
        ]))
        .unwrap();
        let store = Store::open(Some(&temp.path().join("data"))).unwrap();
        let task = TaskDefinition {
            execution: None,
            id: new_id(),
            name: "build".into(),
            command: String::new(),
            cwd: root.clone(),
            sources: vec![],
            artifact: None,
            approved_digest: None,
            steps: commands
                .iter()
                .enumerate()
                .map(|(i, c)| TaskStep {
                    name: format!("step {i}"),
                    command: c.to_string(),
                })
                .collect(),
            failure_policy: policy,
            logging: TaskLogging::Raw,
            interactive: false,
            build_outputs: vec![],
            artifact_from_task: None,
            timeout_seconds: None,
        };
        let project = Project {
            id: new_id(),
            name: "test".into(),
            root: root.clone(),
            repository: None,
            repository_binding: None,
            related_repositories: vec![],
            default_terminal: None,
            shell: ShellConfig {
                native: None,
                executable: launcher::test_shell(),
                login: false,
                init_cwd: root,
                sources: vec![source.into()],
                trusted_digest: None,
            },
            terminals: vec![],
            tasks: vec![task.clone()],
            editor: None,
        };
        let mut workspace = Workspace {
            projects: vec![project.clone()],
            ..Default::default()
        };
        store.save(&mut workspace, 0).unwrap();
        let service = ProjectService { store: &store };
        let review = service.review_initialization(&project.id, &env).unwrap();
        service
            .approve_initialization(review.revision, review)
            .unwrap();
        let result = Self {
            temp,
            store,
            project: project.id,
            task: task.id,
            env,
        };
        result.approve();
        result
    }
    fn approve(&self) {
        let service = TaskService { store: &self.store };
        let review = service
            .review(&self.project, &self.task, &self.env)
            .unwrap();
        service
            .approve(
                review.revision,
                &self.project,
                &self.task,
                &review.digest,
                &self.env,
            )
            .unwrap();
    }
    fn begin(&self, registry: &mut RunRegistry, op: &str, parallel: bool) -> RunStartReply {
        let plan = (TaskService { store: &self.store })
            .launch_plan(&self.project, &self.task, self.env.clone())
            .unwrap();
        registry.begin(plan, op, parallel).unwrap()
    }
    fn execute(&self, registry: &mut RunRegistry, run: &RunInfo) -> RunInfo {
        let prepared = registry
            .shell_plan(&run.run_id)
            .unwrap()
            .prepare(
                &self.temp.path().join("resources"),
                &PathBuf::from(env!("CARGO_BIN_EXE_idk")),
            )
            .unwrap();
        let mut terminal = TerminalSession::spawn_with_output(
            prepared.command,
            24,
            100,
            100,
            Some(registry.output_sink(&run.run_id).unwrap()),
        )
        .unwrap();
        terminal.input(&prepared.bootstrap_bytes).unwrap();
        registry.mark_running(&run.run_id, &new_id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        let exit = loop {
            if let Some(exit) = terminal.try_wait().unwrap() {
                break exit;
            }
            assert!(
                Instant::now() < deadline,
                "task timed out: {}",
                terminal.snapshot().unwrap().text()
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        while !terminal.status().unwrap().reader_closed {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        registry
            .finish(&run.run_id, Some(exit), true, None)
            .unwrap();
        while registry.info(&run.run_id).unwrap().log.state == LogState::Recording {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        registry.info(&run.run_id).unwrap()
    }
    fn log(&self, registry: &RunRegistry, run: &RunInfo) -> String {
        let chunk = registry.read_log(&run.run_id, 1, 0, 65536).unwrap();
        String::from_utf8_lossy(
            &base64::engine::general_purpose::STANDARD
                .decode(chunk.data_base64)
                .unwrap(),
        )
        .into()
    }
}
#[test]
fn same_shell_steps_keep_alias_variables_and_actual_failure() {
    let f = Fixture::new(
        &[
            "echo $task_local; task_alias; set across = preserved",
            "echo $across; /bin/sh -c 'exit 7'",
            "echo SHOULD_NOT_RUN",
        ],
        FailurePolicy::Stop,
    );
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let run = f.execute(&mut registry, &run);
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.exit_code, Some(7));
    assert_eq!(
        run.steps.iter().map(|s| s.exit_code).collect::<Vec<_>>(),
        vec![Some(0), Some(7), None]
    );
    let log = f.log(&registry, &run);
    assert!(
        log.contains("SAME_SHELL") && log.contains("alias-retained") && log.contains("preserved"),
        "{log}"
    );
    assert!(!log.contains("SHOULD_NOT_RUN"));
    assert_eq!(
        std::fs::read_to_string(run.cwd.join("init-count")).unwrap(),
        "once\n"
    );
}
#[test]
fn continue_policy_preserves_first_failure_and_early_exit_is_not_success() {
    let f = Fixture::new(
        &["/bin/sh -c 'exit 9'", "echo CONTINUED"],
        FailurePolicy::Continue,
    );
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let run = f.execute(&mut registry, &run);
    assert_eq!(run.exit_code, Some(9));
    assert!(f.log(&registry, &run).contains("CONTINUED"));
    let f = Fixture::new(&["exit 13", "echo NEVER"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let run = f.execute(&mut registry, &run);
    assert_eq!(run.exit_code, Some(13));
    assert_eq!(run.state, RunState::Failed);
}
#[test]
fn duplicate_cancel_and_crash_preserve_intent_without_restart() {
    let f = Fixture::new(&["echo command"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let operation = new_id();
    let run = f.begin(&mut registry, &operation, false).run;
    assert!(f.begin(&mut registry, &operation, false).existing);
    assert!(f.begin(&mut registry, &new_id(), false).existing);
    assert_eq!(registry.list(None).len(), 1);
    registry.cancel(&run.run_id, false).unwrap();
    assert_eq!(
        registry.info(&run.run_id).unwrap().state,
        RunState::Cancelling
    );
    registry.finish(&run.run_id, None, true, None).unwrap();
    assert_eq!(
        registry.info(&run.run_id).unwrap().state,
        RunState::Cancelled
    );
    let next = f.begin(&mut registry, &new_id(), false).run;
    drop(registry);
    let registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    assert_eq!(
        registry.info(&next.run_id).unwrap().state,
        RunState::Unknown
    );
    assert_eq!(
        registry.info(&run.run_id).unwrap().state,
        RunState::Cancelled
    );
}
#[test]
fn deferred_begin_reservation_commits_and_blocks_duplicates() {
    use idk_workspace::run::BeginReservation;
    let f = Fixture::new(&["echo command"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    let operation = new_id();
    let BeginReservation::Fresh(ticket) = registry
        .begin_reserve(plan.clone(), &operation, false)
        .unwrap()
    else {
        panic!("first reserve must be fresh");
    };
    // A pending reservation blocks the same operation and a non-parallel
    // second start of the same task exactly like a live run would.
    assert!(registry
        .begin_reserve(plan.clone(), &operation, false)
        .is_err());
    assert!(registry
        .begin_reserve(plan.clone(), &new_id(), false)
        .is_err());
    // Parallel intent still reserves a second ticket.
    let BeginReservation::Fresh(other) = registry.begin_reserve(plan, &new_id(), true).unwrap()
    else {
        panic!("parallel reserve must be fresh");
    };
    let probe = ticket.probe(registry.gate());
    let observation = probe.observe_now();
    let reply = registry.begin_commit(ticket, observation).unwrap();
    assert!(!reply.existing);
    assert_eq!(reply.run.state, RunState::Preparing);
    let probe = other.probe(registry.gate());
    let reply = registry.begin_commit(other, probe.observe_now()).unwrap();
    assert!(!reply.existing);
    assert_eq!(registry.list(None).len(), 2);
}
#[test]
fn deferred_reservations_share_history_capacity_and_reopen_within_the_limit() {
    use idk_workspace::run::BeginReservation;
    let f = Fixture::new(&["echo command"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    for _ in 0..63 {
        let run = registry.begin(plan.clone(), &new_id(), false).unwrap().run;
        registry
            .finish(
                &run.run_id,
                Some(TerminalExit {
                    code: 0,
                    signal: None,
                }),
                true,
                None,
            )
            .unwrap();
    }
    let mut tickets = Vec::new();
    for _ in 0..2 {
        let BeginReservation::Fresh(ticket) = registry
            .begin_reserve(plan.clone(), &new_id(), true)
            .unwrap()
        else {
            panic!("parallel reservation must be fresh");
        };
        tickets.push(ticket);
    }
    let mut started = Vec::new();
    for ticket in tickets {
        let observation = ticket.probe(registry.gate()).observe_now();
        started.push(
            registry
                .begin_commit(ticket, observation)
                .unwrap()
                .run
                .run_id,
        );
    }
    assert_eq!(registry.list(None).len(), 64);
    drop(registry);
    let recovered = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    assert_eq!(recovered.list(None).len(), 64);
    for id in started {
        assert_eq!(recovered.info(&id).unwrap().state, RunState::Unknown);
    }
}
#[test]
fn pending_reservations_cannot_overbook_history_and_abandonment_returns_capacity() {
    use idk_workspace::run::BeginReservation;
    let f = Fixture::new(&["echo command"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    let mut tickets = Vec::new();
    for _ in 0..64 {
        let BeginReservation::Fresh(ticket) = registry
            .begin_reserve(plan.clone(), &new_id(), true)
            .unwrap()
        else {
            panic!("parallel reservation must be fresh");
        };
        tickets.push(ticket);
    }
    assert!(registry
        .begin_reserve(plan.clone(), &new_id(), true)
        .is_err());
    registry.begin_abandon(tickets.pop().unwrap());
    let BeginReservation::Fresh(replacement) =
        registry.begin_reserve(plan, &new_id(), true).unwrap()
    else {
        panic!("abandonment must release the reserved capacity");
    };
    tickets.push(replacement);
    for ticket in tickets {
        registry.begin_abandon(ticket);
    }
    assert!(registry.list(None).is_empty());
}
#[test]
fn abandoned_reservation_releases_task_and_lease() {
    use idk_workspace::run::BeginReservation;
    let f = Fixture::new(&["echo command"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    let BeginReservation::Fresh(ticket) = registry.begin_reserve(plan, &new_id(), false).unwrap()
    else {
        panic!("first reserve must be fresh");
    };
    registry.begin_abandon(ticket);
    // After abandonment the task is free again and no run was recorded.
    let run = f.begin(&mut registry, &new_id(), false).run;
    assert_eq!(registry.list(None).len(), 1);
    registry.finish(&run.run_id, None, true, None).unwrap();
}
#[test]
fn finish_probe_then_observed_matches_synchronous_finish() {
    let f = Fixture::new(&["echo command"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let probe = registry.finish_probe(&run.run_id).unwrap();
    let observation = probe.observe_now();
    let finished = registry
        .finish_observed(&run.run_id, None, true, None, observation)
        .unwrap();
    assert_eq!(finished.state, RunState::Unknown);
    assert!(finished.source_end.is_some());
}
#[test]
fn observed_execution_end_stops_timeout_while_source_finalization_is_pending() {
    let f = Fixture::new(&["echo command"], FailurePolicy::Stop);
    f.store
        .update(|workspace| {
            workspace.project_mut(&f.project)?.tasks[0].timeout_seconds = Some(1);
            Ok(())
        })
        .unwrap();
    f.approve();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let after_timeout = Instant::now() + Duration::from_secs(2);
    assert_eq!(
        registry.timed_out_at(after_timeout),
        vec![run.run_id.clone()]
    );
    let probe = registry.finish_probe(&run.run_id).unwrap();
    assert!(registry.timed_out_at(after_timeout).is_empty());
    assert!(registry.cancel(&run.run_id, false).is_err());
    let finished = registry
        .finish_observed(
            &run.run_id,
            Some(TerminalExit {
                code: 0,
                signal: None,
            }),
            true,
            None,
            probe.observe_now(),
        )
        .unwrap();
    assert_eq!(finished.state, RunState::Succeeded);
    assert_eq!(finished.exit_code, Some(0));
    assert!(!finished.cancel_requested && !finished.timeout_requested);
}
#[test]
fn logging_is_bounded_and_search_keeps_byte_positions_and_sanitizes_controls() {
    let f = Fixture::new(&["echo ignored"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let sink = registry.output_sink(&run.run_id).unwrap();
    sink.push(b"first\n\x1b[31merror: synthetic\n");
    std::thread::sleep(Duration::from_millis(50));
    let found = registry
        .search(&run.run_id, 1, "error", &AtomicBool::new(false))
        .unwrap();
    assert_eq!(found.matches[0].offset, 6);
    assert_eq!(found.matches[0].line, 2);
    assert!(!found.matches[0].text.contains('\x1b'));
    let cancelled = registry
        .search(&run.run_id, 1, "error", &AtomicBool::new(true))
        .unwrap();
    assert!(cancelled.cancelled);
    for _ in 0..1500 {
        sink.push(&[b'x'; 8192]);
    }
    registry
        .finish(
            &run.run_id,
            Some(TerminalExit {
                code: 0,
                signal: None,
            }),
            true,
            None,
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let info = registry.info(&run.run_id).unwrap();
    assert_eq!(info.state, RunState::Succeeded);
    assert_eq!(info.log.state, LogState::Limited);
    assert!(info.log.bytes <= 4 * 1024 * 1024);
    assert!(info.log.observed_bytes > info.log.bytes);
}
#[test]
fn changed_definition_and_interactive_raw_are_rejected() {
    let f = Fixture::new(&["echo approved"], FailurePolicy::Stop);
    let mut workspace = f.store.load().unwrap();
    let revision = workspace.revision;
    workspace.project_mut(&f.project).unwrap().tasks[0].steps[0].command = "echo changed".into();
    f.store.save(&mut workspace, revision).unwrap();
    assert!((TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .is_err());
    let mut task = workspace.project(&f.project).unwrap().tasks[0].clone();
    task.interactive = true;
    assert!((TaskService { store: &f.store })
        .save(workspace.revision, &f.project, task)
        .is_err());
}

#[test]
fn real_git_source_lease_blocks_mutations_until_confirmed_cleanup_and_marks_external_changes() {
    let f = Fixture::new(&["echo source"], FailurePolicy::Stop);
    let root = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(root.join("tracked.cpp"), "before\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "fixture"]);
    let repository = idk_workspace::git::Repository::discover(&root).unwrap();
    f.store
        .update(|workspace| {
            let project = workspace.project_mut(&f.project)?;
            project.repository = Some(root.clone());
            project.repository_binding = Some(repository.clone());
            Ok(())
        })
        .unwrap();
    f.approve();
    let gate = SourceGate::default();
    let mut registry = RunRegistry::open(f.store.clone(), gate.clone()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    assert!(gate
        .reserve_mutation(repository.identity(), &new_id())
        .is_err());
    std::fs::write(root.join("tracked.cpp"), "after\n").unwrap();
    let finished = registry
        .finish(
            &run.run_id,
            Some(TerminalExit {
                code: 0,
                signal: None,
            }),
            false,
            Some("cleanup unresolved".into()),
        )
        .unwrap();
    assert_eq!(finished.state, RunState::Unknown);
    assert_eq!(finished.source_changed, Some(true));
    assert!(gate
        .reserve_mutation(repository.identity(), &new_id())
        .is_err());
    let finished = registry
        .finish(
            &run.run_id,
            Some(TerminalExit {
                code: 0,
                signal: None,
            }),
            true,
            None,
        )
        .unwrap();
    assert_eq!(finished.state, RunState::Succeeded);
    assert!(gate
        .reserve_mutation(repository.identity(), &new_id())
        .is_ok());
    let run = f.begin(&mut registry, &new_id(), false).run;
    drop(registry);
    let recovered = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    assert_eq!(
        recovered.info(&run.run_id).unwrap().state,
        RunState::Unknown
    );
    assert!(recovered.publish_source(repository.identity()).is_err());
}
#[test]
fn root_rebind_rejects_live_run_output_reservations_after_directory_rename() {
    let f = Fixture::new(
        &["echo LIVE-READY\nwhile ( ! -e finish-run )\n/bin/sleep 0.02\nend\n/bin/pwd > actual-cwd\necho first-run > shared-output"],
        FailurePolicy::Stop,
    );
    let old = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    let output = old.join("shared-output");
    let duplicate = new_id();
    f.store
        .update(|workspace| {
            let project = workspace.project_mut(&f.project)?;
            project.tasks[0].build_outputs = vec![output.clone()];
            let mut other = project.clone();
            other.id = duplicate.clone();
            other.name = "duplicate root".into();
            workspace.projects.push(other);
            Ok(())
        })
        .unwrap();
    f.approve();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let prepared = registry
        .shell_plan(&run.run_id)
        .unwrap()
        .prepare(
            &f.temp.path().join("resources"),
            &PathBuf::from(env!("CARGO_BIN_EXE_idk")),
        )
        .unwrap();
    let mut terminal = TerminalSession::spawn_with_output(
        prepared.command,
        24,
        100,
        100,
        Some(registry.output_sink(&run.run_id).unwrap()),
    )
    .unwrap();
    terminal.input(&prepared.bootstrap_bytes).unwrap();
    registry.mark_running(&run.run_id, &new_id()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while !terminal.snapshot().unwrap().text().contains("LIVE-READY") {
        assert!(
            Instant::now() < deadline,
            "{}",
            terminal.snapshot().unwrap().text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(terminal.cwd().as_deref(), Some(old.as_path()));
    let moved = f.temp.path().join("moved source");
    std::fs::rename(&old, &moved).unwrap();
    assert_eq!(terminal.cwd().as_deref(), Some(moved.as_path()));
    assert_eq!(
        registry.info(&run.run_id).unwrap().build_outputs,
        vec![output]
    );
    let projects = ProjectService { store: &f.store };
    let config_before = std::fs::read(f.store.config_path()).unwrap();
    let preview = projects
        .preview_rebind_root(&f.project, moved.clone())
        .unwrap();
    let rebound = projects.rebind_root(preview.revision, preview);
    let preview = projects
        .preview_rebind_root(&duplicate, moved.clone())
        .unwrap();
    let rebound_duplicate = projects.rebind_root(preview.revision, preview);
    let config_after = std::fs::read(f.store.config_path()).unwrap();

    // Let the owned shell finish and confirm its actual cwd before asserting
    // the rejected reconnect, so the regression also cleans up on the old code.
    std::fs::write(moved.join("finish-run"), b"continue").unwrap();
    let exit = loop {
        if let Some(exit) = terminal.try_wait().unwrap() {
            break exit;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            terminal.snapshot().unwrap().text()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    while !terminal.status().unwrap().reader_closed {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let finished = registry
        .finish(&run.run_id, Some(exit), true, None)
        .unwrap();
    assert_eq!(finished.state, RunState::Succeeded);
    assert!(finished.cleanup_confirmed);
    assert_eq!(
        std::fs::read_to_string(moved.join("actual-cwd"))
            .unwrap()
            .trim(),
        moved.to_str().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(moved.join("shared-output")).unwrap(),
        "first-run\n"
    );
    assert!(
        rebound.is_err(),
        "reconnect accepted while the live Run reserved the old output path"
    );
    assert!(
        rebound_duplicate.is_err(),
        "another project at the same root bypassed the live Run reservation"
    );
    assert_eq!(config_after, config_before);
    for project in [&f.project, &duplicate] {
        let preview = projects
            .preview_rebind_root(project, moved.clone())
            .unwrap();
        projects.rebind_root(preview.revision, preview).unwrap();
        assert_eq!(
            f.store.load().unwrap().project(project).unwrap().root,
            moved
        );
    }
}

#[test]
fn root_rebind_rejects_pending_run_output_reservations_after_directory_rename() {
    let f = Fixture::new(&["echo task"], FailurePolicy::Stop);
    let old = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    let duplicate = new_id();
    f.store
        .update(|workspace| {
            let project = workspace.project_mut(&f.project)?;
            project.tasks[0].build_outputs = vec![old.join("shared-output")];
            let mut other = project.clone();
            other.id = duplicate.clone();
            other.name = "duplicate root".into();
            workspace.projects.push(other);
            Ok(())
        })
        .unwrap();
    f.approve();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    let BeginReservation::Fresh(ticket) = registry.begin_reserve(plan, &new_id(), false).unwrap()
    else {
        panic!("first build must reserve its outputs");
    };
    let moved = f.temp.path().join("moved source");
    std::fs::rename(&old, &moved).unwrap();
    let projects = ProjectService { store: &f.store };
    let config_before = std::fs::read(f.store.config_path()).unwrap();
    let preview = projects
        .preview_rebind_root(&f.project, moved.clone())
        .unwrap();
    let rebound = projects.rebind_root(preview.revision, preview);
    let preview = projects
        .preview_rebind_root(&duplicate, moved.clone())
        .unwrap();
    let rebound_duplicate = projects.rebind_root(preview.revision, preview);
    let config_after = std::fs::read(f.store.config_path()).unwrap();
    registry.begin_abandon(ticket);
    assert!(
        rebound.is_err(),
        "reconnect accepted while a pending start reserved the old output path"
    );
    assert!(
        rebound_duplicate.is_err(),
        "another project at the same root bypassed the pending Run reservation"
    );
    assert_eq!(config_after, config_before);
    for project in [&f.project, &duplicate] {
        let preview = projects
            .preview_rebind_root(project, moved.clone())
            .unwrap();
        projects.rebind_root(preview.revision, preview).unwrap();
        assert_eq!(
            f.store.load().unwrap().project(project).unwrap().root,
            moved
        );
    }
}

#[test]
fn reconnected_build_outputs_still_block_collisions_at_the_new_missing_path() {
    let f = Fixture::new(&["echo task"], FailurePolicy::Stop);
    let old = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    let old_output = old.join("build/not-created");
    let external = f.temp.path().join("external/not-created");
    f.store
        .update(|workspace| {
            let task = &mut workspace.project_mut(&f.project)?.tasks[0];
            task.build_outputs = vec![old_output.clone(), external.clone()];
            task.artifact = Some(old_output.join("result"));
            Ok(())
        })
        .unwrap();
    f.approve();
    let tasks = TaskService { store: &f.store };
    let captured = tasks
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let moved = f.temp.path().join("moved 한글");
    std::fs::rename(&old, &moved).unwrap();
    let output = moved.join("build/not-created");
    let projects = ProjectService { store: &f.store };
    let preview = projects
        .preview_rebind_root(&f.project, moved.clone())
        .unwrap();
    assert!(
        preview
            .changes
            .iter()
            .any(|change| change.old == old_output && change.new == output),
        "reconnection must review the moved build-output reservation"
    );
    assert!(!preview.changes.iter().any(|change| change.old == external));
    projects.rebind_root(preview.revision, preview).unwrap();
    let workspace = f.store.load().unwrap();
    let task = &workspace.project(&f.project).unwrap().tasks[0];
    assert_eq!(task.cwd, moved);
    assert_eq!(task.build_outputs, vec![output.clone(), external]);
    assert_eq!(task.artifact, Some(output.join("result")));
    assert!(task.approved_digest.is_none());
    assert!(tasks
        .launch_plan(&f.project, &f.task, f.env.clone())
        .is_err());
    let common = projects.review_initialization(&f.project, &f.env).unwrap();
    projects
        .approve_initialization(common.revision, common)
        .unwrap();
    f.approve();
    // Recreating the old source path must not make a plan captured before the
    // reconnect valid again, even when its initialization bytes still match.
    std::fs::create_dir(&old).unwrap();
    std::fs::copy(moved.join("init.csh"), old.join("init.csh")).unwrap();
    assert!(
        registry.begin_reserve(captured, &new_id(), false).is_err(),
        "a captured launch plan reserved the old paths after reconnection"
    );

    let mut other = task.clone();
    other.id = new_id();
    other.name = "another build".into();
    other.build_outputs = vec![output.clone()];
    tasks
        .save(f.store.load().unwrap().revision, &f.project, other.clone())
        .unwrap();
    let review = tasks.review(&f.project, &other.id, &f.env).unwrap();
    tasks
        .approve(
            review.revision,
            &f.project,
            &other.id,
            &review.digest,
            &f.env,
        )
        .unwrap();
    let plan = tasks
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    let BeginReservation::Fresh(ticket) = registry.begin_reserve(plan, &new_id(), false).unwrap()
    else {
        panic!("first build must reserve its outputs");
    };
    let conflicting = tasks
        .launch_plan(&f.project, &other.id, f.env.clone())
        .unwrap();
    assert!(!output.exists());
    let error = registry
        .begin_reserve(conflicting.clone(), &new_id(), false)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("build output conflicts"),
        "{error}"
    );
    registry.begin_abandon(ticket);
    let BeginReservation::Fresh(ticket) = registry
        .begin_reserve(conflicting, &new_id(), false)
        .unwrap()
    else {
        panic!("abandoned output reservation must be released");
    };
    registry.begin_abandon(ticket);
}

#[test]
fn output_collision_and_log_replacement_are_explicit() {
    let f = Fixture::new(&["echo task"], FailurePolicy::Stop);
    let output = f.temp.path().join("build");
    std::fs::create_dir(&output).unwrap();
    f.store
        .update(|workspace| {
            workspace.project_mut(&f.project)?.tasks[0].build_outputs = vec![output.clone()];
            Ok(())
        })
        .unwrap();
    f.approve();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    assert!(registry.begin(plan, &new_id(), true).is_err());
    let sink = registry.output_sink(&run.run_id).unwrap();
    sink.push(b"original\n");
    std::thread::sleep(Duration::from_millis(50));
    let log = f
        .store
        .state_dir
        .join("run-logs")
        .join(format!("{}.log", run.run_id));
    std::fs::rename(&log, log.with_extension("saved")).unwrap();
    idk_workspace::store::atomic_write(&log, b"replacement secret\n").unwrap();
    let descriptor = registry.info(&run.run_id).unwrap().log;
    assert_eq!(descriptor.state, LogState::Expired);
    assert_eq!(descriptor.generation, 2);
    assert!(registry.read_log(&run.run_id, 1, 0, 100).is_err());
    assert!(registry
        .read_log(&run.run_id, 2, 0, 100)
        .unwrap()
        .data_base64
        .is_empty());
}
#[test]
fn interactive_log_disabled_never_creates_a_raw_file() {
    let f = Fixture::new(&["echo SECRET_ECHO"], FailurePolicy::Stop);
    f.store
        .update(|workspace| {
            let task = &mut workspace.project_mut(&f.project)?.tasks[0];
            task.interactive = true;
            task.logging = TaskLogging::Disabled;
            Ok(())
        })
        .unwrap();
    f.approve();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let run = f.execute(&mut registry, &run);
    assert_eq!(run.state, RunState::Succeeded);
    assert_eq!(run.log.state, LogState::Disabled);
    assert!(f.log(&registry, &run).is_empty());
    assert!(!f
        .store
        .state_dir
        .join("run-logs")
        .join(format!("{}.log", run.run_id))
        .exists());
    assert!(
        !std::fs::read_to_string(f.store.state_dir.join("runs.json"))
            .unwrap()
            .contains("SECRET_ECHO")
    );
}

#[test]
fn qmake_make_cmake_and_external_python_tests_use_registered_task_environment() {
    let mut f = Fixture::new(
        &["qmake fixture.pro", "make --silent", "./fixture"],
        FailurePolicy::Stop,
    );
    let qmake = f.temp.path().join("qmake-build");
    std::fs::create_dir(&qmake).unwrap();
    std::fs::write(qmake.join("fixture.pro"),"TEMPLATE = app\nTARGET = fixture\nCONFIG += console\nCONFIG -= app_bundle qt\nSOURCES += main.cpp\n").unwrap();
    std::fs::write(
        qmake.join("main.cpp"),
        "#include <cstdio>\nint main() { std::puts(\"BUILT_BINARY\"); return 0; }\n",
    )
    .unwrap();
    f.store
        .update(|workspace| {
            let task = &mut workspace.project_mut(&f.project)?.tasks[0];
            task.cwd = qmake.clone();
            task.artifact = Some(qmake.join("fixture"));
            task.build_outputs = vec![qmake.clone()];
            Ok(())
        })
        .unwrap();
    f.approve();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let built = f.execute(&mut registry, &run);
    assert_eq!(
        built.state,
        RunState::Succeeded,
        "{}",
        f.log(&registry, &built)
    );
    assert!(f.log(&registry, &built).contains("BUILT_BINARY"));
    let cmake = f.temp.path().join("cmake-build");
    std::fs::create_dir(&cmake).unwrap();
    std::fs::write(cmake.join("CMakeLists.txt"),"cmake_minimum_required(VERSION 3.10)\nproject(Fixture LANGUAGES CXX)\nadd_executable(fixture main.cpp)\n").unwrap();
    std::fs::copy(qmake.join("main.cpp"), cmake.join("main.cpp")).unwrap();
    let mut task = f.store.load().unwrap().project(&f.project).unwrap().tasks[0].clone();
    task.id = new_id();
    task.name = "cmake".into();
    task.cwd = cmake.clone();
    task.build_outputs = vec![cmake.clone()];
    task.artifact = Some(cmake.join("out/fixture"));
    task.steps = vec![
        TaskStep {
            name: "configure".into(),
            command: "cmake -S . -B out".into(),
        },
        TaskStep {
            name: "build".into(),
            command: "cmake --build out".into(),
        },
        TaskStep {
            name: "run".into(),
            command: "./out/fixture".into(),
        },
    ];
    let revision = f.store.load().unwrap().revision;
    (TaskService { store: &f.store })
        .save(revision, &f.project, task.clone())
        .unwrap();
    f.task = task.id;
    f.approve();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let built = f.execute(&mut registry, &run);
    assert_eq!(
        built.state,
        RunState::Succeeded,
        "{}",
        f.log(&registry, &built)
    );
    let build_task = f.task.clone();
    for index in 0..2 {
        let cwd = f.temp.path().join(format!("external-test-{index}"));
        std::fs::create_dir(&cwd).unwrap();
        let extra = cwd.join("test.csh");
        std::fs::write(&extra, "setenv TEST_ENV external\n").unwrap();
        std::fs::write(cwd.join("test_fixture.py"),"import os, unittest\nclass Test(unittest.TestCase):\n def test_environment(self): self.assertEqual(os.environ['TEST_ENV'], 'external')\nif __name__ == '__main__': unittest.main()\n").unwrap();
        let mut task = f
            .store
            .load()
            .unwrap()
            .project(&f.project)
            .unwrap()
            .tasks
            .iter()
            .find(|task| task.id == build_task)
            .unwrap()
            .clone();
        task.id = new_id();
        task.name = format!("Python test {index}");
        task.cwd = cwd.clone();
        task.build_outputs = vec![];
        task.sources = vec![extra.into()];
        task.artifact_from_task = Some(build_task.clone());
        task.steps = vec![TaskStep {
            name: "Python test".into(),
            command: "python3 test_fixture.py".into(),
        }];
        let revision = f.store.load().unwrap().revision;
        (TaskService { store: &f.store })
            .save(revision, &f.project, task.clone())
            .unwrap();
        f.task = task.id;
        f.approve();
        let run = f.begin(&mut registry, &new_id(), false).run;
        let tested = f.execute(&mut registry, &run);
        assert_eq!(
            tested.state,
            RunState::Succeeded,
            "{}",
            f.log(&registry, &tested)
        );
        assert_eq!(tested.cwd, cwd);
        assert!(tested.source_roots.contains(&cwd));
        assert_eq!(
            tested.artifact.from_run.as_deref(),
            Some(built.run_id.as_str())
        );
        assert!(!tested.artifact.verified_bytes);
    }
}
#[test]
fn actual_shell_signal_remains_failure_without_a_fabricated_step_status() {
    let f = Fixture::new(&["/bin/kill -9 $$"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let run = f.execute(&mut registry, &run);
    assert_eq!(run.state, RunState::Failed);
    assert!(
        run.signal.is_some(),
        "exit={:?}, log={}",
        run.exit_code,
        f.log(&registry, &run)
    );
    assert_eq!(run.steps[0].exit_code, None);
}

#[test]
fn bounded_retention_removes_only_old_completed_owned_logs_and_resources() {
    let f = Fixture::new(&["echo retained"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    let mut first = None;
    for _ in 0..65 {
        let run = registry.begin(plan.clone(), &new_id(), false).unwrap().run;
        first.get_or_insert_with(|| run.run_id.clone());
        registry
            .finish(
                &run.run_id,
                Some(TerminalExit {
                    code: 0,
                    signal: None,
                }),
                true,
                None,
            )
            .unwrap();
    }
    let first = first.unwrap();
    assert_eq!(registry.list(None).len(), 64);
    assert!(registry.info(&first).is_err());
    assert!(!f
        .store
        .state_dir
        .join("run-logs")
        .join(format!("{first}.log"))
        .exists());
    assert!(!f.store.runtime_dir.join(format!("run-{first}")).exists());
    assert!(f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .shell
        .sources[0]
        .path
        .exists());
}
#[test]
fn retention_preserves_finalization_retry_and_source_lease_after_a_storage_failure() {
    use idk_workspace::run::BeginReservation;
    let f = Fixture::new(&["echo retained"], FailurePolicy::Stop);
    let root = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "."],
        vec!["commit", "-qm", "fixture"],
    ] {
        assert!(std::process::Command::new("git")
            .current_dir(&root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    let repository = idk_workspace::git::Repository::discover(&root).unwrap();
    f.store
        .update(|workspace| {
            let project = workspace.project_mut(&f.project)?;
            project.repository = Some(root.clone());
            project.repository_binding = Some(repository.clone());
            Ok(())
        })
        .unwrap();
    f.approve();
    let gate = SourceGate::default();
    let mut registry = RunRegistry::open(f.store.clone(), gate.clone()).unwrap();
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    let oldest = registry.begin(plan.clone(), &new_id(), true).unwrap().run;
    registry.mark_running(&oldest.run_id, &new_id()).unwrap();
    let run_use = f.store.runtime_dir.join("run-use.lock");
    assert!(FileLock::acquire(&run_use, true).is_err());
    let observed = oldest.source_start.clone();
    let exit = TerminalExit {
        code: 0,
        signal: None,
    };
    // Fill the remaining history while keeping the first execution owned.
    for _ in 0..63 {
        let BeginReservation::Fresh(ticket) = registry
            .begin_reserve(plan.clone(), &new_id(), true)
            .unwrap()
        else {
            panic!()
        };
        let run = registry.begin_commit(ticket, observed.clone()).unwrap().run;
        registry
            .finish_observed(
                &run.run_id,
                Some(exit.clone()),
                true,
                None,
                observed.clone(),
            )
            .unwrap();
    }
    let log = f
        .store
        .state_dir
        .join("run-logs")
        .join(format!("{}.log", oldest.run_id));
    let directory = f.store.runtime_dir.join(format!("run-{}", oldest.run_id));
    let ledger = f.store.state_dir.join("runs.json");
    let backup = f.store.state_dir.join("runs.saved");
    std::fs::rename(&ledger, &backup).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    assert!(registry
        .finish_observed(
            &oldest.run_id,
            Some(exit.clone()),
            true,
            None,
            observed.clone()
        )
        .is_err());
    assert!(gate
        .state(repository.identity())
        .unwrap()
        .runs
        .contains_key(&oldest.operation_id));
    assert!(
        FileLock::acquire(&run_use, true).is_err(),
        "a failed final save released the Run's workspace reservation"
    );
    std::fs::remove_dir(&ledger).unwrap();
    std::fs::rename(&backup, &ledger).unwrap();
    // Storage recovered, but no finish retry has saved the oldest outcome yet.
    let BeginReservation::Fresh(ticket) = registry
        .begin_reserve(plan.clone(), &new_id(), true)
        .unwrap()
    else {
        panic!()
    };
    registry.begin_abandon(ticket);
    assert!(
        registry.info(&oldest.run_id).is_ok(),
        "retention discarded an execution still awaiting durable finalization"
    );
    assert!(log.is_file() && directory.is_dir());
    assert!(
        FileLock::acquire(&run_use, true).is_err(),
        "storage recovery released the reservation before finalization was durable"
    );
    assert!(gate
        .reserve_mutation(repository.identity(), &new_id())
        .is_err());
    let finished = registry
        .finish_observed(
            &oldest.run_id,
            Some(exit.clone()),
            true,
            None,
            observed.clone(),
        )
        .unwrap();
    assert_eq!(finished.state, RunState::Succeeded);
    assert!(finished.cleanup_confirmed);
    assert!(
        FileLock::acquire(&run_use, true).is_ok(),
        "durably confirmed cleanup did not release the workspace reservation"
    );
    assert!(gate
        .reserve_mutation(repository.identity(), &new_id())
        .is_ok());
    // Once the final outcome is durable, ordinary retention can reclaim it.
    let BeginReservation::Fresh(ticket) = registry
        .begin_reserve(plan.clone(), &new_id(), true)
        .unwrap()
    else {
        panic!()
    };
    let run = registry.begin_commit(ticket, observed.clone()).unwrap().run;
    registry
        .finish_observed(&run.run_id, Some(exit), true, None, observed)
        .unwrap();
    let BeginReservation::Fresh(ticket) = registry.begin_reserve(plan, &new_id(), true).unwrap()
    else {
        panic!()
    };
    registry.begin_abandon(ticket);
    assert!(registry.info(&oldest.run_id).is_err());
    assert!(!log.exists() && !directory.exists());
}
#[test]
fn builtin_aliases_cannot_mask_step_status_or_wrapper_assignment() {
    let f = Fixture::new(&["/bin/sh -c 'exit 17'", "echo NEVER"], FailurePolicy::Stop);
    let source = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .shell
        .sources[0]
        .path
        .clone();
    std::fs::write(
        &source,
        "alias set 'echo set-shadow'\nalias echo 'true'\nalias source 'true'\nalias exit 'true'\n",
    )
    .unwrap();
    let service = ProjectService { store: &f.store };
    let review = service.review_initialization(&f.project, &f.env).unwrap();
    service
        .approve_initialization(review.revision, review)
        .unwrap();
    f.approve();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    let run = f.execute(&mut registry, &run);
    assert_eq!(run.exit_code, Some(17));
    assert_eq!(run.steps[0].exit_code, Some(17));
    assert_eq!(run.steps[1].exit_code, None);
}

#[test]
fn cancellation_remains_requested_through_partial_cleanup_and_then_final_confirmation() {
    let f = Fixture::new(&["echo task"], FailurePolicy::Stop);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    registry.cancel(&run.run_id, false).unwrap();
    assert!(registry.shell_plan(&run.run_id).is_err());
    let unknown = registry
        .finish(
            &run.run_id,
            Some(TerminalExit {
                code: 0,
                signal: None,
            }),
            false,
            Some("cleanup pending".into()),
        )
        .unwrap();
    assert_eq!(unknown.state, RunState::Unknown);
    assert!(unknown.cancel_requested);
    let cancelled = registry
        .finish(
            &run.run_id,
            Some(TerminalExit {
                code: 0,
                signal: None,
            }),
            true,
            None,
        )
        .unwrap();
    assert_eq!(cancelled.state, RunState::Cancelled);
    assert_eq!(cancelled.exit_code, Some(0));
}

#[test]
fn explicit_unknown_cleanup_reconciliation_preserves_unknown_exit_outcome() {
    let f = Fixture::new(&["echo task"], FailurePolicy::Stop);
    let old = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    assert!(registry.acknowledge_unknown_cleanup(&run.run_id).is_err());
    drop(registry);
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    assert_eq!(registry.info(&run.run_id).unwrap().state, RunState::Unknown);
    assert!(
        FileLock::acquire(&f.store.runtime_dir.join("run-use.lock"), true).is_ok(),
        "this recovery case must have no surviving in-memory Run lease"
    );
    let moved = f.temp.path().join("moved source");
    std::fs::rename(&old, &moved).unwrap();
    let projects = ProjectService { store: &f.store };
    let preview = projects
        .preview_rebind_root(&f.project, moved.clone())
        .unwrap();
    let config_before = std::fs::read(f.store.config_path()).unwrap();
    assert!(
        projects
            .rebind_root(preview.revision, preview.clone())
            .is_err(),
        "recovered Unknown work allowed reconnect after its in-memory lease was lost"
    );
    assert_eq!(std::fs::read(f.store.config_path()).unwrap(), config_before);
    let unknown = registry.acknowledge_unknown_cleanup(&run.run_id).unwrap();
    assert_eq!(unknown.state, RunState::Unknown);
    assert!(unknown.cleanup_confirmed);
    assert_eq!(unknown.exit_code, None);
    projects.rebind_root(preview.revision, preview).unwrap();
    let common = projects.review_initialization(&f.project, &f.env).unwrap();
    projects
        .approve_initialization(common.revision, common)
        .unwrap();
    f.approve();
    assert!(!f.begin(&mut registry, &new_id(), false).existing);
}

#[test]
fn failed_unknown_reconciliation_does_not_release_source_or_start_another_run() {
    let f = Fixture::new(&["echo task"], FailurePolicy::Stop);
    let root = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .unwrap()
        .success());
    let repository = idk_workspace::git::Repository::discover(&root).unwrap();
    f.store
        .update(|workspace| {
            let project = workspace.project_mut(&f.project)?;
            project.repository = Some(root.clone());
            project.repository_binding = Some(repository.clone());
            Ok(())
        })
        .unwrap();
    f.approve();
    let gate = SourceGate::default();
    let mut registry = RunRegistry::open(f.store.clone(), gate).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    drop(registry);
    let gate = SourceGate::default();
    let mut registry = RunRegistry::open(f.store.clone(), gate.clone()).unwrap();
    let ledger = f.store.state_dir.join("runs.json");
    let backup = f.store.state_dir.join("runs.saved");
    std::fs::rename(&ledger, &backup).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    assert!(registry.acknowledge_unknown_cleanup(&run.run_id).is_err());
    assert!(!registry.info(&run.run_id).unwrap().cleanup_confirmed);
    assert!(registry.publish_source(repository.identity()).is_err());
    assert!(gate
        .reserve_mutation(repository.identity(), &new_id())
        .is_err());
    let plan = (TaskService { store: &f.store })
        .launch_plan(&f.project, &f.task, f.env.clone())
        .unwrap();
    assert!(registry.begin(plan, &new_id(), false).is_err());
    std::fs::remove_dir(&ledger).unwrap();
    std::fs::rename(&backup, &ledger).unwrap();
    assert!(
        registry
            .acknowledge_unknown_cleanup(&run.run_id)
            .unwrap()
            .cleanup_confirmed
    );
    registry.publish_source(repository.identity()).unwrap();
    assert!(gate
        .reserve_mutation(repository.identity(), &new_id())
        .is_ok());
}
#[test]
fn unchanged_untracked_names_do_not_prove_unchanged_source_contents() {
    let f = Fixture::new(&["echo task"], FailurePolicy::Stop);
    let root = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "."],
        vec!["commit", "-qm", "fixture"],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
    }
    let repository = idk_workspace::git::Repository::discover(&root).unwrap();
    f.store
        .update(|workspace| {
            let project = workspace.project_mut(&f.project)?;
            project.repository = Some(root.clone());
            project.repository_binding = Some(repository);
            Ok(())
        })
        .unwrap();
    f.approve();
    std::fs::write(root.join("untracked.cpp"), "before\n").unwrap();
    let mut registry = RunRegistry::open(f.store.clone(), SourceGate::default()).unwrap();
    let run = f.begin(&mut registry, &new_id(), false).run;
    std::fs::write(root.join("untracked.cpp"), "after\n").unwrap();
    let run = registry
        .finish(
            &run.run_id,
            Some(TerminalExit {
                code: 0,
                signal: None,
            }),
            true,
            None,
        )
        .unwrap();
    assert_eq!(run.state, RunState::Succeeded);
    assert_eq!(run.source_changed, None);
    assert!(run.source_start.error.unwrap().contains("untracked"));
}

#[test]
fn newly_registered_repositories_become_known_and_provider_drop_invalidates_idle_state() {
    let f = Fixture::new(&["echo task"], FailurePolicy::Stop);
    let gate = SourceGate::default();
    let registry = RunRegistry::open(f.store.clone(), gate.clone()).unwrap();
    let root = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .root
        .clone();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .unwrap()
        .success());
    let repository = idk_workspace::git::Repository::discover(&root).unwrap();
    f.store
        .update(|workspace| {
            let project = workspace.project_mut(&f.project)?;
            project.repository = Some(root.clone());
            project.repository_binding = Some(repository.clone());
            Ok(())
        })
        .unwrap();
    assert!(!gate.state(repository.identity()).unwrap().provider_ready);
    registry.publish_registered_sources().unwrap();
    assert!(gate.state(repository.identity()).unwrap().provider_ready);
    drop(registry);
    assert!(!gate.state(repository.identity()).unwrap().provider_ready);
    assert!(gate
        .reserve_mutation(repository.identity(), &new_id())
        .is_err());
}

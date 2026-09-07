use idk_workspace::model::{
    new_id, Project, ShellConfig, SourceSpec, TaskDefinition, Workspace, SCHEMA,
};
use idk_workspace::project::{
    new_terminal, path_availability, ConnectDraft, LaunchEnvironment, PathAvailability,
    ProjectService, TerminalDraft,
};
use idk_workspace::shell::InitializationState;
use idk_workspace::store::{atomic_write, Store};
use idk_workspace::terminal::TerminalSession;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Fixture {
    tmp: TempDir,
    store: Store,
    root: PathBuf,
    home: PathBuf,
    source: PathBuf,
    shell: PathBuf,
    environment: LaunchEnvironment,
}
impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project 한글 ' ! $ ;");
        let home = tmp.path().join("home");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&home).unwrap();
        let source = root.join("environment.csh");
        fs::write(&source, "set project_local = retained\nalias project_alias 'echo PROJECT_ALIAS_OK'\necho initialized >> initialization-count\n").unwrap();
        let shell = std::env::var_os("IDK_TEST_SHELL")
            .or_else(|| std::env::var_os("IDK_TEST_TCSH"))
            .map(PathBuf::from)
            .or_else(|| {
                ["/usr/bin/tcsh", "/bin/tcsh"]
                    .into_iter()
                    .map(PathBuf::from)
                    .find(|p| p.is_file())
            })
            .expect("install a real csh/tcsh or set IDK_TEST_SHELL");
        let environment = LaunchEnvironment::from_variables(BTreeMap::from([
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            (
                "PROJECT_PRIVATE_TOKEN".into(),
                "do-not-persist-this-secret".into(),
            ),
        ]))
        .unwrap();
        let store = Store::open(Some(&tmp.path().join("idk-data"))).unwrap();
        Self {
            tmp,
            store,
            root,
            home,
            source,
            shell,
            environment,
        }
    }
    fn service(&self) -> ProjectService<'_> {
        ProjectService { store: &self.store }
    }
    fn revision(&self) -> u64 {
        self.store.load().unwrap().revision
    }
    fn draft(&self, name: &str, count: usize) -> ConnectDraft {
        ConnectDraft {
            name: name.into(),
            root: self.root.clone(),
            shell: ShellConfig {
                executable: self.shell.clone(),
                login: false,
                init_cwd: self.root.clone(),
                sources: vec![self.source.clone().into()],
                trusted_digest: None,
            },
            terminals: (0..count)
                .map(|i| TerminalDraft {
                    name: format!("Development {}", i + 1),
                    cwd: self.root.clone(),
                    sources: Vec::new(),
                    persistent: true,
                })
                .collect(),
        }
    }
    fn create(&self, name: &str, count: usize) -> Project {
        let mut preview = self
            .service()
            .preview_connect(self.draft(name, count))
            .unwrap();
        preview.allow_duplicate_root = true;
        self.service().create(preview.revision, preview).unwrap()
    }
    fn approve(&self, id: &str) {
        let review = self
            .service()
            .review_initialization(id, &self.environment)
            .unwrap();
        assert!(review.common.error.is_none(), "{:?}", review.common.error);
        self.service()
            .approve_initialization(review.revision, review)
            .unwrap();
    }
}

fn wait_ready(prepared: &idk_workspace::shell::PreparedShell, session: &mut TerminalSession) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if prepared.state().unwrap() == InitializationState::Ready {
            return;
        }
        assert!(
            session.try_wait().unwrap().is_none(),
            "shell exited during init: {}",
            session.snapshot().unwrap().text()
        );
        assert!(
            Instant::now() < deadline,
            "incomplete init: {}",
            session.snapshot().unwrap().text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn wait_line(session: &TerminalSession, line: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = session.snapshot().unwrap();
        if snapshot.text().lines().any(|found| found.trim() == line) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "missing {line:?}: {}",
            snapshot.text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn git(path: &Path, args: &[&str]) {
    let result = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn six_terminal_definitions_default_order_transients_and_real_shells() {
    let fixture = Fixture::new();
    let mut draft = fixture.draft("Example", 2);
    for i in 1..=3 {
        let external = fixture.tmp.path().join(format!("external-test-{i}"));
        fs::create_dir(&external).unwrap();
        draft.terminals.push(TerminalDraft {
            name: format!("Test {i}"),
            cwd: external,
            sources: Vec::new(),
            persistent: true,
        });
    }
    let preview = fixture.service().preview_connect(draft).unwrap();
    let project = fixture.service().create(preview.revision, preview).unwrap();
    assert_eq!(project.terminals.len(), 5);
    assert_eq!(project.terminals[0].cwd, project.terminals[1].cwd);
    assert_ne!(project.terminals[0].id, project.terminals[1].id);
    let extra = new_terminal(
        &project,
        TerminalDraft {
            name: "Sixth".into(),
            cwd: fixture.root.clone(),
            sources: Vec::new(),
            persistent: true,
        },
    )
    .unwrap();
    fixture
        .service()
        .save_terminal(fixture.revision(), &project.id, extra.clone())
        .unwrap();
    let mut ids: Vec<_> = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .terminals
        .iter()
        .map(|t| t.id.clone())
        .collect();
    ids.reverse();
    fixture
        .service()
        .reorder_terminals(fixture.revision(), &project.id, ids.clone())
        .unwrap();
    fixture
        .service()
        .set_default_terminal(fixture.revision(), &project.id, &extra.id)
        .unwrap();
    fixture.approve(&project.id);
    let project = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    assert_eq!(
        project
            .terminals
            .iter()
            .map(|t| t.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(project.default_terminal.as_deref(), Some(extra.id.as_str()));
    let mut sessions = Vec::new();
    for terminal in &project.terminals {
        let launch = fixture
            .service()
            .terminal_launch_plan(&project.id, terminal, fixture.environment.clone())
            .unwrap();
        let prepared = launch
            .shell
            .prepare(
                &fixture.tmp.path().join("resources"),
                Path::new(env!("CARGO_BIN_EXE_idk")),
            )
            .unwrap();
        let mut session = TerminalSession::spawn(prepared.command.clone(), 24, 100, 1000).unwrap();
        session.input(&prepared.bootstrap_bytes).unwrap();
        wait_ready(&prepared, &mut session);
        assert_eq!(
            session.cwd().as_ref(),
            Some(&terminal.cwd.canonicalize().unwrap())
        );
        session
            .input(b"printf '\\n'; project_alias; printf 'LOCAL:%s\\n' \"$project_local\"\r")
            .unwrap();
        wait_line(&session, "PROJECT_ALIAS_OK");
        wait_line(&session, "LOCAL:retained");
        sessions.push((prepared, session));
    }
    sessions[0]
        .1
        .input(b"set project_local = changed\r")
        .unwrap();
    sessions[1]
        .1
        .input(b"printf '\\nINDEPENDENT:%s\\n' \"$project_local\"\r")
        .unwrap();
    wait_line(&sessions[1].1, "INDEPENDENT:retained");
    fixture
        .service()
        .rename(fixture.revision(), &project.id, "Renamed".into())
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join("initialization-count"))
            .unwrap()
            .lines()
            .count(),
        6
    );
    for (_, session) in &mut sessions {
        session.terminate().unwrap();
    }
    let mut temporary = new_terminal(
        &project,
        TerminalDraft {
            name: "Temporary".into(),
            cwd: fixture.root.clone(),
            sources: Vec::new(),
            persistent: false,
        },
    )
    .unwrap();
    let before = fs::read(fixture.store.config_path()).unwrap();
    fixture
        .service()
        .save_terminal(fixture.revision(), &project.id, temporary.clone())
        .unwrap();
    assert_eq!(fs::read(fixture.store.config_path()).unwrap(), before);
    let review = fixture
        .service()
        .review_terminal(&project.id, &temporary, &fixture.environment)
        .unwrap();
    fixture
        .service()
        .approve_transient(
            fixture.revision(),
            &project.id,
            &mut temporary,
            &review,
            &fixture.environment,
        )
        .unwrap();
    assert!(fixture
        .service()
        .terminal_launch_plan(&project.id, &temporary, fixture.environment.clone())
        .is_ok());
    temporary.persistent = true;
    fixture
        .service()
        .save_terminal(fixture.revision(), &project.id, temporary.clone())
        .unwrap();
    assert!(fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .terminals
        .iter()
        .any(|t| t.id == temporary.id));
    let config = fs::read_to_string(fixture.store.config_path()).unwrap();
    assert!(!config.contains("do-not-persist-this-secret"));
    let serialized_review = serde_json::to_string(
        &fixture
            .service()
            .review_initialization(&project.id, &fixture.environment)
            .unwrap(),
    )
    .unwrap();
    assert!(!serialized_review.contains("do-not-persist-this-secret"));
}

#[test]
fn exact_ids_ambiguous_names_alias_paths_and_stale_revisions() {
    let fixture = Fixture::new();
    let first = fixture.create("Same", 1);
    let mut duplicate = fixture
        .service()
        .preview_connect(fixture.draft("Same", 1))
        .unwrap();
    assert_eq!(duplicate.duplicates.len(), 1);
    assert!(fixture
        .service()
        .create(duplicate.revision, duplicate.clone())
        .is_err());
    duplicate.allow_duplicate_root = true;
    let second = fixture
        .service()
        .create(duplicate.revision, duplicate)
        .unwrap();
    fixture
        .service()
        .rename(fixture.revision(), &first.id, second.id.clone())
        .unwrap();
    assert_eq!(
        fixture
            .store
            .load()
            .unwrap()
            .project(&second.id)
            .unwrap()
            .id,
        second.id
    );
    assert_eq!(
        fixture.service().resolve_project_id(&second.id).unwrap(),
        second.id
    );
    fixture
        .service()
        .rename(fixture.revision(), &first.id, "Same".into())
        .unwrap();
    assert!(fixture.service().resolve_project_id("Same").is_err());
    let alias = fixture.tmp.path().join("project-alias");
    symlink(&fixture.root, &alias).unwrap();
    let mut draft = fixture.draft("Alias", 1);
    draft.root = alias;
    assert_eq!(
        fixture
            .service()
            .preview_connect(draft)
            .unwrap()
            .duplicates
            .len(),
        2
    );
    let stale = fixture.revision();
    fixture.service().select(stale, &first.id).unwrap();
    assert!(fixture
        .service()
        .rename(stale, &second.id, "Stale edit".into())
        .is_err());
    assert_eq!(
        fixture.service().list().unwrap().projects[0].project.id,
        first.id
    );
}

#[test]
fn missing_extra_sources_and_missing_terminal_paths_are_independent() {
    let fixture = Fixture::new();
    let project = fixture.create("Partial", 2);
    let mut broken = project.terminals[1].clone();
    broken
        .sources
        .push(fixture.tmp.path().join("missing-extra.csh").into());
    broken.cwd = fixture.tmp.path().join("missing-test-directory");
    fixture
        .service()
        .save_terminal(fixture.revision(), &project.id, broken.clone())
        .unwrap();
    let review = fixture
        .service()
        .review_initialization(&project.id, &fixture.environment)
        .unwrap();
    assert!(review.common.digest.is_some());
    assert!(review.terminals[0].digest.is_some());
    assert!(review.terminals[1].error.is_some());
    fixture
        .service()
        .approve_initialization(review.revision, review)
        .unwrap();
    let project = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    assert!(fixture
        .service()
        .terminal_launch_plan(
            &project.id,
            &project.terminals[0],
            fixture.environment.clone()
        )
        .is_ok());
    assert!(fixture
        .service()
        .terminal_launch_plan(
            &project.id,
            &project.terminals[1],
            fixture.environment.clone()
        )
        .is_err());
    assert_eq!(
        fixture.service().inspect(&project.id).unwrap().terminals[1].path,
        PathAvailability::Missing
    );
    let denied = fixture.tmp.path().join("denied");
    fs::create_dir(&denied).unwrap();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(path_availability(&denied), PathAvailability::Denied);
    }
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn terminal_extra_source_changes_and_static_startup_home_changes_require_review() {
    let fixture = Fixture::new();
    let project = fixture.create("Trust", 2);
    let extra = fixture.root.join("extra.csh");
    fs::write(&extra, "set extra = one\n").unwrap();
    let mut first = project.terminals[0].clone();
    first.sources.push(extra.clone().into());
    fixture
        .service()
        .save_terminal(fixture.revision(), &project.id, first)
        .unwrap();
    fixture.approve(&project.id);
    let approved = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    fs::write(&extra, "set extra = two\n").unwrap();
    assert!(fixture
        .service()
        .terminal_launch_plan(
            &project.id,
            &approved.terminals[0],
            fixture.environment.clone()
        )
        .is_err());
    assert!(fixture
        .service()
        .terminal_launch_plan(
            &project.id,
            &approved.terminals[1],
            fixture.environment.clone()
        )
        .is_ok());
    let stale_review = fixture
        .service()
        .review_initialization(&project.id, &fixture.environment)
        .unwrap();
    fs::write(&extra, "set extra = three\n").unwrap();
    assert!(fixture
        .service()
        .approve_initialization(stale_review.revision, stale_review)
        .is_err());
    fixture.approve(&project.id);
    let approved = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    fs::write(fixture.home.join(".cshrc"), "set startup = new\n").unwrap();
    assert!(fixture
        .service()
        .terminal_launch_plan(
            &project.id,
            &approved.terminals[1],
            fixture.environment.clone()
        )
        .is_err());
    fixture.approve(&project.id);
    let other_home = fixture.tmp.path().join("another-home");
    fs::create_dir(&other_home).unwrap();
    let mut vars = fixture.environment.variables().clone();
    vars.insert("HOME".into(), other_home.to_str().unwrap().into());
    let env = LaunchEnvironment::from_variables(vars).unwrap();
    let review = fixture
        .service()
        .review_initialization(&project.id, &env)
        .unwrap();
    assert!(!review.common.trusted);
}

#[test]
fn moving_root_preserves_ids_external_paths_and_user_files() {
    let fixture = Fixture::new();
    git(&fixture.root, &["init", "-q"]);
    let mut draft = fixture.draft("Move", 2);
    let external = fixture.tmp.path().join("external");
    fs::create_dir(&external).unwrap();
    draft.terminals[1].cwd = external.clone();
    let preview = fixture.service().preview_connect(draft).unwrap();
    let project = fixture.service().create(preview.revision, preview).unwrap();
    let original = fs::read(&fixture.source).unwrap();
    let moved = fixture.tmp.path().join("moved");
    fs::rename(&fixture.root, &moved).unwrap();
    let preview = fixture
        .service()
        .preview_rebind_root(&project.id, moved.clone())
        .unwrap();
    assert!(preview
        .changes
        .iter()
        .any(|c| c.field == "initialization cwd"));
    assert!(!preview.changes.iter().any(|c| c.old == external));
    fixture
        .service()
        .rebind_root(preview.revision, preview)
        .unwrap();
    let updated = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    assert_eq!(updated.id, project.id);
    assert_eq!(updated.root, moved);
    assert_eq!(updated.terminals[0].id, project.terminals[0].id);
    assert_eq!(updated.terminals[0].cwd, moved);
    assert_eq!(updated.terminals[1].cwd, external);
    assert_eq!(fs::read(&updated.shell.sources[0].path).unwrap(), original);
    assert_eq!(updated.repository_binding.as_ref().unwrap().root, moved);
    fixture
        .service()
        .remove_definition(fixture.revision(), &project.id)
        .unwrap();
    assert!(moved.join(".git").exists());
    assert!(updated.shell.sources[0].path.exists());
    assert!(external.exists());
}

#[test]
fn related_linked_worktrees_are_explicit_bindings_separate_from_terminal_cwd() {
    let fixture = Fixture::new();
    git(&fixture.root, &["init", "-q"]);
    git(
        &fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    );
    let linked = fixture.tmp.path().join("linked-worktree");
    git(
        &fixture.root,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    let project = fixture.create("Git", 1);
    let main = project.repository_binding.clone().unwrap();
    let related = fixture
        .service()
        .bind_repository(fixture.revision(), &project.id, linked.clone(), false)
        .unwrap();
    assert_ne!(main.git_dir, related.git_dir);
    assert_eq!(main.common_dir, related.common_dir);
    let stored = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    assert_eq!(stored.repository_binding, Some(main.clone()));
    assert_eq!(stored.terminals[0].cwd, fixture.root);
    fixture
        .service()
        .bind_repository(fixture.revision(), &project.id, linked, true)
        .unwrap();
    let stored = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    assert_eq!(stored.repository_binding, Some(related));
    assert!(stored.related_repositories.contains(&main));
    fixture
        .service()
        .clear_primary_repository(fixture.revision(), &project.id)
        .unwrap();
    assert!(fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .repository
        .is_none());
}

#[test]
fn framed_task_digest_cannot_move_command_bytes_into_the_cwd() {
    let fixture = Fixture::new();
    let project = fixture.create("Digest", 1);
    let task = TaskDefinition {
        id: new_id(),
        name: "Task".into(),
        command: "echo safe".into(),
        cwd: "/tmp/prefix/tmp/suffix".into(),
        sources: Vec::new(),
        artifact: None,
        approved_digest: None,
        steps: Vec::new(),
        failure_policy: Default::default(),
        logging: Default::default(),
        interactive: false,
        build_outputs: Vec::new(),
        artifact_from_task: None,
        timeout_seconds: None,
    };
    let mut other = task.clone();
    other.command = "echo safe/tmp/prefix".into();
    other.cwd = "/tmp/suffix".into();
    assert_eq!(
        format!("{}{}", task.command, task.cwd.display()),
        format!("{}{}", other.command, other.cwd.display())
    );
    assert_ne!(
        project.task_digest(&task).unwrap(),
        project.task_digest(&other).unwrap()
    );
}

#[test]
fn regular_file_limits_symlink_retarget_and_shell_bytes_are_bound_to_trust() {
    let fixture = Fixture::new();
    let mut draft = fixture.draft("Files", 1);
    let copied_shell = fixture.tmp.path().join("tcsh");
    fs::copy(&fixture.shell, &copied_shell).unwrap();
    draft.shell.executable = copied_shell.clone();
    let source_alias = fixture.root.join("selected.csh");
    symlink(&fixture.source, &source_alias).unwrap();
    draft.shell.sources = vec![source_alias.clone().into()];
    let preview = fixture.service().preview_connect(draft).unwrap();
    let project = fixture.service().create(preview.revision, preview).unwrap();
    fixture.approve(&project.id);
    let before = fixture
        .service()
        .review_initialization(&project.id, &fixture.environment)
        .unwrap()
        .common
        .digest;
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&copied_shell)
        .unwrap()
        .write_all(b"idk-fixture-trailer")
        .unwrap();
    let after = fixture
        .service()
        .review_initialization(&project.id, &fixture.environment)
        .unwrap()
        .common
        .digest;
    assert!(after.is_some());
    assert_ne!(before, after);
    fixture.approve(&project.id);
    let alternate = fixture.root.join("alternate.csh");
    fs::write(&alternate, "set altered = yes\n").unwrap();
    fs::remove_file(&source_alias).unwrap();
    symlink(&alternate, &source_alias).unwrap();
    assert!(
        !fixture
            .service()
            .review_initialization(&project.id, &fixture.environment)
            .unwrap()
            .common
            .trusted
    );
    let fifo = fixture.root.join("fifo.csh");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let mut shell = project.shell.clone();
    shell.sources = vec![fifo.into()];
    fixture
        .service()
        .update_shell(fixture.revision(), &project.id, shell.clone())
        .unwrap();
    let start = Instant::now();
    assert!(fixture
        .service()
        .review_initialization(&project.id, &fixture.environment)
        .unwrap()
        .common
        .error
        .unwrap()
        .contains("regular file"));
    assert!(start.elapsed() < Duration::from_secs(2));
    let huge = fixture.root.join("huge.csh");
    fs::File::create(&huge)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    shell.sources = vec![huge.into()];
    fixture
        .service()
        .update_shell(fixture.revision(), &project.id, shell)
        .unwrap();
    assert!(fixture
        .service()
        .review_initialization(&project.id, &fixture.environment)
        .unwrap()
        .common
        .error
        .unwrap()
        .contains("limit"));
}

#[test]
fn legacy_path_sources_load_and_malformed_or_future_config_is_preserved() {
    #[derive(serde::Deserialize)]
    struct Legacy {
        sources: Vec<SourceSpec>,
    }
    let old: Legacy = toml::from_str("sources = ['/tmp/source.csh']").unwrap();
    assert_eq!(old.sources[0].path, PathBuf::from("/tmp/source.csh"));
    assert!(old.sources[0].args.is_empty());
    let fixture = Fixture::new();
    let malformed = b"this is not = [valid configuration";
    atomic_write(&fixture.store.config_path(), malformed).unwrap();
    assert!(fixture.service().list().is_err());
    assert!(fixture
        .service()
        .preview_connect(fixture.draft("Do not reset", 1))
        .is_err());
    assert_eq!(fs::read(fixture.store.config_path()).unwrap(), malformed);
    let future = Workspace {
        schema: SCHEMA + 1,
        ..Workspace::default()
    };
    let encoded = toml::to_string(&future).unwrap();
    atomic_write(&fixture.store.config_path(), encoded.as_bytes()).unwrap();
    assert!(fixture
        .service()
        .list()
        .unwrap_err()
        .to_string()
        .contains("unsupported"));
    assert_eq!(
        fs::read_to_string(fixture.store.config_path()).unwrap(),
        encoded
    );
}

#[test]
fn each_project_launch_uses_its_explicit_environment_without_cross_project_state() {
    let fixture = Fixture::new();
    fs::write(
        &fixture.source,
        "set environment_value = \"$PROJECT_PRIVATE_TOKEN\"\n",
    )
    .unwrap();
    let first = fixture.create("First", 1);
    let second = fixture.create("Second", 1);
    let mut sessions = Vec::new();
    for (project, value) in [(&first, "PROJECT_A"), (&second, "PROJECT_B")] {
        let mut values = fixture.environment.variables().clone();
        values.insert("PROJECT_PRIVATE_TOKEN".into(), value.into());
        let environment = LaunchEnvironment::from_variables(values).unwrap();
        let review = fixture
            .service()
            .review_initialization(&project.id, &environment)
            .unwrap();
        fixture
            .service()
            .approve_initialization(review.revision, review)
            .unwrap();
        let project = fixture
            .store
            .load()
            .unwrap()
            .project(&project.id)
            .unwrap()
            .clone();
        let launch = fixture
            .service()
            .terminal_launch_plan(&project.id, &project.terminals[0], environment)
            .unwrap();
        let prepared = launch
            .shell
            .prepare(
                &fixture.tmp.path().join("resources"),
                Path::new(env!("CARGO_BIN_EXE_idk")),
            )
            .unwrap();
        let mut session = TerminalSession::spawn(prepared.command.clone(), 24, 100, 1000).unwrap();
        session.input(&prepared.bootstrap_bytes).unwrap();
        wait_ready(&prepared, &mut session);
        session
            .input(b"printf '\\nPROJECT_ENV:%s\\n' \"$environment_value\"\r")
            .unwrap();
        wait_line(&session, &format!("PROJECT_ENV:{value}"));
        sessions.push((prepared, session));
    }
    sessions[0]
        .1
        .input(b"setenv PROJECT_PRIVATE_TOKEN changed\r")
        .unwrap();
    sessions[1]
        .1
        .input(b"printf '\\nSTILL:%s\\n' \"$PROJECT_PRIVATE_TOKEN\"\r")
        .unwrap();
    wait_line(&sessions[1].1, "STILL:PROJECT_B");
    for (_, session) in &mut sessions {
        session.terminate().unwrap();
    }
    let persisted = fs::read_to_string(fixture.store.config_path()).unwrap();
    assert!(!persisted.contains("PROJECT_PRIVATE_TOKEN"));
    assert!(!persisted.contains("PROJECT_A"));
    assert!(!persisted.contains("PROJECT_B"));
}

#[test]
fn argument_and_source_order_changes_are_part_of_the_reviewed_definition() {
    let fixture = Fixture::new();
    let project = fixture.create("Order", 1);
    let original = project.source_digest().unwrap();
    let mut changed = project.clone();
    changed.shell.sources[0]
        .args
        .push("literal argument".into());
    assert_ne!(original, changed.source_digest().unwrap());
    let second = fixture.root.join("second.csh");
    fs::write(&second, "set second = value\n").unwrap();
    changed.shell.sources[0].args.clear();
    changed.shell.sources.push(second.into());
    let ordered = changed.source_digest().unwrap();
    changed.shell.sources.reverse();
    assert_ne!(ordered, changed.source_digest().unwrap());
    fixture.approve(&project.id);
    let mut terminal = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .terminals[0]
        .clone();
    terminal.sources.push(SourceSpec {
        path: fixture.source.clone(),
        args: vec!["changed".into()],
    });
    fixture
        .service()
        .save_terminal(fixture.revision(), &project.id, terminal)
        .unwrap();
    assert!(
        !fixture
            .service()
            .review_initialization(&project.id, &fixture.environment)
            .unwrap()
            .terminals[0]
            .trusted
    );
}

#[test]
fn environment_bounds_and_debug_output_do_not_expose_values() {
    let fixture = Fixture::new();
    let mut too_many = fixture.environment.variables().clone();
    for index in 0..1024 {
        too_many.insert(format!("EXTRA_{index}"), String::new());
    }
    assert!(LaunchEnvironment::from_variables(too_many)
        .unwrap_err()
        .to_string()
        .contains("1024"));
    let mut too_large = fixture.environment.variables().clone();
    too_large.insert("LARGE".into(), "x".repeat(256 * 1024));
    assert!(LaunchEnvironment::from_variables(too_large)
        .unwrap_err()
        .to_string()
        .contains("256 KiB"));
    assert!(!format!("{:?}", fixture.environment).contains("do-not-persist-this-secret"));
    let project = fixture.create("Debug", 1);
    fixture.approve(&project.id);
    let project = fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    let launch = fixture
        .service()
        .terminal_launch_plan(
            &project.id,
            &project.terminals[0],
            fixture.environment.clone(),
        )
        .unwrap();
    assert!(!format!("{:?}", launch.shell).contains("do-not-persist-this-secret"));
    let prepared = launch
        .shell
        .prepare(
            &fixture.tmp.path().join("resources"),
            Path::new(env!("CARGO_BIN_EXE_idk")),
        )
        .unwrap();
    assert!(!format!("{prepared:?}").contains("do-not-persist-this-secret"));
}

#[test]
fn changing_directory_symlink_targets_invalidates_the_affected_launch_scope() {
    let fixture = Fixture::new();
    let other = fixture.tmp.path().join("other-directory");
    fs::create_dir(&other).unwrap();
    let init_alias = fixture.tmp.path().join("init-alias");
    symlink(&fixture.root, &init_alias).unwrap();
    let start_alias = fixture.tmp.path().join("start-alias");
    symlink(&fixture.root, &start_alias).unwrap();
    let mut draft = fixture.draft("Directory identity", 1);
    draft.shell.init_cwd = init_alias.clone();
    draft.terminals[0].cwd = start_alias.clone();
    let preview = fixture.service().preview_connect(draft).unwrap();
    let project = fixture.service().create(preview.revision, preview).unwrap();
    fixture.approve(&project.id);
    fs::remove_file(&start_alias).unwrap();
    symlink(&other, &start_alias).unwrap();
    let review = fixture
        .service()
        .review_initialization(&project.id, &fixture.environment)
        .unwrap();
    assert!(review.common.trusted);
    assert!(!review.terminals[0].trusted);
    fixture
        .service()
        .approve_initialization(review.revision, review)
        .unwrap();
    fs::remove_file(&init_alias).unwrap();
    symlink(&other, &init_alias).unwrap();
    assert!(
        !fixture
            .service()
            .review_initialization(&project.id, &fixture.environment)
            .unwrap()
            .common
            .trusted
    );
}

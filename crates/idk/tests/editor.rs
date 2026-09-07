#[path = "common/run_fixture.rs"]
mod fixture;
use idk_workspace::editor::{resolve_location, validate_config, EditorService};
use idk_workspace::model::{EditorConfig, Project, ShellConfig, Workspace};
use idk_workspace::problems::{Problem, Severity, SourceRange};
use idk_workspace::store::Store;
use idk_workspace::terminal::TerminalSession;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
fn problem(run: &idk_workspace::run_wire::RunInfo, file: &Path) -> Problem {
    Problem {
        id: "problem-fixture".into(),
        run_id: run.run_id.clone(),
        project_id: run.project_id.clone(),
        file: Some(file.to_owned()),
        range: Some(SourceRange {
            line: 12,
            column: Some(3),
        }),
        severity: Severity::Error,
        message: "compile failure".into(),
        details: Vec::new(),
        log_offset: 40,
        log_generation: run.log.generation,
        source_generation: Some(7),
    }
}
struct Fixture {
    temporary: tempfile::TempDir,
    store: Store,
    source: PathBuf,
    editor: PathBuf,
    run: idk_workspace::run_wire::RunInfo,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        let editor = temporary.path().join("editor-fixture");
        fs::write(
            &editor,
            "#!/bin/sh\nprintf '%s\\000' \"$@\" > \"$IDK_EDITOR_ARGUMENTS\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::open(Some(&temporary.path().join("data"))).unwrap();
        let run = fixture::run(&source);
        let project = Project {
            id: run.project_id.clone(),
            name: "Editor source".into(),
            root: source.clone(),
            repository: None,
            repository_binding: None,
            related_repositories: Vec::new(),
            default_terminal: None,
            shell: ShellConfig {
                executable: "/usr/bin/tcsh".into(),
                login: false,
                init_cwd: source.clone(),
                sources: Vec::new(),
                trusted_digest: None,
            },
            terminals: Vec::new(),
            tasks: Vec::new(),
            editor: None,
        };
        let mut workspace = Workspace {
            projects: vec![project],
            ..Workspace::default()
        };
        store.save(&mut workspace, 0).unwrap();
        let service = EditorService { store: &store };
        service
            .save(
                workspace.revision,
                &run.project_id,
                EditorConfig {
                    executable: editor.clone(),
                    args: vec!["+{line}".into(), "--".into(), "{file}".into()],
                    external_gui: false,
                },
            )
            .unwrap();
        Self {
            temporary,
            store,
            source,
            editor,
            run,
        }
    }
}
#[test]
fn dedicated_editor_receives_literal_korean_quotes_options_and_placeholder_like_filename() {
    let fixture = Fixture::new();
    let file = fixture
        .source
        .join("- 한글 'quoted' {line} $(touch BAD).cpp");
    fs::write(&file, "unchanged source\n").unwrap();
    let problem = problem(&fixture.run, &file);
    let service = EditorService {
        store: &fixture.store,
    };
    let plan = service.review(&fixture.run, &problem).unwrap();
    assert_eq!(plan.review().arguments[2], file.to_str().unwrap());
    assert!(plan.review().configuration_changed_since_run);
    let output = fixture.temporary.path().join("argv.bin");
    let env = BTreeMap::from([
        (
            "HOME".into(),
            fixture.temporary.path().to_str().unwrap().into(),
        ),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TERM".into(), "xterm-256color".into()),
        (
            "IDK_EDITOR_ARGUMENTS".into(),
            output.to_str().unwrap().into(),
        ),
    ]);
    let command = service.command(&plan, env).unwrap();
    let mut terminal = TerminalSession::spawn(command, 10, 60, 20).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(exit) = terminal.try_wait().unwrap() {
            assert_eq!(exit.code, 0);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let args = fs::read(output).unwrap();
    let args = args
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        args,
        vec![
            b"+12".as_slice(),
            b"--".as_slice(),
            file.as_os_str().as_encoded_bytes()
        ]
    );
    assert_eq!(fs::read(&file).unwrap(), b"unchanged source\n");
    assert!(!fixture.source.join("BAD").exists());
}
#[test]
fn editor_rejects_file_and_executable_replacement_between_review_and_launch() {
    let fixture = Fixture::new();
    let file = fixture.source.join("a.cpp");
    fs::write(&file, "before").unwrap();
    let service = EditorService {
        store: &fixture.store,
    };
    let diagnostic = problem(&fixture.run, &file);
    let plan = service.review(&fixture.run, &diagnostic).unwrap();
    let replacement = fixture.source.join("replacement");
    fs::write(&replacement, "before").unwrap();
    fs::rename(&replacement, &file).unwrap();
    assert!(service.command(&plan, BTreeMap::new()).is_err());
    let plan = service.review(&fixture.run, &diagnostic).unwrap();
    fs::write(&fixture.editor, "#!/bin/sh\nexit 1\n").unwrap();
    assert!(service.command(&plan, BTreeMap::new()).is_err());
}
#[test]
fn recorded_and_current_roots_allow_explicit_external_tests_but_reject_escape_move_and_missing() {
    let fixture = Fixture::new();
    let outside = fixture.temporary.path().join("external tests");
    fs::create_dir(&outside).unwrap();
    let external = outside.join("test.py");
    fs::write(&external, "raise RuntimeError()\n").unwrap();
    let roots = vec![fixture.source.clone(), outside.clone()];
    assert_eq!(
        resolve_location(&outside, Path::new("test.py"), &roots, &roots).unwrap(),
        external
    );
    assert!(resolve_location(
        &outside,
        Path::new("test.py"),
        &roots,
        std::slice::from_ref(&fixture.source)
    )
    .is_err());
    let link = fixture.source.join("escape.py");
    symlink(&external, &link).unwrap();
    assert!(resolve_location(
        &fixture.source,
        Path::new("escape.py"),
        std::slice::from_ref(&fixture.source),
        std::slice::from_ref(&fixture.source)
    )
    .is_err());
    let missing = fixture.source.join("generated.cpp");
    assert!(resolve_location(&fixture.source, &missing, &roots, &roots).is_err());
    assert!(!missing.exists());
    let moved = fixture.temporary.path().join("old source");
    fs::rename(&fixture.source, &moved).unwrap();
    symlink(&outside, &fixture.source).unwrap();
    assert!(resolve_location(
        &fixture.source,
        Path::new("test.py"),
        std::slice::from_ref(&fixture.source),
        &[outside]
    )
    .is_err());
}
#[test]
fn arbitrary_path_controls_argv_interpolation_and_unavailable_line_positions_are_rejected() {
    let fixture = Fixture::new();
    let file = fixture.source.join("a.cpp");
    fs::write(&file, "source").unwrap();
    let mut config = EditorConfig {
        executable: fixture.editor.clone(),
        args: vec!["--".into(), "{file}".into()],
        external_gui: false,
    };
    for args in [
        vec!["--", "--file={file}"],
        vec!["{file}", "--"],
        vec!["--", "{file}", "{file}"],
        vec!["--", "{file}", "{unknown}"],
    ] {
        config.args = args.into_iter().map(str::to_owned).collect();
        assert!(validate_config(&config).is_err());
    }
    let bad = fixture.source.join("newline\nname.cpp");
    fs::write(&bad, "source").unwrap();
    assert!(resolve_location(
        &fixture.source,
        &bad,
        std::slice::from_ref(&fixture.source),
        std::slice::from_ref(&fixture.source)
    )
    .is_err());
    let mut diagnostic = problem(&fixture.run, &file);
    diagnostic.range = None;
    assert!(EditorService {
        store: &fixture.store
    }
    .review(&fixture.run, &diagnostic)
    .is_err());
    diagnostic = problem(&fixture.run, &file);
    diagnostic.run_id = "another-run".into();
    assert!(EditorService {
        store: &fixture.store
    }
    .review(&fixture.run, &diagnostic)
    .is_err());
}

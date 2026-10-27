use idk_workspace::model::{NativeShell, ShellConfig, Workspace};
use idk_workspace::project::{
    discover_shells, ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft,
};
use idk_workspace::shell::InitializationState;
use idk_workspace::store::Store;
use idk_workspace::terminal::TerminalSession;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[test]
fn native_startup_is_owned_by_shell_and_rc_changes_do_not_require_script_approval() {
    let executable = std::env::var("IDK_TEST_NATIVE_SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let rc_name = if executable.ends_with("zsh") {
        ".zshrc"
    } else {
        ".bashrc"
    };
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join(rc_name),
        "echo startup >> \"$HOME/count\"\nexport NATIVE_RC=loaded\nPS1=\"IDK_NATIVE_PROMPT> \"\n",
    )
    .unwrap();
    let store = Store::open(Some(&temp.path().join("data"))).unwrap();
    let service = ProjectService { store: &store };
    let draft = ConnectDraft {
        name: "native".into(),
        root: home.clone(),
        shell: ShellConfig {
            native: Some(NativeShell {
                args: vec!["-i".into()],
                env: BTreeMap::from([("REQUEST_VALUE".into(), "override".into())]),
            }),
            executable: executable.into(),
            login: false,
            init_cwd: home.clone(),
            sources: vec![],
            trusted_digest: None,
        },
        terminals: vec![TerminalDraft {
            name: "dev".into(),
            cwd: home.clone(),
            sources: vec![],
            persistent: true,
        }],
    };
    let preview = service.preview_connect(draft).unwrap();
    let project = service.create(preview.revision, preview).unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TERM".into(), "xterm".into()),
        ("REQUEST_VALUE".into(), "request".into()),
    ]);
    let plan = service
        .terminal_launch_plan(
            &project.id,
            &project.terminals[0],
            LaunchEnvironment::from_variables(env.clone()).unwrap(),
        )
        .unwrap();
    let prepared = plan
        .shell
        .prepare(
            &temp.path().join("resources"),
            std::path::Path::new("/unused-launcher"),
        )
        .unwrap();
    assert!(prepared.bootstrap_bytes.is_empty());
    assert_eq!(prepared.state().unwrap(), InitializationState::Unobserved);
    let mut terminal = TerminalSession::spawn(prepared.command, 24, 100, 200).unwrap();
    // Process creation is intentionally not shell readiness. In particular, zsh
    // may flush input while entering its line editor. Only this test fixture's
    // native prompt tells the test when it is safe to type; idk installs no hook.
    let prompt_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen = terminal.snapshot().unwrap().text();
        if screen.contains("IDK_NATIVE_PROMPT> ") {
            break;
        }
        assert!(
            Instant::now() < prompt_deadline,
            "native prompt did not appear; screen: {screen:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    terminal
        .input(b"printf 'RESULT:%s:%s\\n' \"$NATIVE_RC\" \"$REQUEST_VALUE\"\r")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen = terminal.snapshot().unwrap().text();
        if screen.contains("RESULT:loaded:override") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native command output did not appear; screen: {screen:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(home.join("count")).unwrap(),
        "startup\n"
    );
    std::fs::write(home.join(rc_name), "echo changed\n").unwrap();
    service
        .terminal_launch_plan(
            &project.id,
            &project.terminals[0],
            LaunchEnvironment::from_variables(env).unwrap(),
        )
        .unwrap();
    terminal.input(b"exit\r").unwrap();
    let exit_deadline = Instant::now() + Duration::from_secs(5);
    while terminal.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < exit_deadline,
            "native shell did not exit; screen: {:?}",
            terminal.snapshot().unwrap().text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn unknown_wrapper_is_a_candidate_and_schema_one_remains_readable() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let wrapper = temp.path().join("my-development-wrapper");
    std::fs::write(&wrapper, "#!/bin/sh\nexec /bin/sh \"$@\"\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = LaunchEnvironment::from_variables(BTreeMap::from([
        ("HOME".into(), temp.path().to_string_lossy().into()),
        ("SHELL".into(), wrapper.to_string_lossy().into()),
    ]))
    .unwrap();
    assert_eq!(discover_shells(&env)[0].path, wrapper);
    let old: Workspace =
        toml::from_str("schema = 1\nrevision = 0\nprojects = []\nrecent_projects = []\n").unwrap();
    old.validate().unwrap();
    let future: Workspace = toml::from_str("schema = 999\nrevision = 0\n").unwrap();
    assert!(future.validate().is_err());
}

#[test]
fn explicit_wrapper_executes_literal_arguments_without_csh_inspection() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let wrapper = temp.path().join("custom-wrapper");
    std::fs::write(&wrapper, "#!/bin/sh\nprintf '%s\\n' \"$1\" > received\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let literal = "a space;$(touch injected)'\\\"";
    let plan = idk_workspace::shell::ShellPlan {
        native: Some(NativeShell {
            args: vec![literal.into()],
            env: BTreeMap::new(),
        }),
        shell: wrapper,
        login: false,
        init_cwd: temp.path().into(),
        start_cwd: temp.path().into(),
        sources: vec![],
        env: BTreeMap::from([
            ("HOME".into(), temp.path().to_string_lossy().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]),
        command: None,
    };
    let prepared = plan
        .prepare(
            &temp.path().join("resources"),
            std::path::Path::new("/unused-launcher"),
        )
        .unwrap();
    let mut terminal = TerminalSession::spawn(prepared.command, 24, 80, 100).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while terminal.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(temp.path().join("received")).unwrap(),
        format!("{literal}\n")
    );
    assert!(!temp.path().join("injected").exists());
}

#[test]
fn schema_migration_preserves_original_and_future_configuration_is_not_reset() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&temp.path().join("data"))).unwrap();
    let mut old = Workspace {
        schema: 1,
        ..Workspace::default()
    };
    store.save(&mut old, 0).unwrap();
    let original = std::fs::read(store.config_path()).unwrap();
    let service = ProjectService { store: &store };
    let preview = service
        .preview_connect(ConnectDraft {
            name: "new-native".into(),
            root: temp.path().into(),
            shell: ShellConfig {
                native: Some(NativeShell::default()),
                executable: "/bin/sh".into(),
                login: false,
                init_cwd: temp.path().into(),
                sources: vec![],
                trusted_digest: None,
            },
            terminals: vec![TerminalDraft {
                name: "dev".into(),
                cwd: temp.path().into(),
                sources: vec![],
                persistent: true,
            }],
        })
        .unwrap();
    service.create(preview.revision, preview).unwrap();
    assert_eq!(store.load().unwrap().schema, 2);
    assert_eq!(
        std::fs::read(store.config_dir.join("workspace.schema1.toml")).unwrap(),
        original
    );
    store.update(|_| Ok(())).unwrap();
    assert_eq!(
        std::fs::read(store.config_dir.join("workspace.schema1.toml")).unwrap(),
        original
    );
    let future = b"schema = 999\nrevision = 99\n";
    idk_workspace::store::atomic_write(&store.config_path(), future).unwrap();
    assert!(store.load().is_err());
    assert_eq!(std::fs::read(store.config_path()).unwrap(), future);
}

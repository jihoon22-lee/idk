use idk_workspace::discovery::{
    discover_tasks, inspect_optional_tool, optional_task_draft, snapshot_artifact, OptionalTool,
};
use idk_workspace::editor::{discover_editor, parse_editor_argv};
use idk_workspace::model::{EditorConfig, TaskExecution};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path};

fn executable(path: &Path, script: &str) {
    fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn environment(bin: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([("PATH".into(), bin.to_str().unwrap().into())])
}
#[test]
fn manifests_are_read_only_and_candidates_preserve_literal_argv() {
    let root = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    executable(&bin.path().join("cargo"), "touch SHOULD_NOT_RUN");
    executable(&bin.path().join("npm"), "touch SHOULD_NOT_RUN");
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(
        root.path().join("package.json"),
        r#"{"scripts":{"test $(touch hacked)":"echo ignored","build":"echo ignored"}}"#,
    )
    .unwrap();
    let report = discover_tasks(root.path(), &environment(bin.path())).unwrap();
    assert!(report.warnings.is_empty());
    assert_eq!(report.candidates.len(), 4);
    let candidate = report
        .candidates
        .iter()
        .find(|c| c.name.contains("$("))
        .unwrap();
    assert_eq!(candidate.args, ["run", "test $(touch hacked)"]);
    let draft = candidate.task_definition().unwrap();
    assert!(draft.approved_digest.is_none());
    assert_eq!(draft.cwd, root.path());
    assert!(!root.path().join("idk.toml").exists());
    assert!(!root.path().join("SHOULD_NOT_RUN").exists());
    assert!(!root.path().join("hacked").exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}
#[test]
fn missing_tools_and_malformed_or_special_manifests_are_optional() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
    fs::write(root.path().join("package.json"), "{").unwrap();
    let report = discover_tasks(root.path(), &BTreeMap::new()).unwrap();
    assert_eq!(report.candidates.len(), 2);
    assert!(report
        .candidates
        .iter()
        .all(|c| !c.available && c.missing_tool.as_deref() == Some("cargo")));
    assert!(report.candidates[0].task_definition().is_err());
    assert_eq!(report.warnings.len(), 1);
    fs::remove_file(root.path().join("package.json")).unwrap();
    std::os::unix::fs::symlink("/dev/zero", root.path().join("package.json")).unwrap();
    assert_eq!(
        discover_tasks(root.path(), &BTreeMap::new())
            .unwrap()
            .warnings
            .len(),
        1
    );
}
#[test]
fn editor_quoting_is_literal_and_missing_explicit_preference_never_falls_back() {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("editor with spaces");
    executable(&program, "exit 0");
    executable(&root.path().join("vi"), "exit 0");
    let mut env = environment(root.path());
    env.insert(
        "VISUAL".into(),
        format!("'{}' '--name=a b' '$(touch secret)'", program.display()),
    );
    let candidate = discover_editor(None, &env, root.path()).unwrap().unwrap();
    assert!(candidate.available);
    assert_eq!(candidate.config.executable, program);
    assert_eq!(
        candidate.config.args,
        ["--name=a b", "$(touch secret)", "--", "{file}"]
    );
    assert!(!root.path().join("secret").exists());
    let config = EditorConfig {
        executable: root.path().join("missing"),
        args: vec!["--".into(), "{file}".into()],
        external_gui: false,
    };
    let candidate = discover_editor(Some(&config), &env, root.path())
        .unwrap()
        .unwrap();
    assert!(!candidate.available);
    assert_eq!(candidate.config.executable, config.executable);
    assert_eq!(
        parse_editor_argv("'a b' \"\" c\\ d '$HOME;echo' ").unwrap(),
        ["a b", "", "c d", "$HOME;echo"]
    );
    assert!(parse_editor_argv("'unterminated").is_err());
}
#[test]
fn artifacts_measure_bytes_and_reject_symlink_fifo_and_size() {
    let root = tempfile::tempdir().unwrap();
    let artifact = root.path().join("artifact");
    fs::write(&artifact, b"abc").unwrap();
    let snapshot = snapshot_artifact(&artifact, 8).unwrap();
    assert_eq!(
        snapshot.sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(snapshot.size, 3);
    assert!(snapshot_artifact(&artifact, 2).is_err());
    fs::write(&artifact, b"abcd").unwrap();
    assert_ne!(snapshot_artifact(&artifact, 8).unwrap(), snapshot);
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&artifact, &link).unwrap();
    assert!(snapshot_artifact(&link, 8).is_err());
    let fifo = root.path().join("fifo");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(snapshot_artifact(&fifo, 8).is_err());
}
#[test]
fn explicit_optional_version_and_drafts_preserve_args_exit_and_result_paths() {
    let root = tempfile::tempdir().unwrap();
    let ici = root.path().join("ici space ' quote");
    executable(
        &ici,
        "if [ \"$1\" = --version ]; then echo 'ici 0.13.0'; else printf '%s\\n' \"$@\"; exit 3; fi",
    );
    let info = inspect_optional_tool(OptionalTool::Ici, &ici, &environment(root.path())).unwrap();
    assert!(info.supported, "{:?}", info.reason);
    let task = optional_task_draft(
        &info,
        root.path(),
        &[
            "analyze".into(),
            "--component".into(),
            "a' $(touch BAD)".into(),
        ],
    )
    .unwrap();
    let TaskExecution::Script { script, .. } = task.execution.unwrap() else {
        panic!("expected script")
    };
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .current_dir(root.path())
        .env("IDK_RESULT_PATH", root.path().join("result space.json"))
        .env("IDK_EVENTS_PATH", root.path().join("events.jsonl"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stdout).contains("a' $(touch BAD)"));
    assert!(!root.path().join("BAD").exists());
    executable(&ici, "echo 'ici 9.0.0'");
    assert!(optional_task_draft(&info, root.path(), &[]).is_err());
    assert!(
        !inspect_optional_tool(OptionalTool::Ici, &ici, &environment(root.path()))
            .unwrap()
            .supported
    );
    let missing = inspect_optional_tool(
        OptionalTool::Lens,
        &root.path().join("missing"),
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(!missing.supported);
}
#[test]
fn lens_draft_redirects_only_result_and_preserves_exit() {
    let root = tempfile::tempdir().unwrap();
    let lens = root.path().join("lens");
    executable(&lens, "if [ \"$1\" = --version ]; then echo 'lens 0.10.0'; else echo '{\"complete\":false}'; echo diagnostic >&2; exit 1; fi");
    let info = inspect_optional_tool(OptionalTool::Lens, &lens, &environment(root.path())).unwrap();
    assert!(info.supported);
    let task = optional_task_draft(
        &info,
        root.path(),
        &[
            "build".into(),
            "inspect".into(),
            "compile commands.json".into(),
        ],
    )
    .unwrap();
    let TaskExecution::Script { script, .. } = task.execution.unwrap() else {
        panic!("expected script")
    };
    let result = root.path().join("result.json");
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .env("IDK_RESULT_PATH", &result)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"diagnostic\n");
    assert_eq!(
        fs::read_to_string(result).unwrap(),
        "{\"complete\":false}\n"
    );
}

#[test]
fn version_probes_are_bounded_and_never_accept_partial_or_failed_versions() {
    let root = tempfile::tempdir().unwrap();
    let tool = root.path().join("ici");
    for script in [
        "echo ici 0.13.0; exit 1",
        "echo unrelated 0.13.0",
        "echo ici future",
        "while :; do printf xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done",
    ] {
        executable(&tool, script);
        let result =
            inspect_optional_tool(OptionalTool::Ici, &tool, &environment(root.path())).unwrap();
        assert!(!result.supported, "{script}");
    }
    executable(&tool, "while :; do :; done");
    let start = std::time::Instant::now();
    let result =
        inspect_optional_tool(OptionalTool::Ici, &tool, &environment(root.path())).unwrap();
    assert!(!result.supported);
    assert!(result.reason.unwrap().contains("timed out"));
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn version_probe_kills_owned_descendants_and_preserves_unrelated() {
    const CHILD_MODE: &str = "IDK_TEST_VERSION_DESCENDANT_SUPERVISOR";
    if std::env::var_os(CHILD_MODE).is_none() {
        // Isolate subreaper state from concurrently running tests. The supervisor
        // can reap the probe's orphaned child instead of leaving it to PID 1.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "version_probe_kills_owned_descendants_and_preserves_unrelated",
                "--nocapture",
            ])
            .env(CHILD_MODE, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let root = tempfile::tempdir().unwrap();
    let tool = root.path().join("ici");
    executable(
        &tool,
        "/bin/sleep 30 &\nprintf '%s' \"$!\" > \"$CHILD_PID\"\nwhile :; do :; done",
    );
    let child_file = root.path().join("child.pid");
    let mut env = environment(root.path());
    env.insert("CHILD_PID".into(), child_file.to_str().unwrap().into());
    let mut unrelated = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let inspection = inspect_optional_tool(OptionalTool::Ici, &tool, &env);
    let unrelated_live = unrelated.try_wait().unwrap().is_none();
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    let child_pid = fs::read_to_string(child_file)
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    let mut status = 0;
    let reaped = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
    if reaped == 0 {
        unsafe {
            libc::kill(child_pid, libc::SIGKILL);
            libc::waitpid(child_pid, &mut status, 0);
        }
    }
    assert_eq!(
        reaped, child_pid,
        "owned descendant was not killed by bounded probe cleanup"
    );
    assert!(libc::WIFSIGNALED(status));
    assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
    assert!(unrelated_live);
    let inspection = inspection.unwrap();
    assert!(!inspection.supported);
    // While the supervisor had not reaped the zombie, cleanup was unconfirmed;
    // do not publish a successful inspection merely because SIGKILL was sent.
    assert!(inspection
        .reason
        .unwrap()
        .contains("cleanup is unconfirmed"));
}

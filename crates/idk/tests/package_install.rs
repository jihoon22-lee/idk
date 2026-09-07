//! Explicit candidate tests. Run after the archive is built, never against a
//! substituted debug binary or a fixture with only an ELF-shaped header.
use base64::Engine;
use idk_workspace::client::Client;
use idk_workspace::install::{health, Installer};
use idk_workspace::model::{new_id, FailurePolicy, ShellConfig, TaskDefinition, TaskLogging};
use idk_workspace::package::{sha256, VerifiedBundle, BINARY, CHECKSUMS, MANIFEST};
use idk_workspace::project::{ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft};
use idk_workspace::run_wire::{
    LogState, RunInfo, RunJobState, RunRequest, RunResult, RunState, StepResult,
};
use idk_workspace::store::{atomic_write, Store};
use idk_workspace::task::TaskService;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct OwnedHost(Child);
impl Drop for OwnedHost {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn supplied_bundle() -> VerifiedBundle {
    let path = PathBuf::from(
        std::env::var_os("IDK_TEST_BUNDLE")
            .expect("set IDK_TEST_BUNDLE to the actual candidate archive"),
    );
    let digest = std::env::var("IDK_TEST_BUNDLE_SHA256")
        .expect("set IDK_TEST_BUNDLE_SHA256 to the verified archive digest");
    VerifiedBundle::read(&path, &digest).unwrap()
}
fn screen_contains(client: &mut Client, session: &str, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if client
            .snapshot(session, None)
            .unwrap()
            .screen
            .is_some_and(|screen| screen.text().contains(text))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "candidate screen did not contain {text}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn run_job(client: &mut Client, request: RunRequest) -> RunResult {
    let mut job = client.run(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if job.state != RunJobState::Pending {
            assert_eq!(job.state, RunJobState::Complete, "{job:?}");
            return job.result.unwrap();
        }
        assert!(Instant::now() < deadline, "packaged Run job timed out");
        std::thread::sleep(Duration::from_millis(20));
        job = client.run(RunRequest::Job { job_id: job.job_id }).unwrap();
    }
}
fn run_info(client: &mut Client, id: &str) -> RunInfo {
    let RunResult::Run(run) = run_job(client, RunRequest::Info { run_id: id.into() }) else {
        panic!("expected Run information")
    };
    run
}
fn run_log(client: &mut Client, id: &str) -> Vec<u8> {
    let info = run_info(client, id);
    let RunResult::Log(log) = run_job(
        client,
        RunRequest::Log {
            run_id: id.into(),
            generation: info.log.generation,
            offset: 0,
            limit: 65536,
        },
    ) else {
        panic!("expected raw Run log")
    };
    base64::engine::general_purpose::STANDARD
        .decode(log.data_base64)
        .unwrap()
}

// Only the appended non-loadable identity marker differs. This exercises a
// real executable with a different build SHA, not a second release candidate.
fn changed_identity_bundle(directory: &std::path::Path) -> VerifiedBundle {
    let mut files = BTreeMap::new();
    for name in [
        BINARY,
        MANIFEST,
        CHECKSUMS,
        "idk-third-party-licenses.json",
        "idk-THIRD-PARTY-NOTICES.txt",
    ] {
        files.insert(name, fs::read(directory.join(name)).unwrap());
    }
    files
        .get_mut(BINARY)
        .unwrap()
        .extend_from_slice(b"\nIDK_PACKAGE_TEST_ONLY_IDENTITY_VARIANT\n");
    let mut manifest: serde_json::Value = serde_json::from_slice(&files[MANIFEST]).unwrap();
    manifest["size"] = serde_json::json!(files[BINARY].len());
    manifest["sha256"] = serde_json::json!(sha256(&files[BINARY]));
    manifest["source"]["dirty"] = serde_json::json!(true);
    manifest["main_sha_verified"] = serde_json::json!(false);
    manifest["candidate_kind"] = serde_json::json!("dirty-development");
    manifest["fixture"] =
        serde_json::json!("test-only appended binary identity; never a release candidate");
    files.insert(MANIFEST, serde_json::to_vec(&manifest).unwrap());
    let checksums = files
        .iter()
        .filter(|(name, _)| **name != CHECKSUMS)
        .map(|(name, bytes)| format!("{}  {name}\n", sha256(bytes)))
        .collect::<String>();
    files.insert(CHECKSUMS, checksums.into_bytes());
    let gzip = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gzip);
    for (name, bytes) in files {
        let mut header = tar::Header::new_ustar();
        header.set_path(name).unwrap();
        header.set_size(bytes.len() as u64);
        header.set_mode(if name == BINARY { 0o755 } else { 0o644 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        tar.append(&header, bytes.as_slice()).unwrap();
    }
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    VerifiedBundle::from_bytes(&bytes, &sha256(&bytes)).unwrap()
}

#[test]
#[ignore = "requires the actual built bundle and an installed csh/tcsh"]
fn actual_packaged_install_update_and_uninstall_preserve_live_shell_run_and_logs() {
    let bundle = supplied_bundle();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let store = Store::open(Some(&root.join("data"))).unwrap();
    let installer = Installer::open(&root.join("installation")).unwrap();
    let initial = installer.install(&bundle, &store).unwrap();
    let original_binary = root
        .join("installation/generations")
        .join(&initial.active.name)
        .join(BINARY);
    let original_directory = original_binary.parent().unwrap();
    let shell = std::env::var_os("IDK_TEST_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/bin/tcsh".into());
    assert!(shell.is_file(), "a real csh/tcsh is required");
    let source = root.join("source 한글");
    fs::create_dir(&source).unwrap();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let setup = source.join("setup.csh");
    fs::write(
        &setup,
        "set package_local = retained\necho initialized >> source-count\necho PACKAGE_READY\n",
    )
    .unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home.to_str().unwrap().into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TERM".into(), "xterm-256color".into()),
        ("LANG".into(), "C.UTF-8".into()),
    ]);
    let service = ProjectService { store: &store };
    let preview = service
        .preview_connect(ConnectDraft {
            name: "Package fixture".into(),
            root: source.clone(),
            shell: ShellConfig {
                executable: shell,
                login: false,
                init_cwd: source.clone(),
                sources: vec![setup.into()],
                trusted_digest: None,
            },
            terminals: vec![TerminalDraft {
                name: "Build shell".into(),
                cwd: source.clone(),
                sources: Vec::new(),
                persistent: true,
            }],
        })
        .unwrap();
    let project = service.create(preview.revision, preview).unwrap();
    let environment = LaunchEnvironment::from_variables(env.clone()).unwrap();
    let review = service
        .review_initialization(&project.id, &environment)
        .unwrap();
    service
        .approve_initialization(review.revision, review)
        .unwrap();
    let task=TaskDefinition{id:new_id(),name:"Package live Run".into(),command:"echo PACKAGE_RUN_READY_$package_local\nwhile (! -e run-release)\n echo PACKAGE_RUN_TICK\n sleep 0.1\nend\necho PACKAGE_RUN_DONE".into(),cwd:source.clone(),sources:Vec::new(),artifact:None,approved_digest:None,steps:Vec::new(),failure_policy:FailurePolicy::Stop,logging:TaskLogging::Raw,interactive:false,build_outputs:Vec::new(),artifact_from_task:None,timeout_seconds:Some(60)};
    let tasks = TaskService { store: &store };
    tasks
        .save(store.load().unwrap().revision, &project.id, task.clone())
        .unwrap();
    let review = tasks.review(&project.id, &task.id, &environment).unwrap();
    tasks
        .approve(
            review.revision,
            &project.id,
            &task.id,
            &review.digest,
            &environment,
        )
        .unwrap();
    let project = store.load().unwrap().project(&project.id).unwrap().clone();
    let mut host = OwnedHost(
        Command::new(&original_binary)
            .arg("__host")
            .arg("--config-dir")
            .arg(&store.config_dir)
            .arg("--state-dir")
            .arg(&store.state_dir)
            .arg("--runtime-dir")
            .arg(&store.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut client = loop {
        if let Ok(client) = Client::connect_with_launcher(&store, &original_binary) {
            break client;
        }
        assert!(host.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    let session = client
        .start(
            &project.id,
            &project.terminals[0].id,
            env.clone(),
            24,
            80,
            false,
        )
        .unwrap();
    screen_contains(&mut client, &session.session_id, "PACKAGE_READY");
    let attached = client.attach(&session.session_id, false).unwrap();
    let pid = attached.child_pid;
    client
        .input(
            &session.session_id,
            attached.input_epoch,
            b"set package_local = changed\n",
        )
        .unwrap();
    client
        .detach(&session.session_id, attached.input_epoch)
        .unwrap();
    let RunResult::Started(started) = run_job(
        &mut client,
        RunRequest::Start {
            project_id: project.id.clone(),
            task_id: task.id.clone(),
            operation_id: new_id(),
            environment: env.clone(),
            parallel: false,
            rows: 24,
            cols: 80,
        },
    ) else {
        panic!("expected registered Run");
    };
    let run_id = started.run.run_id;
    let run_session = started.run.session_id.unwrap();
    screen_contains(&mut client, &run_session, "PACKAGE_RUN_READY_retained");
    let run_pid = client
        .snapshot(&run_session, None)
        .unwrap()
        .session
        .child_pid
        .expect("live Run leader");
    let before_log = run_log(&mut client, &run_id);
    assert!(String::from_utf8_lossy(&before_log).contains("PACKAGE_RUN_READY_retained"));
    let run_ledger_before = fs::read(store.state_dir.join("runs.json")).unwrap();
    assert_eq!(health(&store).unwrap().state_schemas.len(), 3);
    assert_eq!(
        fs::read(store.state_dir.join("runs.json")).unwrap(),
        run_ledger_before
    );
    let second = changed_identity_bundle(original_directory);
    let updated = installer.install(&second, &store).unwrap();
    let new_binary = root.join("installation/idk");
    assert!(
        Client::connect_with_launcher(&store, &new_binary).is_err(),
        "new build must not silently replace the old host"
    );
    assert!(host.0.try_wait().unwrap().is_none());
    let mut reattached = Client::connect_with_launcher(&store, &original_binary).unwrap();
    let live = reattached.attach(&session.session_id, false).unwrap();
    assert_eq!(live.child_pid, pid);
    let run = run_info(&mut reattached, &run_id);
    assert_eq!(run.state, RunState::Running);
    assert_eq!(run.session_id.as_deref(), Some(run_session.as_str()));
    assert_eq!(
        reattached
            .snapshot(&run_session, None)
            .unwrap()
            .session
            .child_pid,
        Some(run_pid)
    );
    let updated_log = run_log(&mut reattached, &run_id);
    assert!(updated_log.starts_with(&before_log));
    assert_eq!(
        fs::read(store.state_dir.join("runs.json")).unwrap(),
        run_ledger_before,
        "installation health rewrote the live Run ledger"
    );
    reattached
        .input(
            &session.session_id,
            live.input_epoch,
            b"echo PACKAGE_VALUE_$package_local\n",
        )
        .unwrap();
    screen_contains(
        &mut reattached,
        &session.session_id,
        "PACKAGE_VALUE_changed",
    );
    assert_eq!(
        fs::read_to_string(source.join("source-count"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let later = b"new run metadata created after update";
    atomic_write(&store.state_dir.join("later-result.txt"), later).unwrap();
    installer.uninstall_entrypoints().unwrap();
    assert_eq!(
        fs::read(store.state_dir.join("runs.json")).unwrap(),
        run_ledger_before,
        "uninstall rewrote Run metadata"
    );
    assert_eq!(run_info(&mut reattached, &run_id).state, RunState::Running);
    assert_eq!(
        reattached
            .snapshot(&run_session, None)
            .unwrap()
            .session
            .child_pid,
        Some(run_pid)
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let after_uninstall_log = loop {
        let bytes = run_log(&mut reattached, &run_id);
        if bytes.len() > updated_log.len() {
            break bytes;
        }
        assert!(
            Instant::now() < deadline,
            "live Run output stopped after uninstall"
        );
        std::thread::sleep(Duration::from_millis(30));
    };
    assert!(after_uninstall_log.starts_with(&updated_log));
    assert!(original_binary.is_file());
    assert!(host.0.try_wait().unwrap().is_none());
    assert_eq!(
        fs::read(store.state_dir.join("later-result.txt")).unwrap(),
        later
    );
    assert!(root
        .join("installation/generations")
        .join(updated.active.name)
        .is_dir());
    reattached
        .input(
            &session.session_id,
            live.input_epoch,
            b"echo AFTER_UNINSTALL\n",
        )
        .unwrap();
    screen_contains(&mut reattached, &session.session_id, "AFTER_UNINSTALL");
    reattached
        .detach(&session.session_id, live.input_epoch)
        .unwrap();
    fs::write(source.join("run-release"), b"finish owned fixture\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let run = run_info(&mut reattached, &run_id);
        if !run.state.is_live() && run.cleanup_confirmed && run.log.state == LogState::Complete {
            assert_eq!(run.state, RunState::Succeeded);
            assert_eq!(run.exit_code, Some(0));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Run cleanup remained pending after explicit fixture completion"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        String::from_utf8_lossy(&run_log(&mut reattached, &run_id)).contains("PACKAGE_RUN_DONE")
    );
    assert_eq!(
        fs::read_to_string(source.join("source-count"))
            .unwrap()
            .lines()
            .count(),
        2,
        "update/uninstall repeated initialization"
    );
    let preview = reattached.preview_close(None).unwrap();
    let targets = preview
        .targets
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<Vec<_>>();
    reattached.shutdown(&targets, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut original = fs::File::open(original_binary).unwrap();
    let mut bytes = Vec::new();
    original.read_to_end(&mut bytes).unwrap();
    assert_eq!(sha256(&bytes), bundle.review().manifest.sha256);
}

#[path = "common/run_fixture.rs"]
mod run_fixture;
fn health_ledgers(root: &std::path::Path) -> BTreeMap<String, serde_json::Value> {
    let mut run = run_fixture::run(root);
    let session = new_id();
    run.session_id = Some(session.clone());
    run.state = RunState::Running;
    run.cleanup_confirmed = false;
    run.finished_at_ms = None;
    run.exit_code = None;
    run.steps = vec![StepResult {
        index: 0,
        name: "registered command".into(),
        exit_code: None,
    }];
    run.log.state = LogState::Recording;
    BTreeMap::from([
        (
            "host-sessions.json".into(),
            serde_json::json!({"schema":1,"sessions":[{"session_id":session,"project_id":run.project_id,"terminal_id":run.run_id,"name":run.name,"persistent":false,"state":"running","definition_revision":4,"launch_digest":run.launch_digest,"exit":null,"purpose":"run","run_id":run.run_id}]}),
        ),
        (
            "runs.json".into(),
            serde_json::json!({"schema":1,"runs":[run]}),
        ),
        (
            "git-operations.json".into(),
            serde_json::json!({"schema":1,"operations":[{"id":new_id(),"context_id":new_id(),"project_id":new_id(),"repository":{"root":root,"git_dir":root.join(".git"),"common_dir":root.join(".git")},"kind":"switch_branch","state":"unknown","cleanup_acknowledged":false,"outcome":null,"exit_code":null,"commit":null,"error":"unconfirmed previous operation"}]}),
        ),
    ])
}
fn directory_names(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}
#[test]
fn health_validates_all_typed_ledgers_without_restoring_or_creating_runtime_state() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&temp.path().join("data"))).unwrap();
    let ledgers = health_ledgers(temp.path());
    for (name, ledger) in &ledgers {
        store.write_state(name, ledger).unwrap();
    }
    let before = ledgers
        .keys()
        .map(|name| {
            let path = store.state_dir.join(name);
            let metadata = fs::metadata(&path).unwrap();
            (
                name.clone(),
                (
                    fs::read(path).unwrap(),
                    metadata.ino(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let state_names = directory_names(&store.state_dir);
    let runtime_names = directory_names(&store.runtime_dir);
    let report = health(&store).unwrap();
    assert_eq!(report.state_schemas.len(), 3);
    for name in ledgers.keys() {
        assert_eq!(report.state_schemas[name], 1);
        let path = store.state_dir.join(name);
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(
            (
                fs::read(path).unwrap(),
                metadata.ino(),
                metadata.mtime(),
                metadata.mtime_nsec()
            ),
            before[name]
        );
    }
    assert_eq!(directory_names(&store.state_dir), state_names);
    assert_eq!(directory_names(&store.runtime_dir), runtime_names);
    assert!(!store.state_dir.join("run-logs").exists());
    assert!(!store.socket_path().exists());
    let run: serde_json::Value = serde_json::from_slice(&before["runs.json"].0).unwrap();
    assert_eq!(run["runs"][0]["state"], "Running");
    assert_eq!(run["runs"][0]["cleanup_confirmed"], false);
}
#[test]
fn malformed_future_and_inconsistent_typed_records_are_rejected_and_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&temp.path().join("data"))).unwrap();
    let good = health_ledgers(temp.path());
    let mut bad = Vec::new();
    for name in good.keys() {
        let mut future = good[name].clone();
        future["schema"] = serde_json::json!(99);
        bad.push((name.clone(), future));
        bad.push((name.clone(), serde_json::json!({"schema":1})));
    }
    let mut host = good["host-sessions.json"].clone();
    host["sessions"][0]["state"] = serde_json::json!("invented_state");
    bad.push(("host-sessions.json".into(), host));
    let mut host = good["host-sessions.json"].clone();
    host["sessions"][0]["session_id"] = serde_json::json!("not-an-id");
    bad.push(("host-sessions.json".into(), host));
    let mut host = good["host-sessions.json"].clone();
    let duplicate = host["sessions"][0].clone();
    host["sessions"].as_array_mut().unwrap().push(duplicate);
    bad.push(("host-sessions.json".into(), host));
    let mut host = good["host-sessions.json"].clone();
    host["sessions"][0]["purpose"] = serde_json::json!("editor");
    bad.push(("host-sessions.json".into(), host));
    for (field, value) in [
        (
            "steps",
            serde_json::json!([{"index":0,"name":"step","exit_code":999}]),
        ),
        ("launch_digest", serde_json::json!("broken")),
        (
            "source_start",
            serde_json::json!({"identity":null,"generation":0,"git_head":"not-a-Git-id","dirty":false,"status_digest":null,"error":null}),
        ),
    ] {
        let mut run = good["runs.json"].clone();
        run["runs"][0][field] = value;
        bad.push(("runs.json".into(), run));
    }
    let mut run = good["runs.json"].clone();
    run["runs"][0]["log"]["run_id"] = serde_json::json!(new_id());
    bad.push(("runs.json".into(), run));
    let mut run = good["runs.json"].clone();
    run["runs"][0]["log"]["bytes"] = serde_json::json!("text instead of counter");
    bad.push(("runs.json".into(), run));
    let mut run = good["runs.json"].clone();
    let duplicate = run["runs"][0].clone();
    run["runs"].as_array_mut().unwrap().push(duplicate);
    bad.push(("runs.json".into(), run));
    let mut git = good["git-operations.json"].clone();
    git["operations"][0]["commit"] = serde_json::json!("not-object-id");
    bad.push(("git-operations.json".into(), git));
    let mut git = good["git-operations.json"].clone();
    git["operations"][0]["repository"]["root"] = serde_json::json!("relative/path");
    bad.push(("git-operations.json".into(), git));
    let mut git = good["git-operations.json"].clone();
    git["operations"][0]["state"] = serde_json::json!("running");
    git["operations"][0]["cleanup_acknowledged"] = serde_json::json!(true);
    bad.push(("git-operations.json".into(), git));
    let mut git = good["git-operations.json"].clone();
    let duplicate = git["operations"][0].clone();
    git["operations"].as_array_mut().unwrap().push(duplicate);
    bad.push(("git-operations.json".into(), git));
    for (name, broken) in bad {
        for (name, value) in &good {
            store.write_state(name, value).unwrap();
        }
        store.write_state(&name, &broken).unwrap();
        let path = store.state_dir.join(&name);
        let bytes = fs::read(&path).unwrap();
        let names = directory_names(&store.state_dir);
        assert!(
            health(&store).is_err(),
            "accepted malformed {name}: {broken}"
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(directory_names(&store.state_dir), names);
        assert!(directory_names(&store.runtime_dir).is_empty());
    }
}
#[test]
fn read_only_health_does_not_create_missing_directories() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store {
        config_dir: temp.path().join("missing-config"),
        state_dir: temp.path().join("missing-state"),
        runtime_dir: temp.path().join("missing-runtime"),
    };
    assert!(health(&store).is_err());
    assert!(directory_names(temp.path()).is_empty());
}

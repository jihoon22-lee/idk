//! Real owned PTYs and real Git processes; hook/signing/SSH authentication
//! programs below are controlled fixtures, not external authentication evidence.
use idk_workspace::git::*;
use idk_workspace::model::SourceGate;
use idk_workspace::terminal::{TerminalExit, TerminalSession};
use portable_pty::CommandBuilder;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

const ANSWER: &str = "fixture-private-answer";
const PROMPT: &str = "FIXTURE_TTY_READY";
struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    service: GitService,
    gate: SourceGate,
}
impl Fixture {
    fn new(extra_environment: BTreeMap<String, String>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Fixture"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        fs::write(root.join("base"), "base\n").unwrap();
        git(&root, &["add", "base"]);
        git(&root, &["commit", "-m", "initial"]);
        let mut environment: BTreeMap<_, _> = [
            ("HOME".into(), temp.path().to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
        ]
        .into();
        environment.extend(extra_environment);
        let service = GitService::new(
            GitExecutable::discover().unwrap(),
            Repository::discover(&root).unwrap(),
            environment,
        )
        .unwrap();
        let gate = SourceGate::default();
        gate.ready(service.repository().identity()).unwrap();
        Self {
            temp,
            root,
            service,
            gate,
        }
    }
    fn commit_plan(&self) -> GitOperationPlan {
        fs::write(self.root.join("base"), "changed\n").unwrap();
        git(&self.root, &["add", "base"]);
        self.service
            .plan_commit(
                &self.service.commit_preview().unwrap(),
                "interactive fixture",
            )
            .unwrap()
    }
    fn resources(&self) -> PathBuf {
        self.temp.path().join("resources")
    }
}
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(root)
        .env("HOME", root.parent().unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "fixture Git command failed");
    output.stdout
}
fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn prompt_script(after: &str) -> String {
    format!("exec 3<>/dev/tty\nsaved=$(stty -g <&3)\ntrap 'stty \"$saved\" <&3 2>/dev/null || :; exit 129' HUP INT TERM\nstty -echo <&3\nprintf '{PROMPT}\\n' >&3\nIFS= read -r answer <&3\nstty \"$saved\" <&3\n[ \"$answer\" = '{ANSWER}' ] || exit 47\n{after}")
}
fn wait_prompt(session: &mut TerminalSession) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            session.try_wait().unwrap().is_none(),
            "operation exited before its fixture prompt"
        );
        let snapshot = session.snapshot().unwrap();
        assert!(snapshot.error.is_none(), "operation PTY error");
        if snapshot.text().contains(PROMPT) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "operation fixture prompt timed out"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn wait_exit(session: &mut TerminalSession) -> TerminalExit {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = session.snapshot().unwrap();
        assert!(
            !snapshot.text().contains(ANSWER),
            "hidden input appeared on the terminal"
        );
        assert!(snapshot.error.is_none(), "operation PTY error");
        if let Some(exit) = session.try_wait().unwrap() {
            return exit;
        }
        assert!(Instant::now() < deadline, "owned Git process did not exit");
        thread::sleep(Duration::from_millis(5));
    }
}
fn answer_operation(f: &Fixture, plan: &GitOperationPlan) -> (GitOperationResult, TerminalExit) {
    let mut observed = None;
    let result = f
        .service
        .execute_with(plan, &f.gate, &f.resources(), |builder| {
            let mut session = TerminalSession::spawn(builder, 24, 100, 100)?;
            wait_prompt(&mut session);
            session.input(format!("{ANSWER}\n").as_bytes())?;
            let exit = wait_exit(&mut session);
            let snapshot = session.snapshot()?;
            observed = Some(exit.clone());
            Ok(CommandOutcome {
                exit_code: Some(exit.code),
                cancelled: false,
                output_limited: snapshot.output_limited,
            })
        })
        .unwrap();
    let exit = observed.unwrap();
    assert_eq!(result.exit_code, Some(exit.code));
    let metadata = serde_json::to_string(&result).unwrap();
    assert!(
        !metadata.contains(ANSWER) && !metadata.contains(PROMPT),
        "sensitive PTY transcript entered ordinary metadata"
    );
    (result, exit)
}

#[test]
fn interactive_hook_uses_owned_tty_and_does_not_inject_into_busy_terminal() {
    let f = Fixture::new(BTreeMap::new());
    script(
        &f.root.join(".git/hooks/pre-commit"),
        &prompt_script("printf 'HOOK_ACCEPTED\\n' >&3\nexit 0"),
    );
    let mut busy_command = CommandBuilder::new("/bin/sh");
    busy_command.args([
        "-c",
        "printf 'BUSY_READY\\n'; while IFS= read -r line; do printf 'UNEXPECTED_INPUT\\n'; done",
    ]);
    let mut busy = TerminalSession::spawn(busy_command, 12, 80, 100).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while !busy.snapshot().unwrap().text().contains("BUSY_READY") {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let (result, exit) = answer_operation(&f, &f.commit_plan());
    assert_eq!(exit.code, 0);
    assert_eq!(result.outcome, GitOutcome::Succeeded);
    assert!(result.commit.is_some());
    assert!(busy.try_wait().unwrap().is_none());
    let screen = busy.snapshot().unwrap().text();
    assert!(!screen.contains("UNEXPECTED_INPUT") && !screen.contains(ANSWER));
    busy.terminate().unwrap();
    wait_exit(&mut busy);
}

#[test]
fn configured_signing_fixture_reads_hidden_tty_input_and_its_failure_is_preserved() {
    let f = Fixture::new(BTreeMap::new());
    let signer = f.temp.path().join("signer");
    // Deliberately fail after reading the passphrase. This proves the configured
    // signing path is honored without claiming a real cryptographic signature.
    script(
        &signer,
        &prompt_script("printf 'SIGNING_FIXTURE_REJECTED\\n' >&3\nexit 23"),
    );
    git(
        &f.root,
        &["config", "gpg.program", signer.to_str().unwrap()],
    );
    git(&f.root, &["config", "commit.gpgSign", "true"]);
    let (result, exit) = answer_operation(&f, &f.commit_plan());
    assert_ne!(exit.code, 0);
    assert_eq!(result.outcome, GitOutcome::Failed);
    assert!(result.commit.is_none());
    assert_eq!(f.service.history(10).unwrap().len(), 1);
    assert_eq!(f.service.commit_preview().unwrap().staged.len(), 1);
}

#[test]
fn controlled_ssh_authentication_fixture_pushes_to_actual_local_bare_git() {
    let transport_temp = tempfile::tempdir().unwrap();
    let remote = transport_temp.path().join("remote.git");
    fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "--bare"]);
    let ssh = transport_temp.path().join("ssh-fixture");
    // Git invokes this configured SSH program, whose fixture prompt uses the
    // operation TTY. Packet traffic remains stdin/stdout to actual receive-pack.
    // No network or third-party authentication service is involved.
    script(
        &ssh,
        &prompt_script("exec /usr/bin/git-receive-pack \"$FIXTURE_REMOTE_PATH\""),
    );
    let f = Fixture::new(
        [
            ("GIT_SSH".into(), ssh.to_str().unwrap().into()),
            ("GIT_SSH_VARIANT".into(), "ssh".into()),
            (
                "FIXTURE_REMOTE_PATH".into(),
                remote.to_str().unwrap().into(),
            ),
        ]
        .into(),
    );
    git(
        &f.root,
        &[
            "remote",
            "add",
            "origin",
            "ssh://fixture@localhost/fixture.git",
        ],
    );
    let plan = f
        .service
        .plan_push("origin", "main", &f.service.refresh().unwrap().revision)
        .unwrap();
    let (result, exit) = answer_operation(&f, &plan);
    assert_eq!(exit.code, 0);
    assert_eq!(result.outcome, GitOutcome::Succeeded);
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/main"]),
        git(&f.root, &["rev-parse", "HEAD"])
    );
}

#[test]
fn cancellation_waits_for_real_git_exit_while_source_lease_is_held() {
    let f = Fixture::new(BTreeMap::new());
    git(&f.root, &["branch", "topic"]);
    script(
        &f.root.join(".git/hooks/post-checkout"),
        &prompt_script("exit 0"),
    );
    let before = f.service.refresh().unwrap().revision;
    let plan = f.service.plan_switch("topic", &before).unwrap();
    let mut observed = None;
    let result = f
        .service
        .execute_with(&plan, &f.gate, &f.resources(), |builder| {
            let mut session = TerminalSession::spawn(builder, 24, 100, 100)?;
            wait_prompt(&mut session);
            assert!(f
                .gate
                .reserve_run(f.service.repository().identity(), "during-prompt", "build")
                .is_err());
            session.terminate()?;
            let exit = wait_exit(&mut session);
            assert!(f
                .gate
                .reserve_run(
                    f.service.repository().identity(),
                    "after-exit-before-return",
                    "build"
                )
                .is_err());
            observed = Some(exit.clone());
            Ok(CommandOutcome {
                exit_code: Some(exit.code),
                cancelled: true,
                output_limited: false,
            })
        })
        .unwrap();
    assert_eq!(result.exit_code, Some(observed.unwrap().code));
    assert_eq!(result.outcome, GitOutcome::Cancelled);
    // checkout changes the branch before running post-checkout: cancellation
    // preserves and reports that actual change instead of pretending rollback.
    assert_eq!(
        result.after.unwrap().revision.head.reference(),
        Some("refs/heads/topic")
    );
    assert!(!result.warnings.is_empty());
    assert!(f
        .gate
        .reserve_run(f.service.repository().identity(), "after-return", "build")
        .is_ok());
}

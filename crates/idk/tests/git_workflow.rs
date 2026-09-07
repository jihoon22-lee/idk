use idk_workspace::git::*;
use idk_workspace::model::SourceGate;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    service: GitService,
    gate: SourceGate,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Fixture"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        let environment: BTreeMap<_, _> = [
            ("HOME".into(), temp.path().to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]
        .into();
        let service = GitService::new(
            GitExecutable::discover().unwrap(),
            Repository::discover(&root).unwrap(),
            environment,
        )
        .unwrap();
        let gate = SourceGate::default();
        Self {
            temp,
            root,
            service,
            gate,
        }
    }
    fn write(&self, path: &str, body: &str) {
        fs::write(self.root.join(path), body).unwrap();
    }
    fn initial(&self) {
        self.write("base", "base\n");
        git(&self.root, &["add", "base"]);
        git(&self.root, &["commit", "-m", "initial"]);
    }
    fn execute(&self, plan: &GitOperationPlan) -> GitOperationResult {
        self.service
            .run_noninteractive(plan, &self.gate, &self.temp.path().join("resources"))
            .unwrap()
    }
    fn stage(&self, paths: &[&str]) -> GitOperationResult {
        let revision = self.service.refresh().unwrap().revision;
        let plan = self
            .service
            .plan_stage(
                paths
                    .iter()
                    .map(|p| GitPath::from_text(p).unwrap())
                    .collect(),
                &revision,
            )
            .unwrap();
        self.execute(&plan)
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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn hook(f: &Fixture, name: &str, body: &str) {
    let path = f.root.join(".git/hooks").join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn unborn_literal_filename_stage_unstage_and_full_index_commit() {
    let f = Fixture::new();
    assert!(matches!(
        f.service.refresh().unwrap().revision.head,
        Head::Unborn { .. }
    ));
    let names = [
        "한글 file",
        "line\nbreak",
        "-option",
        ":(glob)*",
        "outside-selection",
    ];
    for name in names {
        f.write(name, "content\n");
    }
    let raw = b"raw-\xff".to_vec();
    fs::write(f.root.join(OsString::from_vec(raw.clone())), b"raw").unwrap();
    let paths: Vec<_> = names
        .iter()
        .map(|p| GitPath::from_text(p).unwrap())
        .chain([GitPath::from_bytes(raw).unwrap()])
        .collect();
    let snapshot = f.service.refresh().unwrap();
    assert_eq!(snapshot.entries.len(), 6);
    let plan = f
        .service
        .plan_stage(paths.clone(), &snapshot.revision)
        .unwrap();
    assert_eq!(f.execute(&plan).outcome, GitOutcome::Succeeded);
    let plan = f
        .service
        .plan_unstage(
            vec![paths[0].clone()],
            &f.service.refresh().unwrap().revision,
        )
        .unwrap();
    assert_eq!(f.execute(&plan).outcome, GitOutcome::Succeeded);
    assert!(f.root.join(names[0]).exists());
    assert_eq!(f.stage(&[names[0]]).outcome, GitOutcome::Succeeded);
    let preview = f.service.commit_preview().unwrap();
    assert_eq!(preview.staged.len(), 6);
    let plan = f
        .service
        .plan_commit(&preview, "all actual staged files")
        .unwrap();
    let outcome = f.execute(&plan);
    assert_eq!(outcome.outcome, GitOutcome::Succeeded, "{outcome:?}");
    assert_eq!(
        f.service
            .commit_files(&outcome.commit.unwrap().oid)
            .unwrap()
            .len(),
        6
    );
    assert!(f.service.refresh().unwrap().clean());
    assert_eq!(f.service.history(10).unwrap().len(), 1);
    assert!(f
        .service
        .run_noninteractive(&plan.clone(), &f.gate, &f.temp.path().join("resources"))
        .is_err());
}

#[test]
fn index_changes_reject_stale_review_and_unstage_preserves_unstaged_edits() {
    let f = Fixture::new();
    f.initial();
    f.write("base", "staged\n");
    f.stage(&["base"]);
    let preview = f.service.commit_preview().unwrap();
    let plan = f
        .service
        .plan_commit(&preview, "draft retained by caller")
        .unwrap();
    f.write("another", "external stage");
    git(&f.root, &["add", "another"]);
    let mut called = false;
    assert!(f
        .service
        .execute_with(&plan, &f.gate, &f.temp.path().join("resources"), |_| {
            called = true;
            unreachable!()
        })
        .is_err());
    assert!(!called);
    f.write("base", "later unstaged edit\n");
    let plan = f
        .service
        .plan_unstage(
            vec![GitPath::from_text("base").unwrap()],
            &f.service.refresh().unwrap().revision,
        )
        .unwrap();
    assert_eq!(f.execute(&plan).outcome, GitOutcome::Succeeded);
    assert_eq!(
        fs::read_to_string(f.root.join("base")).unwrap(),
        "later unstaged edit\n"
    );
    assert_eq!(preview.staged.len(), 1);
}

#[test]
fn hooks_are_not_bypassed_and_modified_commit_is_reported() {
    let f = Fixture::new();
    f.initial();
    f.write("base", "changed\n");
    f.stage(&["base"]);
    hook(&f, "pre-commit", "printf hook > failed-hook\nexit 1");
    let preview = f.service.commit_preview().unwrap();
    let plan = f.service.plan_commit(&preview, "failure draft").unwrap();
    let outcome = f.execute(&plan);
    assert_eq!(outcome.outcome, GitOutcome::Failed);
    assert!(f.root.join("failed-hook").exists());
    assert_eq!(f.service.history(10).unwrap().len(), 1);
    hook(
        &f,
        "pre-commit",
        "printf injected > hook-added\ngit add hook-added",
    );
    let plan = f
        .service
        .plan_commit(&f.service.commit_preview().unwrap(), "hook changes index")
        .unwrap();
    let outcome = f.execute(&plan);
    assert_eq!(
        outcome.outcome,
        GitOutcome::ChangedAfterExecution,
        "{outcome:?}"
    );
    assert!(outcome.commit.is_some());
    assert_eq!(f.service.history(10).unwrap().len(), 2);
}

#[test]
fn signing_failure_uses_the_configured_program() {
    let f = Fixture::new();
    f.initial();
    f.write("base", "changed\n");
    f.stage(&["base"]);
    let signer = f.temp.path().join("signer");
    fs::write(&signer, "#!/bin/sh\nprintf signer > signer-ran\nexit 1\n").unwrap();
    fs::set_permissions(&signer, fs::Permissions::from_mode(0o700)).unwrap();
    git(&f.root, &["config", "commit.gpgSign", "true"]);
    git(
        &f.root,
        &["config", "gpg.program", signer.to_str().unwrap()],
    );
    let plan = f
        .service
        .plan_commit(&f.service.commit_preview().unwrap(), "signed")
        .unwrap();
    assert_eq!(f.execute(&plan).outcome, GitOutcome::Failed);
    assert!(f.root.join("signer-ran").exists());
}

#[test]
fn reads_suppress_filters_diff_helpers_and_fsmonitor_but_stage_runs_filter() {
    let f = Fixture::new();
    f.initial();
    f.write(".gitattributes", "base filter=fixture diff=fixture\n");
    let helper = f.temp.path().join("helper");
    fs::write(&helper, "#!/bin/sh\nprintf helper > helper-ran\ncat\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    for key in [
        "filter.fixture.clean",
        "diff.fixture.command",
        "diff.fixture.textconv",
        "core.fsmonitor",
    ] {
        git(&f.root, &["config", key, helper.to_str().unwrap()]);
    }
    f.write("base", "new\n");
    assert!(f.service.refresh().unwrap().conversion_filters_disabled);
    let diff = f.service.diff(DiffTarget::Worktree, None).unwrap();
    assert!(diff.text.contains("new"));
    f.service.history(10).unwrap();
    f.service.commit_preview().unwrap();
    assert!(!f.root.join("helper-ran").exists());
    assert_eq!(f.stage(&["base"]).outcome, GitOutcome::Succeeded);
    assert!(f.root.join("helper-ran").exists());
}

#[test]
fn binary_and_large_diff_are_explicitly_bounded() {
    let f = Fixture::new();
    f.initial();
    fs::write(f.root.join("base"), b"\0binary").unwrap();
    assert!(f.service.diff(DiffTarget::Worktree, None).unwrap().binary);
    fs::write(f.root.join("base"), "large line\n".repeat(150_000)).unwrap();
    assert!(
        f.service
            .diff(DiffTarget::Worktree, None)
            .unwrap()
            .truncated
    );
}

#[test]
fn switch_requires_known_idle_gate_and_holds_lease_until_executor_returns() {
    let f = Fixture::new();
    f.initial();
    let create = f.service.plan_branch_create("topic", None).unwrap();
    assert_eq!(f.execute(&create).outcome, GitOutcome::Succeeded);
    let plan = f
        .service
        .plan_switch("topic", &f.service.refresh().unwrap().revision)
        .unwrap();
    assert!(f
        .service
        .run_noninteractive(&plan, &f.gate, &f.temp.path().join("resources"))
        .is_err());
    f.gate.ready(f.service.repository().identity()).unwrap();
    let run = f
        .gate
        .reserve_run(f.service.repository().identity(), "active", "build")
        .unwrap();
    assert!(f
        .service
        .run_noninteractive(&plan, &f.gate, &f.temp.path().join("resources"))
        .is_err());
    drop(run);
    let other = f
        .gate
        .reserve_run(Path::new("/unrelated"), "other", "build");
    assert!(other.is_err());
    let outcome = f
        .service
        .execute_with(&plan, &f.gate, &f.temp.path().join("resources"), |_| {
            assert!(f
                .gate
                .reserve_run(f.service.repository().identity(), "racing", "build")
                .is_err());
            Ok(CommandOutcome {
                exit_code: None,
                cancelled: true,
                output_limited: false,
            })
        })
        .unwrap();
    assert_eq!(outcome.outcome, GitOutcome::Cancelled);
    f.gate.ready(Path::new("/unrelated")).unwrap();
    let _unrelated = f
        .gate
        .reserve_run(Path::new("/unrelated"), "unrelated-run", "build")
        .unwrap();
    let plan = f
        .service
        .plan_switch("topic", &f.service.refresh().unwrap().revision)
        .unwrap();
    assert_eq!(f.execute(&plan).outcome, GitOutcome::Succeeded);
    assert_eq!(
        f.service.refresh().unwrap().revision.head.reference(),
        Some("refs/heads/topic")
    );
    f.write("dirty", "preserve");
    assert!(f
        .service
        .plan_switch("main", &f.service.refresh().unwrap().revision)
        .is_err());
}

#[test]
fn local_remote_fetch_fast_forward_pull_explicit_push_and_non_ff_refusal() {
    let f = Fixture::new();
    f.initial();
    f.gate.ready(f.service.repository().identity()).unwrap();
    let remote = f.temp.path().join("remote.git");
    fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "--bare"]);
    git(
        &f.root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    let push = f
        .service
        .plan_push("origin", "main", &f.service.refresh().unwrap().revision)
        .unwrap();
    assert_eq!(
        push.preview()
            .remote
            .as_ref()
            .unwrap()
            .destination
            .as_deref(),
        Some("refs/heads/main")
    );
    assert_eq!(f.execute(&push).outcome, GitOutcome::Succeeded);
    let peer = f.temp.path().join("peer");
    git(
        f.temp.path(),
        &[
            "clone",
            "-b",
            "main",
            remote.to_str().unwrap(),
            peer.to_str().unwrap(),
        ],
    );
    git(&peer, &["config", "user.name", "Peer"]);
    git(&peer, &["config", "user.email", "peer@example.invalid"]);
    fs::write(peer.join("peer"), "peer change").unwrap();
    git(&peer, &["add", "peer"]);
    git(&peer, &["commit", "-m", "peer"]);
    git(&peer, &["push", "origin", "main"]);
    let fetch = f.service.plan_fetch("origin").unwrap();
    assert_eq!(f.execute(&fetch).outcome, GitOutcome::Succeeded);
    let pull = f
        .service
        .plan_pull_ff_only("origin", "main", &f.service.refresh().unwrap().revision)
        .unwrap();
    assert_eq!(pull.preview().remote.as_ref().unwrap().behind, Some(1));
    assert_eq!(f.execute(&pull).outcome, GitOutcome::Succeeded);
    assert!(f.root.join("peer").exists());
    f.write("ours", "ours");
    f.stage(&["ours"]);
    let plan = f
        .service
        .plan_commit(&f.service.commit_preview().unwrap(), "ours")
        .unwrap();
    assert_eq!(f.execute(&plan).outcome, GitOutcome::Succeeded);
    fs::write(peer.join("theirs"), "theirs").unwrap();
    git(&peer, &["add", "theirs"]);
    git(&peer, &["commit", "-m", "theirs"]);
    git(&peer, &["push", "origin", "main"]);
    let push = f
        .service
        .plan_push("origin", "main", &f.service.refresh().unwrap().revision)
        .unwrap();
    assert_eq!(f.execute(&push).outcome, GitOutcome::Failed);
    let pull = f
        .service
        .plan_pull_ff_only("origin", "main", &f.service.refresh().unwrap().revision)
        .unwrap();
    assert_eq!(f.execute(&pull).outcome, GitOutcome::Failed);
    assert!(!f.root.join("theirs").exists());
}

#[test]
fn remote_credentials_are_redacted_and_endpoint_changes_invalidate_plans() {
    let f = Fixture::new();
    f.initial();
    git(
        &f.root,
        &[
            "remote",
            "add",
            "origin",
            "https://user:SECRET@example.invalid/repo?token=QUERY",
        ],
    );
    let remotes = f.service.remotes().unwrap();
    let serialized = serde_json::to_string(&remotes).unwrap();
    assert!(
        !serialized.contains("SECRET")
            && !serialized.contains("QUERY")
            && !format!("{remotes:?}").contains("SECRET")
    );
    assert!(remotes[0].embedded_credentials);
    assert!(f.service.plan_fetch("origin").is_err());
    git(&f.root, &["remote", "set-url", "origin", "/safe/local"]);
    let plan = f.service.plan_fetch("origin").unwrap();
    git(&f.root, &["remote", "set-url", "origin", "/changed/local"]);
    let mut called = false;
    assert!(f
        .service
        .execute_with(&plan, &f.gate, &f.temp.path().join("resources"), |_| {
            called = true;
            unreachable!()
        })
        .is_err());
    assert!(!called);
    git(
        &f.root,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://user@example.invalid/repo",
        ],
    );
    assert!(!f.service.remotes().unwrap()[0].embedded_credentials);
    assert!(f.service.plan_fetch("origin").is_ok());
}

#[test]
fn linked_worktree_targets_its_own_index_and_detached_head_is_explicit() {
    let f = Fixture::new();
    f.initial();
    let linked = f.temp.path().join("linked worktree");
    git(
        &f.root,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let repository = Repository::discover(&linked).unwrap();
    assert_ne!(repository.git_dir, f.service.repository().git_dir);
    assert_eq!(repository.common_dir, f.service.repository().common_dir);
    let service = GitService::new(
        GitExecutable::discover().unwrap(),
        repository,
        [
            ("HOME".into(), f.temp.path().to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]
        .into(),
    )
    .unwrap();
    fs::write(linked.join("linked-only"), "linked").unwrap();
    f.write("main-only", "main");
    let snapshot = service.refresh().unwrap();
    let plan = service
        .plan_stage(
            vec![GitPath::from_text("linked-only").unwrap()],
            &snapshot.revision,
        )
        .unwrap();
    assert_eq!(
        service
            .run_noninteractive(&plan, &f.gate, &f.temp.path().join("resources"))
            .unwrap()
            .outcome,
        GitOutcome::Succeeded
    );
    assert!(f.service.commit_preview().unwrap().staged.is_empty());
    assert_eq!(service.commit_preview().unwrap().staged.len(), 1);
    git(&linked, &["reset", "--quiet", "HEAD", "--", "linked-only"]);
    git(&linked, &["checkout", "--detach", "HEAD"]);
    assert!(matches!(
        service.refresh().unwrap().revision.head,
        Head::Detached { .. }
    ));
    assert!(service
        .plan_pull_ff_only("origin", "main", &service.refresh().unwrap().revision)
        .is_err());
}

#[test]
fn conflicts_are_explicit_and_commit_review_refuses_them() {
    let f = Fixture::new();
    f.initial();
    git(&f.root, &["checkout", "-b", "other"]);
    f.write("base", "other\n");
    git(&f.root, &["commit", "-am", "other"]);
    git(&f.root, &["checkout", "main"]);
    f.write("base", "main\n");
    git(&f.root, &["commit", "-am", "main"]);
    let output = Command::new("/usr/bin/git")
        .args(["merge", "other"])
        .current_dir(&f.root)
        .env("HOME", f.temp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let snapshot = f.service.refresh().unwrap();
    assert!(snapshot.conflicted());
    assert!(snapshot
        .entries
        .iter()
        .any(|entry| entry.conflict && entry.path.bytes() == b"base"));
    assert!(f.service.commit_preview().is_err());
    f.write("base", "resolved\n");
    assert_eq!(f.stage(&["base"]).outcome, GitOutcome::Succeeded);
    assert!(!f.service.refresh().unwrap().conflicted());
}

#[test]
fn missing_promisor_objects_never_trigger_automatic_fetch() {
    let f = Fixture::new();
    f.initial();
    let helper = f.temp.path().join("transport");
    fs::write(&helper, "#!/bin/sh\nprintf fetched > fetch-ran\nexit 1\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &f.root,
        &[
            "remote",
            "add",
            "origin",
            &format!("ext::{}", helper.display()),
        ],
    );
    git(&f.root, &["config", "remote.origin.promisor", "true"]);
    git(&f.root, &["config", "extensions.partialclone", "origin"]);
    git(&f.root, &["config", "protocol.ext.allow", "always"]);
    let oid = String::from_utf8(git(&f.root, &["rev-parse", "HEAD"])).unwrap();
    let oid = oid.trim();
    fs::remove_file(f.root.join(".git/objects").join(&oid[..2]).join(&oid[2..])).unwrap();
    assert!(f.service.refresh().is_err());
    assert!(!f.root.join("fetch-ran").exists());
}

#[test]
fn submodule_helpers_are_not_run_and_unchecked_worktrees_are_not_clean() {
    let f = Fixture::new();
    f.initial();
    let module = f.temp.path().join("module");
    fs::create_dir(&module).unwrap();
    git(&module, &["init"]);
    git(&module, &["config", "user.name", "Module"]);
    git(&module, &["config", "user.email", "module@example.invalid"]);
    fs::write(module.join("file"), "base").unwrap();
    git(&module, &["add", "file"]);
    git(&module, &["commit", "-m", "module"]);
    git(
        &f.root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            module.to_str().unwrap(),
            "nested",
        ],
    );
    git(&f.root, &["commit", "-am", "submodule"]);
    let helper = f.temp.path().join("nested-helper");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf called > nested-helper-ran\ncat\n",
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let nested = f.root.join("nested");
    git(
        &nested,
        &["config", "filter.secret.clean", helper.to_str().unwrap()],
    );
    fs::write(nested.join(".gitattributes"), "file filter=secret\n").unwrap();
    fs::write(nested.join("file"), "changed").unwrap();
    let snapshot = f.service.refresh().unwrap();
    assert!(snapshot.submodule_worktrees_unchecked);
    assert!(!snapshot.clean());
    f.service.diff(DiffTarget::Worktree, None).unwrap();
    assert!(!nested.join("nested-helper-ran").exists());
}

#[test]
fn execution_error_is_unknown_and_private_resources_are_removed() {
    let f = Fixture::new();
    f.initial();
    f.write("base", "new");
    f.stage(&["base"]);
    let plan = f
        .service
        .plan_commit(&f.service.commit_preview().unwrap(), "PRIVATE DRAFT")
        .unwrap();
    let resources = f.temp.path().join("resources");
    let result = f
        .service
        .execute_with(&plan, &f.gate, &resources, |builder| {
            assert!(!format!("{:?}", builder.get_argv()).contains("PRIVATE DRAFT"));
            let message = resources
                .join(format!("git-operation-{}", plan.id()))
                .join("message");
            assert_eq!(
                fs::metadata(message).unwrap().permissions().mode() & 0o777,
                0o600
            );
            anyhow::bail!("sensitive executor error which must not leak")
        })
        .unwrap();
    assert_eq!(result.outcome, GitOutcome::Unknown);
    assert!(!serde_json::to_string(&result)
        .unwrap()
        .contains("sensitive executor"));
    assert!(!resources
        .join(format!("git-operation-{}", plan.id()))
        .exists());
}

#[test]
fn intent_to_add_is_not_presented_as_committable_content() {
    let f = Fixture::new();
    f.initial();
    f.write("intent", "not yet staged\n");
    git(&f.root, &["add", "--intent-to-add", "intent"]);
    let preview = f.service.commit_preview().unwrap();
    assert!(preview.staged.is_empty());
    assert!(f
        .service
        .plan_commit(&preview, "cannot commit unstaged intent")
        .is_err());
    assert_eq!(f.stage(&["intent"]).outcome, GitOutcome::Succeeded);
    assert_eq!(f.service.commit_preview().unwrap().staged.len(), 1);
}

#[test]
fn effective_rewrites_and_push_endpoints_are_reviewed_separately() {
    let f = Fixture::new();
    f.initial();
    git(&f.root, &["remote", "add", "origin", "fixture:repo"]);
    git(
        &f.root,
        &[
            "config",
            "url.https://TOKEN@example.invalid/.insteadOf",
            "fixture:",
        ],
    );
    let remotes = f.service.remotes().unwrap();
    assert!(remotes[0].embedded_credentials);
    assert!(!serde_json::to_string(&remotes).unwrap().contains("TOKEN"));
    assert!(f.service.plan_fetch("origin").is_err());
    git(&f.root, &["remote", "set-url", "origin", "/safe/local"]);
    git(
        &f.root,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "https://PUSH_SECRET@example.invalid/repo",
        ],
    );
    assert!(f.service.plan_fetch("origin").is_ok());
    assert!(f
        .service
        .plan_push("origin", "main", &f.service.refresh().unwrap().revision)
        .is_err());
    assert!(!serde_json::to_string(&f.service.remotes().unwrap())
        .unwrap()
        .contains("PUSH_SECRET"));
}

#[test]
fn partial_clone_policy_is_explicit_for_git_before_lazy_fetch_control() {
    let f = Fixture::new();
    f.initial();
    assert!(f.service.refresh().unwrap().clean());
    let version = String::from_utf8(git(&f.root, &["--version"])).unwrap();
    let mut parts = version.strip_prefix("git version ").unwrap().split('.');
    let version = (
        parts.next().unwrap().parse::<u32>().unwrap(),
        parts.next().unwrap().parse::<u32>().unwrap(),
    );
    git(&f.root, &["remote", "add", "origin", "/fixture/no-network"]);
    git(&f.root, &["config", "remote.origin.promisor", "true"]);
    if version < (2, 46) {
        let error = f.service.refresh().unwrap_err();
        assert!(error.to_string().contains("cannot suppress lazy fetch"));
        git(&f.root, &["config", "remote.origin.promisor", "false"]);
        assert!(f.service.refresh().unwrap().clean());
    } else {
        assert!(f.service.refresh().unwrap().clean());
    }
}

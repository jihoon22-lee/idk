use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};

use idk_workspace::git::{GitExecutable, Repository};
use idk_workspace::model::SourceGate;

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    git: GitExecutable,
}

// On a fixture failure, still release the peer and main test thread before
// propagating the panic through join(); a failing race test must not hang.
struct FinishBarrier(Arc<Barrier>);
impl Drop for FinishBarrier {
    fn drop(&mut self) {
        self.0.wait();
    }
}

impl Fixture {
    fn new(name: &str, commit: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let fixture = Self {
            _directory: directory,
            root,
            git: GitExecutable::discover().unwrap(),
        };
        fixture.run(["init", "-q"]);
        fixture.run(["symbolic-ref", "HEAD", "refs/heads/main"]);
        if commit {
            std::fs::write(fixture.root.join("source.txt"), "fixture\n").unwrap();
            fixture.run(["add", "--", "source.txt"]);
            fixture.run(["commit", "-q", "-m", "fixture"]);
        }
        fixture
    }

    fn run<I, S>(&self, args: I) -> Vec<u8>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        fixture_git(&self.git, &self.root, args)
    }
}

fn fixture_git<I, S>(git: &GitExecutable, cwd: &Path, args: I) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(git.path());
    command.current_dir(cwd).args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
    ]);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let output = command.args(args).output().unwrap();
    assert!(
        output.status.success(),
        "fixture Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn unborn_detached_and_special_paths_preserve_binding() {
    let fixture = Fixture::new("한글 repo ' ; $(never-exec)\nlast\n", false);
    let repository = Repository::discover(&fixture.root).unwrap();
    assert_eq!(repository.root, fixture.root.canonicalize().unwrap());
    assert_eq!(
        repository.identity(),
        fixture.root.join(".git").canonicalize().unwrap()
    );
    repository.verify().unwrap();
    let serialized = serde_json::to_vec(&repository).unwrap();
    assert_eq!(
        serde_json::from_slice::<Repository>(&serialized).unwrap(),
        repository
    );

    std::fs::write(fixture.root.join("한글 파일.txt"), "source\n").unwrap();
    fixture.run(["add", "--", "한글 파일.txt"]);
    fixture.run(["commit", "-q", "-m", "initial"]);
    fixture.run(["checkout", "-q", "--detach", "HEAD"]);
    assert_eq!(Repository::discover(&fixture.root).unwrap(), repository);
    repository.verify().unwrap();
    assert!(!fixture.root.join("never-exec").exists());
}

#[test]
fn linked_worktrees_share_common_directory_but_not_gate_identity() {
    let fixture = Fixture::new("source", true);
    let linked = fixture._directory.path().join("외부 worktree");
    fixture.run([
        OsString::from("worktree"),
        OsString::from("add"),
        OsString::from("-q"),
        OsString::from("-b"),
        OsString::from("linked"),
        linked.as_os_str().to_owned(),
    ]);
    let main = Repository::discover(&fixture.root).unwrap();
    let other = Repository::discover(&linked).unwrap();
    assert_ne!(main.identity(), other.identity());
    assert_eq!(main.common_dir, other.common_dir);
    assert_eq!(other.root, linked.canonicalize().unwrap());
    assert!(other.git_dir.starts_with(main.common_dir.join("worktrees")));
    main.verify().unwrap();
    other.verify().unwrap();

    let gate = SourceGate::default();
    assert!(gate
        .reserve_run(main.identity(), "run-unknown", "build")
        .is_err());
    assert!(gate
        .reserve_mutation(main.identity(), "mutate-unknown")
        .is_err());
    gate.ready(main.identity()).unwrap();
    gate.ready(other.identity()).unwrap();
    let run = gate.reserve_run(main.identity(), "run", "build").unwrap();
    assert!(gate.reserve_mutation(main.identity(), "same-tree").is_err());
    let other_change = gate
        .reserve_mutation(other.identity(), "other-tree")
        .unwrap();
    other.verify().unwrap();
    // An actual Git source mutation consumes the different worktree lease.
    fixture_git(
        &fixture.git,
        &other.root,
        ["checkout", "-q", "-b", "gated-linked"],
    );
    assert!(fixture
        .run(["symbolic-ref", "--short", "HEAD"])
        .starts_with(b"main"));
    drop(other_change);
    drop(run);
    assert!(gate.state(main.identity()).unwrap().runs.is_empty());
    assert!(gate.state(other.identity()).unwrap().mutation.is_none());
}

#[test]
fn source_start_and_actual_git_change_cannot_both_reserve_same_worktree() {
    let fixture = Fixture::new("race", true);
    let repository = Repository::discover(&fixture.root).unwrap();
    let gate = SourceGate::default();
    gate.ready(repository.identity()).unwrap();
    let start = Arc::new(Barrier::new(3));
    let finished = Arc::new(Barrier::new(3));
    let run_thread = {
        let gate = gate.clone();
        let repository = repository.clone();
        let start = start.clone();
        let finished = finished.clone();
        std::thread::spawn(move || {
            start.wait();
            let lease = gate
                .reserve_run(repository.identity(), "run-race", "build")
                .ok();
            let _finished = FinishBarrier(finished);
            lease.is_some()
        })
    };
    let mutation_thread = {
        let gate = gate.clone();
        let repository = repository.clone();
        let git = fixture.git.clone();
        let start = start.clone();
        let finished = finished.clone();
        std::thread::spawn(move || {
            start.wait();
            let lease = gate
                .reserve_mutation(repository.identity(), "git-race")
                .ok();
            let _finished = FinishBarrier(finished);
            let won = lease.is_some();
            if won {
                git.verify_repository(&repository).unwrap();
                fixture_git(
                    &git,
                    &repository.root,
                    ["checkout", "-q", "-b", "gated-change"],
                );
            }
            won
        })
    };
    start.wait();
    finished.wait();
    let run_won = run_thread.join().unwrap();
    let mutation_won = mutation_thread.join().unwrap();
    assert_ne!(run_won, mutation_won);
    let branch = fixture.run(["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(
        branch,
        if mutation_won {
            b"gated-change\n".to_vec()
        } else {
            b"main\n".to_vec()
        }
    );
}

#[test]
fn moved_or_retargeted_git_binding_is_rejected() {
    let first = Fixture::new("first", true);
    let second = Fixture::new("second", true);
    let repository = Repository::discover(&first.root).unwrap();
    let target = Repository::discover(&second.root).unwrap();
    let preserved_git = first._directory.path().join("original-git");
    std::fs::rename(first.root.join(".git"), &preserved_git).unwrap();
    std::fs::write(
        first.root.join(".git"),
        format!("gitdir: {}\n", target.git_dir.display()),
    )
    .unwrap();
    assert!(repository.verify().is_err());
    let retargeted = Repository::discover(&first.root).unwrap();
    assert_eq!(retargeted.git_dir, target.git_dir);
    assert_ne!(retargeted, repository);

    let moved = second._directory.path().join("moved");
    std::fs::rename(&second.root, &moved).unwrap();
    assert!(target.verify().is_err());
    assert_eq!(
        Repository::discover(&moved).unwrap().root,
        moved.canonicalize().unwrap()
    );
}

#[test]
fn bare_and_non_repository_directories_are_not_project_worktrees() {
    let directory = tempfile::tempdir().unwrap();
    assert!(Repository::discover(directory.path()).is_err());
    let git = GitExecutable::discover().unwrap();
    fixture_git(&git, directory.path(), ["init", "--bare", "-q"]);
    assert!(Repository::discover(directory.path()).is_err());
}

#[test]
fn repository_ignores_inherited_binding_overrides() {
    if let Some(target) = std::env::var_os("IDK_GIT_ENV_TEST_TARGET") {
        let expected = PathBuf::from(target).canonicalize().unwrap();
        let repository = Repository::discover(&expected).unwrap();
        assert_eq!(repository.root, expected);
        assert_eq!(repository.git_dir, expected.join(".git"));
        return;
    }
    let target = Fixture::new("target", true);
    let poison = Fixture::new("poison", true);
    let trace = poison.root.join("unwanted-trace");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "repository_ignores_inherited_binding_overrides",
            "--nocapture",
        ])
        .env("IDK_GIT_ENV_TEST_TARGET", &target.root)
        .env("GIT_DIR", poison.root.join(".git"))
        .env("GIT_COMMON_DIR", poison.root.join(".git"))
        .env("GIT_WORK_TREE", &poison.root)
        .env("GIT_INDEX_FILE", poison.root.join("alternative-index"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.worktree")
        .env("GIT_CONFIG_VALUE_0", &poison.root)
        .env("GIT_CONFIG_PARAMETERS", "invalid injected parameters")
        .env("GIT_TRACE", &trace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child test failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!trace.exists());
    assert!(!poison.root.join("alternative-index").exists());
}

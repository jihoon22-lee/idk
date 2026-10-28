#[path = "common/launcher.rs"]
mod launcher;
use idk_workspace::store::Store;
use std::path::Path;
use std::process::Command;

fn invoke(data: &Path, home: &Path, args: &[&str]) {
    let output = Command::new(launcher::path())
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env("HOME", home)
        .env_remove("IDK_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn cli_native_repeat_key_patch_and_explicit_clear_preserve_intended_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let data = temp.path().join("data");
    invoke(
        &data,
        temp.path(),
        &[
            "project",
            "connect",
            "partial",
            source.to_str().unwrap(),
            "--shell",
            "/bin/sh",
            "--shell-arg=-i",
            "--shell-env",
            "A=a",
            "--shell-env",
            "B=b",
        ],
    );
    let store = Store::open(Some(&data)).unwrap();
    let shell = || {
        store.load().unwrap().projects[0]
            .shell
            .native
            .clone()
            .unwrap()
    };
    invoke(
        &data,
        temp.path(),
        &["project", "environment", "partial", "--native"],
    );
    assert_eq!(shell().args, ["-i"]);
    assert_eq!(shell().env.len(), 2);
    invoke(
        &data,
        temp.path(),
        &[
            "project",
            "environment",
            "partial",
            "--shell-env-set",
            "A=updated",
        ],
    );
    assert_eq!(shell().env["A"], "updated");
    assert_eq!(shell().env["B"], "b");
    invoke(
        &data,
        temp.path(),
        &[
            "project",
            "environment",
            "partial",
            "--shell-env-unset",
            "A",
        ],
    );
    assert!(!shell().env.contains_key("A"));
    assert_eq!(shell().env["B"], "b");
    invoke(
        &data,
        temp.path(),
        &["project", "environment", "partial", "--shell-env", "C=c"],
    );
    assert_eq!(shell().env.len(), 1);
    assert_eq!(shell().env["C"], "c");
    invoke(
        &data,
        temp.path(),
        &[
            "project",
            "environment",
            "partial",
            "--clear-shell-env",
            "--shell-env-set",
            "D=d",
            "--clear-shell-args",
        ],
    );
    assert_eq!(shell().env.len(), 1);
    assert_eq!(shell().env["D"], "d");
    assert!(shell().args.is_empty());
    assert!(
        !store.socket_path().exists(),
        "definition edits must not start a host"
    );
}

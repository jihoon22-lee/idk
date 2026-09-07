#[path = "common/launcher.rs"]
mod launcher;

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn invoke(root: &Path, home: &Path, args: &[&str]) -> Value {
    let output = Command::new(launcher::path())
        .args(["--data-dir", root.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "idk {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn connect_and_manage_six_paths_without_running_scripts_or_changing_legacy_data() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let home = temp.path().join("home");
    let source = temp.path().join("source 한글");
    let external = temp.path().join("external");
    for path in [&home, &source, &external] {
        std::fs::create_dir(path).unwrap();
    }
    let legacy = home.join(".config/idk");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("ws.toml"), "legacy = 'preserved'\n").unwrap();
    let script = source.join("setup.csh");
    std::fs::write(&script, "echo MUST_NOT_RUN > unexpected-output\n").unwrap();
    let shell = std::env::var("IDK_TEST_SHELL").unwrap_or_else(|_| "/usr/bin/tcsh".into());
    let project = invoke(
        &data,
        &home,
        &[
            "project",
            "connect",
            "한글 project",
            source.to_str().unwrap(),
            "--shell",
            &shell,
            "--source",
            script.to_str().unwrap(),
        ],
    );
    let project_id = project["id"].as_str().unwrap();
    invoke(
        &data,
        &home,
        &[
            "terminal",
            "add",
            project_id,
            "dev2",
            source.to_str().unwrap(),
        ],
    );
    for number in 1..=3 {
        let path = external.join(format!("test{number}"));
        std::fs::create_dir(&path).unwrap();
        invoke(
            &data,
            &home,
            &[
                "terminal",
                "add",
                project_id,
                &format!("test{number}"),
                path.to_str().unwrap(),
            ],
        );
    }
    let terminals = invoke(&data, &home, &["terminal", "list", project_id]);
    assert_eq!(terminals.as_array().unwrap().len(), 5);
    let missing = external.join("not-mounted");
    let sixth = invoke(
        &data,
        &home,
        &[
            "terminal",
            "add",
            project_id,
            "later",
            missing.to_str().unwrap(),
        ],
    );
    let sixth_id = sixth["id"].as_str().unwrap();
    let terminals = invoke(&data, &home, &["terminal", "list", project_id]);
    assert_eq!(terminals.as_array().unwrap().len(), 6);
    assert_eq!(terminals[5]["path"]["status"], "missing");
    let ids: std::collections::HashSet<_> = terminals
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["definition"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 6);
    let trust = invoke(&data, &home, &["project", "trust", project_id, "--yes"]);
    assert!(
        trust.get("environment").is_none(),
        "launch environment must not be serialized"
    );
    assert!(!source.join("unexpected-output").exists());
    assert!(!source.join(".git").exists());
    assert!(!missing.exists());
    invoke(
        &data,
        &home,
        &["terminal", "remove", project_id, sixth_id, "--yes"],
    );
    invoke(&data, &home, &["project", "remove", project_id, "--yes"]);
    assert_eq!(
        std::fs::read_to_string(legacy.join("ws.toml")).unwrap(),
        "legacy = 'preserved'\n"
    );
    assert!(source.is_dir() && script.is_file() && external.is_dir());
}

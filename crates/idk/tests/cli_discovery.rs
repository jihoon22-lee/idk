use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
fn call(data: &Path, home: &Path, bin: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_idk"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env("HOME", home)
        .env("PATH", bin)
        .output()
        .unwrap()
}
fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
fn executable(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
#[test]
fn discovery_lists_and_registers_only_selected_unapproved_tasks_without_execution() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    let data = temp.path().join("data");
    for path in [&source, &home, &bin] {
        fs::create_dir(path).unwrap();
    }
    fs::write(
        source.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .unwrap();
    executable(
        &bin.join("cargo"),
        &format!("echo ran > '{}'", source.join("executed").display()),
    );
    success(call(
        &data,
        &home,
        &bin,
        &[
            "project",
            "connect",
            "demo",
            source.to_str().unwrap(),
            "--shell",
            "/bin/sh",
        ],
    ));
    let readable = success(call(&data, &home, &bin, &["discover", "demo"]));
    assert!(String::from_utf8_lossy(&readable.stdout).contains("1. cargo build [available]"));
    let json: Value = serde_json::from_slice(
        &success(call(
            &data,
            &home,
            &bin,
            &["discover", "demo", "--json", "--register", "cargo test"],
        ))
        .stdout,
    )
    .unwrap();
    assert_eq!(json["registered"]["name"], "cargo test");
    assert!(json["registered"]["approved_digest"].is_null());
    assert_eq!(json["executed"], false);
    assert!(!source.join("executed").exists());
    assert!(!source.join("idk.toml").exists());
    assert!(!call(
        &data,
        &home,
        &bin,
        &["discover", "demo", "--register", "cargo test"]
    )
    .status
    .success());
    let tasks: Value = serde_json::from_slice(
        &success(call(&data, &home, &bin, &["task", "list", "demo"])).stdout,
    )
    .unwrap();
    assert_eq!(tasks.as_array().unwrap().len(), 1);
    assert!(!data.join("state/runs.json").exists());
}
#[test]
fn optional_tool_is_inspected_only_when_selected_and_registration_does_not_run_analysis() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    let data = temp.path().join("data");
    for path in [&source, &home, &bin] {
        fs::create_dir(path).unwrap();
    }
    let tool = bin.join("ici");
    let marker = source.join("version-was-run");
    executable(&tool, &format!("if [ \"$1\" = --version ]; then echo selected > '{}'; echo ici 0.13.0; else exit 90; fi", marker.display()));
    success(call(
        &data,
        &home,
        &bin,
        &[
            "project",
            "connect",
            "demo",
            source.to_str().unwrap(),
            "--shell",
            "/bin/sh",
        ],
    ));
    success(call(&data, &home, &bin, &["discover", "demo"]));
    assert!(!marker.exists());
    let output = success(call(
        &data,
        &home,
        &bin,
        &[
            "discover",
            "demo",
            "--tool",
            "ici",
            "--executable",
            tool.to_str().unwrap(),
            "--register",
            "static analysis",
            "--json",
        ],
    ));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["inspection"]["version"], "0.13.0");
    assert_eq!(value["registered"], true);
    assert_eq!(value["executed"], false);
    assert!(value["draft"]["approved_digest"].is_null());
    assert!(marker.exists());
    executable(&tool, "echo ici 1.0.0");
    let rejected = call(
        &data,
        &home,
        &bin,
        &[
            "discover",
            "demo",
            "--tool",
            "ici",
            "--executable",
            tool.to_str().unwrap(),
            "--register",
            "unsupported",
            "--json",
        ],
    );
    assert!(!rejected.status.success());
    let value: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(value["inspection"]["supported"], false);
}

#[test]
fn artifact_cli_measures_registered_bytes_without_mutating_definitions() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    let data = temp.path().join("data");
    for path in [&source, &home, &bin] {
        fs::create_dir(path).unwrap();
    }
    let artifact = source.join("binary");
    fs::write(&artifact, b"abc").unwrap();
    success(call(
        &data,
        &home,
        &bin,
        &[
            "project",
            "connect",
            "demo",
            source.to_str().unwrap(),
            "--shell",
            "/bin/sh",
        ],
    ));
    success(call(
        &data,
        &home,
        &bin,
        &[
            "task",
            "add",
            "demo",
            "build",
            "--program",
            "/bin/true",
            "--artifact",
            artifact.to_str().unwrap(),
        ],
    ));
    let store = idk_workspace::store::Store::open(Some(&data)).unwrap();
    let before = fs::read(store.config_path()).unwrap();
    let output = success(call(
        &data,
        &home,
        &bin,
        &["discover", "demo", "--artifact", "build", "--json"],
    ));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["measurement"]["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(value["run_relation_modified"], false);
    assert_eq!(fs::read(store.config_path()).unwrap(), before);
    assert!(!call(
        &data,
        &home,
        &bin,
        &[
            "discover",
            "demo",
            "--artifact",
            "build",
            "--max-bytes",
            "2"
        ]
    )
    .status
    .success());
    assert_eq!(fs::read(store.config_path()).unwrap(), before);
}

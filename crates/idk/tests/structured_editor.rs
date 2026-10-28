#[path = "common/run_fixture.rs"]
mod fixture;
use idk_workspace::{
    editor::EditorService,
    model::{EditorConfig, Project, ShellConfig, Workspace},
    result_store::ResultStore,
    store::Store,
};
use std::{fs, os::unix::fs::PermissionsExt};
#[test]
fn structured_editor_binds_immutable_finding_and_rechecks_body_without_raw_offsets() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let file = source.join("a.cpp");
    fs::write(&file, "source").unwrap();
    let editor = temp.path().join("editor");
    fs::write(&editor, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::open(Some(&temp.path().join("data"))).unwrap();
    let mut run = fixture::run(&source);
    let project = Project {
        id: run.project_id.clone(),
        name: "fixture".into(),
        root: source.clone(),
        repository: None,
        repository_binding: None,
        related_repositories: vec![],
        default_terminal: None,
        shell: ShellConfig {
            native: None,
            executable: "/bin/sh".into(),
            login: false,
            init_cwd: source.clone(),
            sources: vec![],
            trusted_digest: None,
        },
        terminals: vec![],
        tasks: vec![],
        editor: Some(EditorConfig {
            executable: editor,
            args: vec!["--".into(), "{file}".into()],
            external_gui: false,
        }),
    };
    let mut workspace = Workspace {
        projects: vec![project],
        ..Workspace::default()
    };
    store.save(&mut workspace, 0).unwrap();
    let results = ResultStore::open(&store.state_dir).unwrap();
    let payload = serde_json::json!({"schema_id":"lens.result","schema_version":1,"producer":{"name":"lens","version":"0.10.0"},"run_id":run.run_id,
        "complete":false,"execution":{},"source":{},"findings":[{"severity":"warn","message":"finding","evidence":{"path":"a.cpp"}}]});
    run.structured_result =
        Some(results.capture_bytes(&run.run_id, &serde_json::to_vec(&payload).unwrap()));
    let finding = results
        .findings(run.structured_result.as_ref().unwrap(), 0, 1)
        .unwrap()
        .findings
        .remove(0);
    let service = EditorService { store: &store };
    let plan = service.review_finding(&run, &finding).unwrap();
    assert!(plan.review().problem_id.is_empty());
    assert_eq!(
        plan.review().finding.as_ref().unwrap().body_sha256,
        finding.body_sha256
    );
    assert_eq!(plan.review().file, file);
    let mut forged = finding.clone();
    forged.body_sha256 = "0".repeat(64);
    assert!(service.review_finding(&run, &forged).is_err());
    forged = finding.clone();
    forged.location.as_mut().unwrap().path = "other.cpp".into();
    assert!(service.review_finding(&run, &forged).is_err());
    for primary in [
        serde_json::Value::Null,
        serde_json::json!({"path":"a.cpp", "start_line":u64::MAX}),
    ] {
        let mut invalid_payload = payload.clone();
        invalid_payload["findings"][0]
            .as_object_mut()
            .unwrap()
            .remove("evidence");
        if !primary.is_null() {
            invalid_payload["findings"][0]["primary_location"] = primary;
        }
        let mut invalid_run = run.clone();
        invalid_run.structured_result = Some(
            results.capture_bytes(&run.run_id, &serde_json::to_vec(&invalid_payload).unwrap()),
        );
        let invalid_finding = results
            .findings(invalid_run.structured_result.as_ref().unwrap(), 0, 1)
            .unwrap()
            .findings
            .remove(0);
        assert!(service
            .review_finding(&invalid_run, &invalid_finding)
            .is_err());
    }
    let body = run.structured_result.as_ref().unwrap().body().unwrap();
    results.remove(body, false).unwrap();
    assert!(service
        .command(&plan, std::collections::BTreeMap::new())
        .is_err());
    assert!(service.review_finding(&run, &finding).is_err());
}

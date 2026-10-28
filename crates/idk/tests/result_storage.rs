#[path = "common/run_fixture.rs"]
mod run_fixture;
use base64::Engine;
use idk_workspace::{
    model::{new_id, SourceGate},
    result_store::{ComparisonState, EventState, ResultStore, MAX_EVENT_BYTES, TOTAL_RESULT_BYTES},
    run::RunRegistry,
    run_wire::{RunState, StepResult},
    store::Store,
    structured_result::{StructuredResult, MAX_RESULT_BYTES},
};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn lens() -> Value {
    serde_json::from_str(include_str!("fixtures/lens-result-100-cases.json")).unwrap()
}
fn path(state: &Path, result: &StructuredResult) -> std::path::PathBuf {
    let body = result.body().unwrap();
    state
        .join("results")
        .join(format!("{}.{}.json", body.run_id, body.sha256))
}
fn run(cwd: &Path) -> idk_workspace::run_wire::RunInfo {
    let mut run = run_fixture::run(cwd);
    run.run_id = new_id();
    run.operation_id = new_id();
    run.log.run_id = run.run_id.clone();
    run.finished_at_ms = Some(now());
    run.started_at_ms = now();
    run.steps = vec![StepResult {
        index: 0,
        name: "fixture".into(),
        exit_code: Some(2),
    }];
    run
}
#[test]
fn exact_25543_byte_lens_and_megabyte_body_have_small_summaries_and_lossless_pages() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let fixture = include_bytes!("fixtures/lens-result-100-cases.json");
    assert_eq!(fixture.len(), 25_543);
    let small = store.capture_bytes(&new_id(), fixture);
    assert_eq!(small.summary().unwrap().findings_count, 100);
    let mut document = lens();
    document["extension"] = json!("x".repeat(1024 * 1024 + 123));
    let original = serde_json::to_vec(&document).unwrap();
    assert!(original.len() > 1024 * 1024);
    let stored = store.capture_bytes(&new_id(), &original);
    assert!(matches!(stored, StructuredResult::Stored { .. }));
    assert!(serde_json::to_vec_pretty(&stored).unwrap().len() < 4096);
    let mut bytes = vec![];
    let mut offset = 0;
    loop {
        let page = store.raw(&stored, offset, 65536).unwrap();
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode(page.data_base64)
                .unwrap(),
        );
        offset = page.next_offset;
        if page.eof {
            break;
        }
    }
    assert_eq!(bytes, original);
    let first = store.findings(&stored, 0, 75).unwrap();
    assert!(serde_json::to_vec(&first).unwrap().len() < 65536);
    assert_eq!(
        first.findings[0].location.as_ref().unwrap().path,
        "tests/example.xml"
    );
    assert_eq!(
        first.findings[0].location.as_ref().unwrap().start_line,
        None
    );
    assert!(first.more);
    assert_eq!(
        store
            .findings(&stored, first.next_offset, 100)
            .unwrap()
            .next_offset,
        100
    );
    assert!(store.raw(&stored, 0, 65537).is_err());
    assert!(store.findings(&stored, 0, 101).is_err());
    let export = dir.path().join("export.json");
    store.export(&stored, &export).unwrap();
    assert_eq!(fs::read(&export).unwrap(), original);
    assert!(store.export(&stored, &export).is_err());
    assert_eq!(
        fs::metadata(path(dir.path(), &stored))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o400
    );
}
#[test]
fn immutable_snapshot_and_explicit_invalid_unsupported_oversize_expired_tampered_states() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let input = dir.path().join("producer.json");
    assert!(matches!(
        store.capture(&new_id(), &input),
        StructuredResult::Missing
    ));
    fs::write(&input, b"{").unwrap();
    let invalid = store.capture(&new_id(), &input);
    assert!(matches!(
        invalid,
        StructuredResult::Invalid { body: Some(_), .. }
    ));
    fs::write(
        &input,
        br#"{"schema_id":"lens.result","schema_version":77}"#,
    )
    .unwrap();
    assert!(matches!(
        store.capture(&new_id(), &input),
        StructuredResult::Unsupported { .. }
    ));
    fs::File::create(&input)
        .unwrap()
        .set_len(MAX_RESULT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        store.capture(&new_id(), &input),
        StructuredResult::Oversize { .. }
    ));
    fs::write(&input, serde_json::to_vec(&lens()).unwrap()).unwrap();
    let stored = store.capture(&new_id(), &input);
    fs::write(&input, b"changed producer output").unwrap();
    assert!(matches!(
        store.verify(&stored),
        StructuredResult::Stored { .. }
    ));
    let managed = path(dir.path(), &stored);
    fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&managed, b"tampered").unwrap();
    assert!(matches!(
        store.raw(&stored, 0, 10).unwrap().result,
        StructuredResult::Tampered { .. }
    ));
    fs::remove_file(&managed).unwrap();
    assert!(matches!(
        store.verify(&stored),
        StructuredResult::Expired { .. }
    ));
    std::os::unix::fs::symlink(&input, &managed).unwrap();
    assert!(matches!(
        store.verify(&stored),
        StructuredResult::Tampered { .. }
    ));
    let link = dir.path().join("producer-link");
    std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
    assert!(matches!(
        store.capture(&new_id(), &link),
        StructuredResult::Invalid { .. }
    ));
}
#[test]
fn storage_cap_refuses_new_body_without_overwriting_existing_or_claiming_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let first = store.capture_bytes(&new_id(), &serde_json::to_vec(&lens()).unwrap());
    let used = store.bytes_used().unwrap();
    let filler = dir.path().join("results/owned-budget-fixture");
    fs::File::create(&filler)
        .unwrap()
        .set_len(TOTAL_RESULT_BYTES - used)
        .unwrap();
    let next = store.capture_bytes(&new_id(), &serde_json::to_vec(&lens()).unwrap());
    assert!(matches!(next, StructuredResult::Limited { .. }));
    assert!(matches!(
        store.verify(&first),
        StructuredResult::Stored { .. }
    ));
    assert_eq!(store.bytes_used().unwrap(), TOTAL_RESULT_BYTES);
}
#[test]
fn legacy_inline_ledger_is_backed_up_and_migrated_without_inline_ipc() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&dir.path().join("data"))).unwrap();
    let input = dir.path().join("legacy.json");
    fs::write(&input, serde_json::to_vec(&lens()).unwrap()).unwrap();
    let mut old = run(dir.path());
    old.structured_result = Some(idk_workspace::structured_result::inspect(
        &input,
        &old.run_id,
    ));
    store
        .write_state("runs.json", &json!({"schema":1,"runs":[old]}))
        .unwrap();
    let original = fs::read(store.state_dir.join("runs.json")).unwrap();
    let registry = RunRegistry::open(store.clone(), SourceGate::default()).unwrap();
    assert_eq!(
        fs::read(store.state_dir.join("runs.previous-v1.json")).unwrap(),
        original
    );
    let ledger: Value =
        serde_json::from_slice(&fs::read(store.state_dir.join("runs.json")).unwrap()).unwrap();
    assert_eq!(ledger["schema"], 2);
    assert!(ledger["runs"][0]["structured_result"]
        .get("document")
        .is_none());
    let history = registry.list(None);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].exit_code, Some(2));
    assert!(matches!(
        history[0].structured_result,
        Some(StructuredResult::Stored { .. })
    ));
    let page = registry.findings(&history[0].run_id, 0, 1).unwrap();
    assert_eq!(page.findings.len(), 1);
}
#[test]
fn age_retention_preserves_pin_and_unknown_cleanup_and_pin_capacity_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&dir.path().join("data"))).unwrap();
    let mut expired = run(dir.path());
    expired.finished_at_ms = Some(1);
    let mut pinned = run(dir.path());
    pinned.finished_at_ms = Some(1);
    pinned.pinned = true;
    let mut unknown = run(dir.path());
    unknown.state = RunState::Unknown;
    unknown.cleanup_confirmed = false;
    unknown.finished_at_ms = Some(1);
    store
        .write_state(
            "runs.json",
            &json!({"schema":2,"runs":[expired,pinned.clone(),unknown.clone()]}),
        )
        .unwrap();
    let registry = RunRegistry::open(store.clone(), SourceGate::default()).unwrap();
    assert_eq!(registry.list(None).len(), 2);
    assert!(registry.info(&pinned.run_id).is_ok());
    assert!(registry.info(&unknown.run_id).is_ok());
    drop(registry);
    let records: Vec<_> = (0..17).map(|_| run(dir.path())).collect();
    store
        .write_state("runs.json", &json!({"schema":2,"runs":records}))
        .unwrap();
    let mut registry = RunRegistry::open(store, SourceGate::default()).unwrap();
    let records = registry.list(None);
    for record in &records[..16] {
        registry.pin_result(&record.run_id, true).unwrap();
    }
    assert!(registry.pin_result(&records[16].run_id, true).is_err());
    registry.pin_result(&records[0].run_id, false).unwrap();
    registry.pin_result(&records[16].run_id, true).unwrap();
    assert_eq!(
        registry.list(None).iter().filter(|run| run.pinned).count(),
        16
    );
}
#[test]
fn unpin_rotates_only_ordinary_history_and_preserves_the_selected_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&dir.path().join("data"))).unwrap();
    let mut baseline = run(dir.path());
    baseline.pinned = true;
    let mut records = vec![baseline.clone()];
    records.extend((0..64).map(|_| run(dir.path())));
    let first_ordinary = records[1].run_id.clone();
    store
        .write_state("runs.json", &json!({"schema":2,"runs":records}))
        .unwrap();
    let mut registry = RunRegistry::open(store, SourceGate::default()).unwrap();
    assert_eq!(registry.list(None).len(), 65);
    registry.pin_result(&baseline.run_id, false).unwrap();
    assert_eq!(registry.list(None).len(), 64);
    assert!(registry.info(&first_ordinary).is_err());
    assert!(!registry.info(&baseline.run_id).unwrap().pinned);
}
fn ici(complete: bool, has_finding: bool) -> Value {
    let mut v: Value = serde_json::from_str(include_str!("fixtures/ici-result-v2.json")).unwrap();
    v["identity"]["fingerprint_version"] = json!("ici.next.fingerprint.v3");
    v["execution"]["required_complete"] = json!(complete);
    v["execution"]["cancelled"] = json!(false);
    v["checks"] = json!([{"task_id":"analyze-tool-a","check_id":"lint","component_id":"tool-a","language":"python","scope_digest":"sha256:scope","state":"SUCCEEDED","evidence_complete":complete,"analysis_unit_id":null,"variant":null}]);
    if !has_finding {
        v["findings"] = json!([]);
    }
    v
}
#[test]
fn ici_comparison_uses_fingerprints_and_completed_check_scope_not_empty_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let old = store.capture_bytes(&new_id(), &serde_json::to_vec(&ici(true, true)).unwrap());
    let new = store.capture_bytes(&new_id(), &serde_json::to_vec(&ici(true, false)).unwrap());
    let comparison = store.compare("old", &old, "new", &new);
    assert!(matches!(comparison.state, ComparisonState::Comparable));
    assert_eq!(comparison.resolved_count, 1);
    let incomplete =
        store.capture_bytes(&new_id(), &serde_json::to_vec(&ici(false, false)).unwrap());
    let comparison = store.compare("old", &old, "new", &incomplete);
    assert_eq!(comparison.resolved_count, 0);
    assert_eq!(comparison.carried_count, 1);
    assert!(matches!(comparison.state, ComparisonState::Partial));
    let mut changed = ici(true, false);
    changed["identity"]["toolchain_digest"] = json!("other toolchain");
    let changed = store.capture_bytes(&new_id(), &serde_json::to_vec(&changed).unwrap());
    assert!(matches!(
        store.compare("old", &old, "new", &changed).state,
        ComparisonState::Incompatible
    ));
    let mut unknown = ici(true, false);
    unknown["checks"] = Value::Null;
    let unknown = store.capture_bytes(&new_id(), &serde_json::to_vec(&unknown).unwrap());
    assert!(matches!(
        store.compare("old", &old, "new", &unknown).state,
        ComparisonState::Incompatible
    ));
}
#[test]
fn lens_missing_location_is_not_invented_and_incomplete_comparison_carries_findings() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let mut old = lens();
    old["run_id"] = Value::Null;
    let old = store.capture_bytes(&new_id(), &serde_json::to_vec(&old).unwrap());
    let mut new = lens();
    new["findings"] = json!([]);
    new["complete"] = json!(false);
    let new = store.capture_bytes(&new_id(), &serde_json::to_vec(&new).unwrap());
    let comparison = store.compare("old", &old, "new", &new);
    assert_eq!(comparison.resolved_count, 0);
    assert_eq!(comparison.carried_count, 100);
    let mut unknown = lens();
    unknown["findings"] = json!([{"severity":"warn","code":"unknown","message":"src/fake.c:12: error: text is not location"}]);
    let unknown = store.capture_bytes(&new_id(), &serde_json::to_vec(&unknown).unwrap());
    assert!(store.findings(&unknown, 0, 1).unwrap().findings[0]
        .location
        .is_none());
}
#[test]
fn events_are_bounded_partial_progress_and_never_a_run_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let path = dir.path().join("events.jsonl");
    let event = |seq| {
        json!({"schema_id":"ici.next.event","schema_version":1,"run_id":"producer","seq":seq,"event_type":"task.progress","task_id":"lint","timestamp":"2026-10-28T00:00:00Z","message":"working"}).to_string()+"\n"
    };
    fs::write(
        &path,
        format!("{}{}{}{{\"cut\":", event(0), event(2), event(2)),
    )
    .unwrap();
    let progress = store.events(&new_id(), &path, Some("producer"), true);
    assert!(matches!(progress.state, EventState::Partial));
    assert_eq!(progress.events_observed, 2);
    assert_eq!(progress.invalid_events, 1);
    assert_eq!(progress.sequence_gaps, 1);
    assert!(progress.truncated_tail);
    assert!(progress.body.is_some());
    assert_eq!(
        store
            .events(&new_id(), &path, Some("wrong"), false)
            .events_observed,
        0
    );
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_EVENT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        store.events(&new_id(), &path, None, false).state,
        EventState::Oversize
    ));
}

#[test]
fn lens_complete_evidence_compares_and_scope_or_unknown_identity_never_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let old = store.capture_bytes(&new_id(), &serde_json::to_vec(&lens()).unwrap());
    let mut current = lens();
    current["findings"].as_array_mut().unwrap().remove(0);
    let new = store.capture_bytes(&new_id(), &serde_json::to_vec(&current).unwrap());
    let compared = store.compare("baseline", &old, "new", &new);
    assert!(matches!(compared.state, ComparisonState::Comparable));
    assert_eq!((compared.resolved_count, compared.unchanged_count), (1, 99));
    current["source"]["identity"] = json!(["lens", "test", "parse", "different.xml"]);
    let changed = store.capture_bytes(&new_id(), &serde_json::to_vec(&current).unwrap());
    let compared = store.compare("baseline", &old, "new", &changed);
    assert!(matches!(compared.state, ComparisonState::Incompatible));
    assert_eq!(compared.resolved_count, 0);
    let mut unknown = lens();
    unknown["source"] = json!({});
    let baseline = store.capture_bytes(&new_id(), &serde_json::to_vec(&unknown).unwrap());
    unknown["findings"] = json!([]);
    let current = store.capture_bytes(&new_id(), &serde_json::to_vec(&unknown).unwrap());
    let compared = store.compare("baseline", &baseline, "new", &current);
    assert!(matches!(compared.state, ComparisonState::Incompatible));
    assert_eq!(compared.resolved_count, 0);
}

#[test]
fn ici_locationless_findings_survive_and_changed_completed_check_scope_is_carried() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let mut baseline = ici(true, true);
    baseline["findings"][0]
        .as_object_mut()
        .unwrap()
        .remove("primary_location");
    let old = store.capture_bytes(&new_id(), &serde_json::to_vec(&baseline).unwrap());
    assert!(store.findings(&old, 0, 1).unwrap().findings[0]
        .location
        .is_none());
    let mut current = ici(true, false);
    current["checks"][0]["scope_digest"] = json!("changed-check-scope");
    let new = store.capture_bytes(&new_id(), &serde_json::to_vec(&current).unwrap());
    let compared = store.compare("baseline", &old, "new", &new);
    assert!(matches!(compared.state, ComparisonState::Partial));
    assert_eq!((compared.resolved_count, compared.carried_count), (0, 1));
    current["scope"]["kind"] = json!("selected");
    let new = store.capture_bytes(&new_id(), &serde_json::to_vec(&current).unwrap());
    assert!(matches!(
        store.compare("baseline", &old, "new", &new).state,
        ComparisonState::Incompatible
    ));
}

#[test]
fn finding_pages_bound_unicode_bytes_and_keep_body_digest_and_exact_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let mut document = lens();
    document["findings"] = json!((0..100)
        .map(|_| json!({"severity":"warn", "code":"unicode", "message":"界".repeat(20000)}))
        .collect::<Vec<_>>());
    let result = store.capture_bytes(&new_id(), &serde_json::to_vec(&document).unwrap());
    let mut offset = 0;
    while offset < 100 {
        let page = store.findings(&result, offset, 100).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() < 65536);
        assert!(page.next_offset > offset);
        for (index, finding) in page.findings.iter().enumerate() {
            assert_eq!(finding.index, offset + index);
            assert_eq!(finding.body_sha256, result.body().unwrap().sha256);
            assert!(finding.truncated);
        }
        offset = page.next_offset;
    }
}

#[test]
fn pin_byte_cap_is_separate_and_unpin_restores_age_retention_on_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&dir.path().join("data"))).unwrap();
    let mut records = vec![];
    // Body files may already have expired. The persisted declared byte budget
    // must still reject oversize pinned state, without integer overflow.
    for _ in 0..8 {
        let mut record = run(dir.path());
        record.pinned = true;
        record.structured_result = Some(StructuredResult::Expired {
            body: idk_workspace::structured_result::BodyReference {
                run_id: record.run_id.clone(),
                sha256: "a".repeat(64),
                bytes: 16 * 1024 * 1024,
            },
        });
        records.push(record);
    }
    records[0].log.bytes = 1;
    store
        .write_state("runs.json", &json!({"schema":2,"runs":records}))
        .unwrap();
    assert!(RunRegistry::open(store.clone(), SourceGate::default()).is_err());
    let mut pinned = run(dir.path());
    pinned.pinned = true;
    pinned.finished_at_ms = Some(1);
    store
        .write_state("runs.json", &json!({"schema":2,"runs":[pinned.clone()]}))
        .unwrap();
    let mut registry = RunRegistry::open(store.clone(), SourceGate::default()).unwrap();
    registry.pin_result(&pinned.run_id, false).unwrap();
    assert!(registry.info(&pinned.run_id).is_ok());
    drop(registry);
    assert!(RunRegistry::open(store, SourceGate::default())
        .unwrap()
        .list(None)
        .is_empty());
}

#[test]
fn retired_body_cleanup_intent_survives_failure_and_retries_without_removing_changed_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&dir.path().join("data"))).unwrap();
    let results = ResultStore::open(&store.state_dir).unwrap();
    let mut expired = run(dir.path());
    expired.finished_at_ms = Some(1);
    let original = serde_json::to_vec(&lens()).unwrap();
    let body = results.capture_bytes(&expired.run_id, &original);
    let managed = path(&store.state_dir, &body);
    expired.structured_result = Some(body);
    fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&managed, b"replacement must be preserved").unwrap();
    store
        .write_state("runs.json", &json!({"schema":2,"runs":[expired]}))
        .unwrap();
    assert!(RunRegistry::open(store.clone(), SourceGate::default()).is_err());
    let ledger: Value =
        serde_json::from_slice(&fs::read(store.state_dir.join("runs.json")).unwrap()).unwrap();
    assert!(ledger["runs"].as_array().unwrap().is_empty());
    assert_eq!(ledger["retired"].as_array().unwrap().len(), 1);
    assert_eq!(
        fs::read(&managed).unwrap(),
        b"replacement must be preserved"
    );
    // Simulate an operator restoring the owned snapshot after inspecting it.
    fs::write(&managed, &original).unwrap();
    fs::set_permissions(&managed, fs::Permissions::from_mode(0o400)).unwrap();
    let registry = RunRegistry::open(store.clone(), SourceGate::default()).unwrap();
    assert!(registry.list(None).is_empty());
    assert!(!managed.exists());
    let ledger: Value =
        serde_json::from_slice(&fs::read(store.state_dir.join("runs.json")).unwrap()).unwrap();
    assert!(ledger["retired"].as_array().unwrap().is_empty());
}

#[test]
fn legacy_migration_budget_failure_preserves_original_ledger_and_backup() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(&dir.path().join("data"))).unwrap();
    let input = dir.path().join("legacy.json");
    fs::write(&input, serde_json::to_vec(&lens()).unwrap()).unwrap();
    let mut old = run(dir.path());
    old.structured_result = Some(idk_workspace::structured_result::inspect(
        &input,
        &old.run_id,
    ));
    store
        .write_state("runs.json", &json!({"schema":1,"runs":[old]}))
        .unwrap();
    let original = fs::read(store.state_dir.join("runs.json")).unwrap();
    ResultStore::open(&store.state_dir).unwrap();
    fs::File::create(store.state_dir.join("results/owned-budget-fixture"))
        .unwrap()
        .set_len(TOTAL_RESULT_BYTES)
        .unwrap();
    assert!(RunRegistry::open(store.clone(), SourceGate::default()).is_err());
    assert_eq!(
        fs::read(store.state_dir.join("runs.json")).unwrap(),
        original
    );
    assert_eq!(
        fs::read(store.state_dir.join("runs.previous-v1.json")).unwrap(),
        original
    );
}

#[test]
fn changed_ledger_summary_cannot_turn_incomplete_body_into_resolved_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResultStore::open(dir.path()).unwrap();
    let mut document = lens();
    document["complete"] = json!(false);
    let mut result = store.capture_bytes(&new_id(), &serde_json::to_vec(&document).unwrap());
    let StructuredResult::Stored { summary, .. } = &mut result else {
        panic!()
    };
    summary.complete = Some(true);
    assert!(matches!(
        store.verify(&result),
        StructuredResult::Tampered { .. }
    ));
    let baseline = store.capture_bytes(&new_id(), &serde_json::to_vec(&lens()).unwrap());
    let compared = store.compare("baseline", &baseline, "changed", &result);
    assert!(matches!(compared.state, ComparisonState::Unavailable));
    assert_eq!(compared.resolved_count, 0);
}

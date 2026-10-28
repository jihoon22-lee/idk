use idk_workspace::structured_result::{inspect, StructuredResult};
use std::fs;

#[test]
fn producer_contract_fixtures_preserve_partial_scope_and_independent_identity() {
    for (fixture, id) in [
        (include_str!("fixtures/ici-result-v2.json"), "idk-run"),
        (
            include_str!("fixtures/lens-result-v1.json"),
            "fixture-run-1",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.json");
        fs::write(&path, fixture).unwrap();
        let StructuredResult::Available {
            document,
            idk_run_id,
            producer_run_id,
            ..
        } = inspect(&path, id)
        else {
            panic!("fixture rejected")
        };
        assert_eq!(idk_run_id, id);
        assert_eq!(
            document,
            serde_json::from_str::<serde_json::Value>(fixture).unwrap()
        );
        if document["schema_id"] == "ici.next.run" {
            assert_eq!(document["gate"]["selected"], "INCOMPLETE");
            assert_eq!(document["gate"]["has_violations"], true);
            assert_ne!(producer_run_id.as_deref(), Some(id));
            assert!(document["checks"].is_null());
        } else {
            assert_eq!(document["complete"], false);
            assert!(document["findings"][0].get("primary_location").is_none());
        }
    }
}

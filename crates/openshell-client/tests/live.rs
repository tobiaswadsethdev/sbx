//! Integration tests against a real gateway.
//!
//! Ignored by default because they need a running OpenShell gateway and a
//! Docker daemon. Run them with:
//!
//! ```sh
//! cargo test -p openshell-client -- --ignored --test-threads=1
//! ```

use std::collections::BTreeMap;

use openshell_client::{CliClient, CreateOpts, Error, OpenShell, Phase};

const TEST_LABEL: &str = "sbx.test";

fn client() -> CliClient {
    CliClient::new()
}

#[test]
#[ignore = "needs a live gateway"]
fn gateway_is_connected() {
    let st = client().status().expect("status");
    assert!(st.is_connected(), "gateway not connected: {st:?}");
    assert!(!st.version.is_empty());
}

#[test]
#[ignore = "needs a live gateway"]
fn missing_sandbox_is_not_found() {
    match client().get("sbx-definitely-does-not-exist") {
        Err(Error::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
#[ignore = "needs a live gateway"]
fn create_exec_delete_roundtrip() {
    let c = client();
    let name = "sbx-livetest";

    // Leftover from an interrupted run would make create fail.
    let _ = c.delete(name);

    let mut labels = BTreeMap::new();
    labels.insert(TEST_LABEL.to_string(), "roundtrip".to_string());

    let created = c
        .create(&CreateOpts {
            name: name.to_string(),
            labels,
            command: vec!["true".into()],
            ..Default::default()
        })
        .expect("create");

    assert_eq!(created.name, name);
    assert_eq!(created.phase, Phase::Ready);
    assert_eq!(
        created.labels.get(TEST_LABEL).map(String::as_str),
        Some("roundtrip")
    );

    // Labels are the session index, so selector filtering has to work.
    let listed = c
        .list(Some(&format!("{TEST_LABEL}=roundtrip")))
        .expect("list");
    assert!(listed.iter().any(|s| s.name == name), "not in {listed:?}");

    let out = c.exec(name, &["sh", "-c", "echo hello"]).expect("exec");
    assert!(out.ok());
    assert_eq!(out.trimmed(), "hello");

    // A non-zero remote exit is data, not a client error.
    let out = c
        .exec(name, &["sh", "-c", "echo oops >&2; exit 42"])
        .expect("exec should not error on non-zero exit");
    assert_eq!(out.exit_code, 42);
    assert!(out.stderr.contains("oops"), "stderr was {:?}", out.stderr);

    c.delete(name).expect("delete");
    match c.get(name) {
        Err(Error::NotFound(_)) => {}
        other => panic!("expected NotFound after delete, got {other:?}"),
    }
}

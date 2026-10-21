use super::*;
use serde_json::{json, Value};
use std::fs;

fn wire() -> Value {
    json!({"statistics":{"total":{"sources":2,"clones":1}},"duplicates":[{
        "format":"rust","kind":"exact","isNew":true,"tokens":60,"lines":8,
        "firstFile":{"name":"core/lib/via_a/src/lib.rs","start":1,"end":8},
        "secondFile":{"name":"via_verifier/bin/test.rs","start":2,"end":9}}]})
}
fn parsed(value: &Value) -> scan::Result<scan::Report> {
    scan::parse(&serde_json::to_vec(value)?, Path::new("/repo"), false)
}

#[test]
fn missing_or_wrong_critical_json_is_an_error_but_additive_fields_are_compatible() {
    for pointer in [
        "/duplicates",
        "/statistics",
        "/statistics/total",
        "/statistics/total/sources",
        "/statistics/total/clones",
        "/duplicates/0/format",
        "/duplicates/0/kind",
        "/duplicates/0/isNew",
        "/duplicates/0/tokens",
        "/duplicates/0/lines",
        "/duplicates/0/firstFile",
        "/duplicates/0/secondFile",
        "/duplicates/0/firstFile/name",
        "/duplicates/0/firstFile/start",
        "/duplicates/0/firstFile/end",
    ] {
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let mut missing = wire();
        missing.pointer_mut(parent).unwrap().as_object_mut().unwrap().remove(key);
        assert!(parsed(&missing).is_err(), "missing {pointer}");
        let mut wrong = wire();
        *wrong.pointer_mut(pointer).unwrap() = json!(null);
        assert!(parsed(&wrong).is_err(), "wrong type {pointer}");
    }
    let mut additive = wire();
    additive["future"] = json!({"x":42});
    additive["duplicates"][0]["future"] = json!(true);
    assert_eq!(parsed(&additive).unwrap().duplicates.len(), 1);
    for (pointer, value) in [
        ("/statistics/total/sources", json!(0)),
        ("/statistics/total/clones", json!(2)),
        ("/duplicates/0/firstFile/name", json!("../outside.rs")),
        ("/duplicates/0/firstFile/name", json!("/outside.rs")),
        ("/duplicates/0/firstFile/name", json!("file.txt")),
        ("/duplicates/0/firstFile/name", json!("bad`name.rs")),
        ("/duplicates/0/firstFile/start", json!(0)),
        ("/duplicates/0/firstFile/end", json!(0)),
        ("/duplicates/0/tokens", json!(0)),
        ("/duplicates/0/kind", json!("semantic")),
    ] {
        let mut broken = wire();
        *broken.pointer_mut(pointer).unwrap() = value;
        assert!(parsed(&broken).is_err(), "invalid {pointer}");
    }
}

#[test]
fn exact_ownership_gate_and_old_mixed_pairs_follow_the_decisions_in_both_orders() {
    let mut report = parsed(&wire()).unwrap();
    let d = &mut report.duplicates[0];
    let empty = BTreeSet::new();
    let renames = BTreeMap::new();
    assert!(blocking(d, &empty, &renames));
    d.is_new = false;
    assert!(!blocking(d, &empty, &renames));
    d.is_new = true;
    d.second_file.name = RepoPath("core/lib/upstream/src/lib.rs".into());
    assert!(blocking(d, &empty, &renames));
    let old = BTreeSet::from([d.pair()]);
    assert!(!blocking(d, &old, &renames));
    std::mem::swap(&mut d.first_file, &mut d.second_file);
    assert!(!blocking(d, &old, &renames));
    let prior = d.second_file.name.clone();
    d.second_file.name = RepoPath("core/bin/via_renamed/main.rs".into());
    assert!(!blocking(d, &old, &BTreeMap::from([(d.second_file.name.clone(), prior)])));
    d.second_file.name = RepoPath("core/lib/other/src/lib.rs".into());
    assert!(!blocking(d, &empty, &renames));
    for path in [
        "via_indexer/tests/a.rs",
        "core/bin/via_cli/main.rs",
        "nested/via_module/lib.rs",
        "via_verifier/a.rs",
    ] {
        assert!(RepoPath(path.into()).via());
    }
    assert!(!RepoPath("core/lib/not_via_name/lib.rs".into()).via());
}

#[test]
fn supported_release_versions_only_and_markdown_is_data() {
    for version in ["jscpd 5.4.2", "jscpd 5.10.0", "jscpd 6.0.0"] {
        assert!(scan::version(version).is_ok());
    }
    for version in ["jscpd 5.3.0", "jscpd 5.4.2-beta.1", "cpd 5.4.2", "5.4.2", "jscpd latest"] {
        assert!(scan::version(version).is_err());
    }
    let fragment = scan::Fragment {
        name: RepoPath("core/lib/via_reorg/src/lib.rs".into()),
        start: 12,
        end: 13,
    };
    assert_eq!(report::location(&fragment), "`core/lib/via_reorg/src/lib.rs:12–13`");
    assert_eq!(report::escape("[a]|<script>`"), "&#91;a&#93;&#124;&lt;script&gt;&#96;");
}

#[test]
fn archive_and_name_metadata_preserve_bytes_and_handle_dirty_renames() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    scan::git(root, &["init", "-q"]).unwrap();
    scan::git(root, &["config", "user.name", "Fixture"]).unwrap();
    scan::git(root, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fs::write(root.join("old name.rs"), b"// exact bytes\r\nfn a() {}\r\n").unwrap();
    fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    scan::git(root, &["add", "."]).unwrap();
    scan::git(root, &["commit", "-qm", "fixture"]).unwrap();
    let destination = root.join("export");
    scan::archive(root, "HEAD", &destination).unwrap();
    assert_eq!(
        fs::read(root.join("old name.rs")).unwrap(),
        fs::read(destination.join("old name.rs")).unwrap()
    );
    fs::remove_dir_all(destination).unwrap();
    scan::git(root, &["mv", "old name.rs", "new name.rs"]).unwrap();
    fs::write(root.join("untracked.rs"), "fn untracked() {}\n").unwrap();
    let (paths, renames) = changed(root, "HEAD").unwrap();
    assert!(paths.contains(&RepoPath("untracked.rs".into())));
    assert_eq!(renames.get(&RepoPath("new name.rs".into())), Some(&RepoPath("old name.rs".into())));
}

#[test]
fn process_status_matches_fresh_schema_without_parsing_notices() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let config = root.join("config.json");
    fs::write(&config, "{}").unwrap();
    let executable = root.join("scanner");
    let fake = |report: Option<Value>, status: i32, diagnostic: &str| {
        let report = report
            .map(|v| format!("cat > \"$out/jscpd-report.json\" <<'JSON'\n{v}\nJSON\n"))
            .unwrap_or_default();
        fs::write(&executable, format!("#!/bin/sh\nwhile [ \"$1\" != --output ]; do shift; done\nshift\nout=$1\n{report}cat >&2 <<'MESSAGE'\n{diagnostic}\nMESSAGE\nexit {status}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    };
    fake(Some(wire()), 0, "New release notice: scanning Rust");
    scan::run(&executable, root, &config, &root.join("valid"), &[], false).unwrap();
    assert!(scan::run(&executable, root, &config, &root.join("valid"), &[], false).is_err());
    for (i, report, status, diagnostic) in [
        (1, Some(wire()), 1, "ERROR: failure"),
        (2, None, 0, ""),
        (4, Some(json!({})), 0, ""),
        (5, Some(wire()), 2, "bad config"),
    ] {
        fake(report, status, diagnostic);
        assert!(scan::run(
            &executable,
            root,
            &config,
            &root.join(format!("failed-{i}")),
            &[],
            false
        )
        .is_err());
    }
    assert!(scan::parse(&serde_json::to_vec(&wire()).unwrap(), root, true).is_err());
    let mut boundary = wire();
    boundary["boundaries"] = json!([
        {"name":scan::RULES[0],"violations":[{"kind":"exact","tokens":60,"lines":8,
        "copy":{"name":"core/node/via_a/lib.rs","start":1,"end":8},
        "protected":{"name":"core/lib/via_reorg/src/lib.rs","start":1,"end":8}}]},
        {"name":scan::RULES[1],"violations":[]}]);
    fake(
        Some(boundary.clone()),
        1,
        "Warning: new notice\nBoundary wording may change between releases",
    );
    scan::run(&executable, root, &config, &root.join("completed-boundary"), &[], true).unwrap();
    for (label, status) in [("wrong-success", 0), ("tool-error", 2)] {
        fake(Some(boundary.clone()), status, "diagnostic context");
        assert!(scan::run(&executable, root, &config, &root.join(label), &[], true).is_err());
    }
    let mut no_violations = boundary.clone();
    no_violations["boundaries"][0]["violations"] = json!([]);
    fake(Some(no_violations.clone()), 0, "Warning: rule keeps copies out of no file");
    scan::run(&executable, root, &config, &root.join("completed-no-violations"), &[], true)
        .unwrap();
    fake(Some(no_violations), 1, "ERROR: tool failed");
    assert!(
        scan::run(&executable, root, &config, &root.join("non-finding-error"), &[], true).is_err()
    );
    boundary["boundaries"][0]["name"] = json!("unexpected-rule");
    assert!(scan::parse(&serde_json::to_vec(&boundary).unwrap(), root, true).is_err());
}

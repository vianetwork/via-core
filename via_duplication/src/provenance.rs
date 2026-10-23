//! Offline literal-copy checks and explicit upstream-drift reporting bypass scanner installation.
// Adapt TestFX's required source identities and blob comparison to delimited Rust copy regions.
// https://github.com/microsoft/testfx/blob/86637afa32e290883fc832bf0ddb782e809bef57/.github/scripts/check_vendored_files.py

use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::{Command as Process, ExitCode},
};

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::scan::Result;

#[derive(Subcommand)]
pub enum Command {
    Check {
        #[arg(long, default_value = "third_party/bitcoin-reader-sources.json")]
        manifest: PathBuf,
    },
    Drift {
        #[arg(long, default_value = "third_party/bitcoin-reader-sources.json")]
        manifest: PathBuf,
        /// Explicitly enable tracking-issue writes; omission only reports upstream blobs.
        #[arg(long)]
        issues_repo: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    repo: String,
    tracked_ref: String,
    baseline_commit: String,
    upstream_path: String,
    baseline_blob: String,
    upstream_region: String,
    local_path: String,
    start_marker: String,
    end_marker: String,
    retained_text: String,
    changes: String,
}

fn pinned_sha(value: &str) -> bool {
    value.len() == 40
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && value.bytes().any(|byte| byte != b'0')
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-./".contains(&byte))
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn repo_name(value: &str) -> bool {
    token(value) && value.split('/').count() == 2
}

fn relative_path(value: &str) -> bool {
    token(value)
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn read_manifest(root: &Path, path: &Path) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(root.join(path))?)?;
    if manifest.schema_version != 1 || manifest.entries.is_empty() {
        return Err("unsupported or empty provenance manifest".into());
    }
    let mut ids = BTreeSet::new();
    for entry in &manifest.entries {
        if !token(&entry.id)
            || entry.id.contains('/')
            || !ids.insert(&entry.id)
            || !repo_name(&entry.repo)
            || !token(&entry.tracked_ref)
            || !pinned_sha(&entry.baseline_commit)
            || !pinned_sha(&entry.baseline_blob)
            || !relative_path(&entry.upstream_path)
            || !relative_path(&entry.local_path)
            || [
                &entry.upstream_region,
                &entry.start_marker,
                &entry.end_marker,
                &entry.retained_text,
                &entry.changes,
            ]
            .iter()
            .any(|text| text.trim().is_empty())
            || entry.start_marker == entry.end_marker
        {
            return Err(
                format!("invalid required provenance identity/region: {}", entry.id).into(),
            );
        }
    }
    Ok(manifest)
}

fn check(root: &Path, manifest: &Manifest) -> Result<()> {
    for entry in &manifest.entries {
        let path = root.join(&entry.local_path).canonicalize()?;
        if !path.starts_with(root) {
            return Err(format!("{}: local path escapes repository", entry.id).into());
        }
        let text = std::fs::read_to_string(path)?;
        if text.matches(&entry.start_marker).count() != 1
            || text.matches(&entry.end_marker).count() != 1
        {
            return Err(format!("{}: copy markers must each occur once", entry.id).into());
        }
        let after_marker = text.find(&entry.start_marker).unwrap() + entry.start_marker.len();
        let start = text[after_marker..]
            .find('\n')
            .map(|offset| after_marker + offset + 1)
            .ok_or("copy start marker has no following line")?;
        let end = text.find(&entry.end_marker).unwrap();
        let end_line = text[..end].rfind('\n').map_or(0, |offset| offset + 1);
        if start > end_line || text[start..end_line] != entry.retained_text {
            return Err(format!("{}: literal retained region changed", entry.id).into());
        }
    }
    Ok(())
}

trait DriftApi {
    fn blob(&mut self, entry: &Entry) -> Result<String>;
    fn issue(&mut self, id: &str, title: &str, body: &str) -> Result<()>;
}

fn drift(manifest: &Manifest, api: &mut impl DriftApi, write_issues: bool) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for entry in &manifest.entries {
        let blob = api.blob(entry)?;
        if !pinned_sha(&blob) {
            return Err(format!("{}: invalid upstream blob response", entry.id).into());
        }
        if blob == entry.baseline_blob {
            continue;
        }
        changed.push(entry.id.clone());
        if write_issues {
            let body = format!(
                "<!-- bitcoin-reader-source:{} -->\nThe tracked upstream file changed. Review the diff before any port.\n\nSource: https://github.com/{}/blob/{}/{}\nBaseline commit: `{}`\nBaseline blob: `{}`\nCurrent blob: `{blob}`\nRetained region: {}\nLocal file: `{}`\n",
                entry.id, entry.repo, entry.tracked_ref, entry.upstream_path,
                entry.baseline_commit, entry.baseline_blob, entry.upstream_region, entry.local_path,
            );
            api.issue(
                &entry.id,
                &format!("Bitcoin reader upstream drift: {}", entry.id),
                &body,
            )?;
        }
    }
    Ok(changed)
}

fn github(method: &str, endpoint: &str, input: Option<Value>) -> Result<Vec<Value>> {
    use std::io::Write;
    use std::process::Stdio;
    let mut command = Process::new("gh");
    command.args(["api", "--method", method, endpoint]);
    if input.is_some() {
        command.args(["--input", "-"]).stdin(Stdio::piped());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&input)?)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "GitHub {method} {endpoint} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<Value>()
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

struct GitHub {
    issues_repo: Option<String>,
}

impl DriftApi for GitHub {
    fn blob(&mut self, entry: &Entry) -> Result<String> {
        let endpoint = format!(
            "repos/{}/contents/{}?ref={}",
            entry.repo, entry.upstream_path, entry.tracked_ref
        );
        let response = github("GET", &endpoint, None)?;
        response
            .first()
            .and_then(|value| value["sha"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| "upstream contents response has no blob SHA".into())
    }

    fn issue(&mut self, id: &str, title: &str, body: &str) -> Result<()> {
        let repo = self
            .issues_repo
            .as_ref()
            .ok_or("issue writes were not enabled")?;
        let marker = format!("<!-- bitcoin-reader-source:{id} -->");
        let mut page = 1;
        loop {
            let response = github(
                "GET",
                &format!("repos/{repo}/issues?state=open&per_page=100&page={page}"),
                None,
            )?;
            let issues = response
                .first()
                .and_then(Value::as_array)
                .ok_or("malformed issue-list response")?;
            if let Some(issue) = issues.iter().find(|issue| {
                issue.get("pull_request").is_none()
                    && issue["body"]
                        .as_str()
                        .is_some_and(|body| body.starts_with(&marker))
            }) {
                if issue["body"] != body || issue["title"] != title {
                    let number = issue["number"].as_u64().ok_or("issue has no number")?;
                    github(
                        "PATCH",
                        &format!("repos/{repo}/issues/{number}"),
                        Some(json!({"title":title,"body":body})),
                    )?;
                }
                return Ok(());
            }
            if issues.len() < 100 {
                github(
                    "POST",
                    &format!("repos/{repo}/issues"),
                    Some(json!({"title":title,"body":body})),
                )?;
                return Ok(());
            }
            page += 1;
        }
    }
}

pub fn run(command: Command) -> ExitCode {
    let result = (|| -> Result<()> {
        let root = PathBuf::from(
            crate::scan::git(Path::new("."), &["rev-parse", "--show-toplevel"])?.trim(),
        )
        .canonicalize()?;
        let (path, issues_repo, do_drift) = match command {
            Command::Check { manifest } => (manifest, None, false),
            Command::Drift {
                manifest,
                issues_repo,
            } => (manifest, issues_repo, true),
        };
        if issues_repo.as_ref().is_some_and(|repo| !repo_name(repo)) {
            return Err("invalid issues repository".into());
        }
        let manifest = read_manifest(&root, &path)?;
        check(&root, &manifest)?;
        if do_drift {
            let writes = issues_repo.is_some();
            let changed = drift(&manifest, &mut GitHub { issues_repo }, writes)?;
            println!(
                "Upstream drift: {} changed regions: {}",
                changed.len(),
                changed.join(", ")
            );
        } else {
            println!(
                "Provenance OK: {} literal regions; offline check",
                manifest.entries.len()
            );
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Provenance error: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn entry() -> Value {
        json!({"id":"walk","repo":"owner/repo","tracked_ref":"main","baseline_commit":"a".repeat(40),"upstream_path":"src/lib.rs","baseline_blob":"b".repeat(40),"upstream_region":"walk loop","local_path":"copy.rs","start_marker":"// start","end_marker":"// end","retained_text":"\tloop {}\n","changes":"surrounding adapters only"})
    }

    fn fixture(entry: Value) -> (tempfile::TempDir, Manifest) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("copy.rs"), "// start\n\tloop {}\n// end\n").unwrap();
        std::fs::write(
            root.path().join("manifest.json"),
            serde_json::to_vec(&json!({"schema_version":1,"entries":[entry]})).unwrap(),
        )
        .unwrap();
        let manifest = read_manifest(root.path(), Path::new("manifest.json")).unwrap();
        (root, manifest)
    }

    #[test]
    fn every_manifest_field_is_required() {
        for field in entry().as_object().unwrap().keys() {
            let mut missing = entry();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Entry>(missing).is_err(), "{field}");
        }
        for field in ["schema_version", "entries"] {
            let mut missing = json!({"schema_version":1,"entries":[entry()]});
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<Manifest>(missing).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn malformed_pins_paths_and_regions_fail() {
        for (field, bad) in [
            ("baseline_commit", ""),
            ("baseline_commit", "main"),
            (
                "baseline_commit",
                "0000000000000000000000000000000000000000",
            ),
            ("baseline_blob", "bad"),
            ("local_path", "../copy.rs"),
            ("local_path", "/copy.rs"),
            ("upstream_path", ""),
            ("repo", "owner"),
            ("retained_text", " "),
        ] {
            let (root, _) = fixture(entry());
            let mut invalid = entry();
            invalid[field] = json!(bad);
            std::fs::write(
                root.path().join("manifest.json"),
                serde_json::to_vec(&json!({"schema_version":1,"entries":[invalid]})).unwrap(),
            )
            .unwrap();
            assert!(
                read_manifest(root.path(), Path::new("manifest.json")).is_err(),
                "{field}: {bad}"
            );
        }
    }

    #[test]
    fn unchanged_literal_passes_offline_and_any_byte_change_fails() {
        let (root, manifest) = fixture(entry());
        check(root.path(), &manifest).unwrap();
        for text in [
            "// start\n loop {}\n// end\n",
            "// start\n\tloop { break; }\n// end\n",
            "// start\n\tloop {}\n",
            "// start\n\tloop {}\n// end\n// start\n",
        ] {
            std::fs::write(root.path().join("copy.rs"), text).unwrap();
            assert!(check(root.path(), &manifest).is_err());
        }
    }

    struct Fake {
        blob: Option<String>,
        issues: BTreeMap<String, String>,
        writes: usize,
    }

    impl DriftApi for Fake {
        fn blob(&mut self, _: &Entry) -> Result<String> {
            self.blob.clone().ok_or_else(|| "source unavailable".into())
        }
        fn issue(&mut self, id: &str, _: &str, body: &str) -> Result<()> {
            if self.issues.get(id).map(String::as_str) != Some(body) {
                self.issues.insert(id.into(), body.into());
                self.writes += 1;
            }
            Ok(())
        }
    }

    #[test]
    fn drift_is_idempotent_and_fetch_or_decode_failures_remain_errors() {
        let (_, manifest) = fixture(entry());
        let mut api = Fake {
            blob: Some("b".repeat(40)),
            issues: BTreeMap::new(),
            writes: 0,
        };
        assert!(drift(&manifest, &mut api, true).unwrap().is_empty());
        assert_eq!(api.writes, 0);
        api.blob = Some("c".repeat(40));
        assert_eq!(drift(&manifest, &mut api, true).unwrap(), ["walk"]);
        drift(&manifest, &mut api, true).unwrap();
        assert_eq!((api.issues.len(), api.writes), (1, 1));
        api.blob = Some("d".repeat(40));
        drift(&manifest, &mut api, true).unwrap();
        assert_eq!((api.issues.len(), api.writes), (1, 2));
        api.blob = None;
        assert!(drift(&manifest, &mut api, true).is_err());
        api.blob = Some("invalid json/blob".into());
        assert!(drift(&manifest, &mut api, true).is_err());
        assert_eq!(api.writes, 2);
    }
}

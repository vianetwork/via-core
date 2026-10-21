use semver::Version;
use serde::Deserialize;
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const RULES: [&str; 2] = ["reuse-shared-via-reorg", "reuse-shared-via-btc-client"];

// Narrow required fields, with additive fields tolerated. Native JSON is the wire contract:
// https://github.com/kucherenko/jscpd/blob/db415b5d7cc34d72a324fe5eae2147646dc3558b/rust/crates/cpd-reporter/src/json_reporter.rs
#[derive(Deserialize)]
pub struct Report {
    pub duplicates: Vec<Duplicate>,
    pub statistics: Statistics,
    pub boundaries: Option<Vec<Boundary>>,
}
#[derive(Deserialize)]
pub struct Statistics {
    pub total: Total,
}
#[derive(Deserialize)]
pub struct Total {
    pub sources: usize,
    pub clones: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Duplicate {
    pub format: String,
    pub kind: Kind,
    pub is_new: bool,
    pub tokens: usize,
    pub lines: usize,
    pub first_file: Fragment,
    pub second_file: Fragment,
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Exact,
    Renamed,
}
#[derive(Deserialize)]
pub struct Fragment {
    pub name: RepoPath,
    pub start: usize,
    pub end: usize,
}
#[derive(Deserialize, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[serde(transparent)]
pub struct RepoPath(pub String);
pub type FilePair = (RepoPath, RepoPath);
#[derive(Deserialize)]
pub struct Boundary {
    pub name: String,
    pub violations: Vec<Violation>,
}
#[derive(Deserialize)]
pub struct Violation {
    pub kind: Kind,
    pub tokens: usize,
    pub lines: usize,
    pub copy: Fragment,
    pub protected: Fragment,
}

impl RepoPath {
    pub fn via(&self) -> bool {
        self.0.split('/').any(|s| s.starts_with("via_"))
    }
}
impl Fragment {
    fn normalize(&mut self, root: &Path) -> Result<()> {
        let raw = Path::new(&self.name.0);
        let path = if raw.is_absolute() { raw.strip_prefix(root)? } else { raw };
        if path.components().any(|c| !matches!(c, Component::Normal(_)))
            || path.extension().is_none_or(|x| x != "rs")
            || path.to_str().is_none()
            || self.name.0.contains(['`', '\n', '\r'])
            || self.start == 0
            || self.end < self.start
        {
            return Err("invalid Rust path or inclusive line range in detector JSON".into());
        }
        self.name = RepoPath(path.to_str().unwrap().to_owned());
        Ok(())
    }
}
impl Duplicate {
    pub fn pair(&self) -> FilePair {
        pair(self.first_file.name.clone(), self.second_file.name.clone())
    }
}
pub fn pair(a: RepoPath, b: RepoPath) -> FilePair {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

pub fn parse(bytes: &[u8], root: &Path, boundary: bool) -> Result<Report> {
    let mut report: Report = serde_json::from_slice(bytes)?;
    if report.statistics.total.sources == 0
        || report.statistics.total.clones != report.duplicates.len()
    {
        return Err("empty scan or inconsistent clone count".into());
    }
    for d in &mut report.duplicates {
        if d.format != "rust" || d.tokens == 0 || d.lines == 0 {
            return Err("invalid clone format/size".into());
        }
        d.first_file.normalize(root)?;
        d.second_file.normalize(root)?;
        if d.lines != d.first_file.end - d.first_file.start + 1 {
            return Err("inconsistent clone line count".into());
        }
    }
    if boundary {
        let rules = report.boundaries.as_mut().ok_or("missing boundary results")?;
        if rules.iter().map(|r| r.name.as_str()).collect::<Vec<_>>() != RULES {
            return Err("unexpected boundary rules".into());
        }
        for rule in rules {
            for v in &mut rule.violations {
                if v.kind != Kind::Exact || v.tokens == 0 || v.lines == 0 {
                    return Err("invalid boundary clone".into());
                }
                v.copy.normalize(root)?;
                v.protected.normalize(root)?;
                if v.lines != v.copy.end - v.copy.start + 1 {
                    return Err("inconsistent boundary line count".into());
                }
            }
        }
    }
    Ok(report)
}

pub fn output(command: &mut Command) -> Result<String> {
    let result = command.output()?;
    if !result.status.success() {
        return Err(format!(
            "{} failed ({}): {}",
            command.get_program().to_string_lossy(),
            result.status,
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(result.stdout)?)
}
pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    output(Command::new("git").current_dir(root).args(args))
}

// Resolve once in npm's documented local bin directory, rather than searching its cache/PATH:
// https://docs.npmjs.com/cli/v10/configuring-npm/folders/#executables
pub fn install(temp: &Path) -> Result<(PathBuf, Version)> {
    let prefix = temp.join("npm");
    output(
        Command::new("npm")
            .args([
                "install",
                "--no-save",
                "--no-package-lock",
                "--no-audit",
                "--no-fund",
                "--prefix",
            ])
            .arg(&prefix)
            .arg("jscpd@latest"),
    )?;
    let executable = prefix.join("node_modules/.bin/jscpd");
    let version = version(&output(Command::new(&executable).arg("--version"))?)?;
    Ok((executable, version))
}
pub fn version(text: &str) -> Result<Version> {
    let version =
        Version::parse(text.trim().strip_prefix("jscpd ").ok_or("unrecognized jscpd version")?)?;
    if version < Version::new(5, 4, 2) || !version.pre.is_empty() {
        return Err(format!("unsupported jscpd {version}; need a release >=5.4.2").into());
    }
    Ok(version)
}

// One archive, no registered worktree; check both sides of the pipeline:
// https://git-scm.com/docs/git-archive
pub fn archive(root: &Path, oid: &str, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    let mut producer = Command::new("git")
        .current_dir(root)
        .args(["archive", "--format=tar", oid])
        .stdout(Stdio::piped())
        .spawn()?;
    let consumer = Command::new("tar")
        .args(["-x", "-C"])
        .arg(destination)
        .stdin(producer.stdout.take().unwrap())
        .status();
    let produced = producer.wait()?;
    if !consumer?.success() || !produced.success() {
        return Err("base archive/export failed".into());
    }
    Ok(())
}

// With only the JSON reporter, reconcile native status against validated, fresh boundary results.
// https://github.com/kucherenko/jscpd/blob/db415b5d7cc34d72a324fe5eae2147646dc3558b/rust/crates/cpd/src/main.rs
pub fn run(
    executable: &Path,
    root: &Path,
    config: &Path,
    out: &Path,
    flags: &[&str],
    boundary: bool,
) -> Result<Report> {
    fs::create_dir(out)?; // Refuse existing output, including stale JSON.
    let result = Command::new(executable)
        .current_dir(root)
        .arg(root)
        .arg("--config")
        .arg(config)
        .args([
            "--reporters",
            "json",
            "--absolute",
            "--silent",
            "--no-tips",
            "--no-colors",
            "--output",
        ])
        .arg(out)
        .args(flags)
        .output()?;
    let report = parse(&fs::read(out.join("jscpd-report.json"))?, root, boundary)?;
    let expected_kind = if flags.contains(&"renamed") { Kind::Renamed } else { Kind::Exact };
    if report.duplicates.iter().any(|d| d.kind != expected_kind) {
        return Err("unexpected clone kind for scan mode".into());
    }
    let valid_status = if boundary {
        let total: usize =
            report.boundaries.as_ref().unwrap().iter().map(|r| r.violations.len()).sum();
        result.status.code() == Some(if total == 0 { 0 } else { 1 })
    } else {
        result.status.success()
    };
    if !valid_status {
        return Err(format!(
            "scan failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    Ok(report)
}

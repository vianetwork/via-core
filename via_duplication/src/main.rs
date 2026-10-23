mod provenance;
mod report;
mod scan;
#[cfg(test)]
mod tests;

use clap::{Parser, Subcommand};
use scan::{FilePair, Kind, RepoPath, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::ExitCode,
};

// Adapt Agave's typed dispatch, with Via's three private synchronous responsibilities:
// https://github.com/anza-xyz/agave/blob/910af6bed4ad4f3a946f856da2a5b0618ea2f909/ci/xtask/src/main.rs
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Mode,
}
#[derive(Subcommand)]
enum Mode {
    Provenance {
        #[command(subcommand)]
        command: provenance::Command,
    },
    Check {
        #[arg(long)]
        base: Option<String>,
    },
    Boundaries,
    Inventory,
}
// Own fork policy: native newness blocks Via–Via; previously shared mixed pairs stay advisory.
// https://github.com/vianetwork/via-core/blob/c04f47d32de55e231452e487c6cccd815334015a/docs/research/duplication-detection-with-jscpd.md
#[derive(PartialEq, Eq)]
enum Ownership {
    Via,
    Mixed,
    Upstream,
}
fn classify(pair: &FilePair) -> Ownership {
    match (pair.0.via(), pair.1.via()) {
        (true, true) => Ownership::Via,
        (false, false) => Ownership::Upstream,
        _ => Ownership::Mixed,
    }
}
fn blocking(
    d: &scan::Duplicate,
    old_pairs: &BTreeSet<FilePair>,
    renames: &BTreeMap<RepoPath, RepoPath>,
) -> bool {
    let old = |p: &RepoPath| renames.get(p).unwrap_or(p).clone();
    d.kind == Kind::Exact
        && d.is_new
        && match classify(&d.pair()) {
            Ownership::Via => true,
            Ownership::Mixed => {
                !old_pairs.contains(&scan::pair(old(&d.first_file.name), old(&d.second_file.name)))
            }
            Ownership::Upstream => false,
        }
}

// File membership and one-to-one renames only; native isNew supplies the gate, not hunks.
// https://github.com/PostHog/posthog/blob/10e4218b671e8cf967c79993ba8607dae2f6eae7/bin/lint_duplication.py
fn changed(root: &Path, base: &str) -> Result<(BTreeSet<RepoPath>, BTreeMap<RepoPath, RepoPath>)> {
    let names = scan::git(root, &["diff", "--name-status", "-z", "--find-renames", base, "--"])?;
    let mut fields = names.split_terminator('\0');
    let (mut paths, mut renames) = (BTreeSet::new(), BTreeMap::new());
    while let Some(status) = fields.next() {
        let first = RepoPath(fields.next().ok_or("incomplete git name/status")?.to_owned());
        paths.insert(first.clone());
        if status.starts_with('R') || status.starts_with('C') {
            let last = RepoPath(fields.next().ok_or("incomplete git rename")?.to_owned());
            paths.insert(last.clone());
            if status.starts_with('R') {
                renames.insert(last, first);
            }
        }
    }
    for name in scan::git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?
        .split_terminator('\0')
    {
        paths.insert(RepoPath(name.to_owned()));
    }
    Ok((paths, renames))
}

fn execute(mode: Mode, temp: &Path, version: &mut String) -> Result<(u8, String)> {
    let root = std::fs::canonicalize(
        scan::git(Path::new("."), &["rev-parse", "--show-toplevel"])?.trim(),
    )?;
    let head = scan::git(&root, &["rev-parse", "HEAD"])?;
    let base = match &mode {
        Mode::Check { base } => {
            let reference = match base {
                Some(b) => b.clone(),
                None => scan::git(&root, &["merge-base", "HEAD", "origin/main"])
                    .map_err(|e| {
                        format!("cannot resolve local base; fetch origin/main or pass --base: {e}")
                    })?
                    .trim()
                    .to_owned(),
            };
            Some(
                scan::git(
                    &root,
                    &[
                        "rev-parse",
                        "--verify",
                        "--end-of-options",
                        &format!("{reference}^{{commit}}"),
                    ],
                )?
                .trim()
                .to_owned(),
            )
        }
        _ => None,
    };
    let (executable, resolved) = scan::install(temp)?;
    *version = resolved.to_string();
    let config = root.join("via_duplication/jscpd.json");
    let mut body = format!(
        "Tested HEAD: {} (working tree included). Base: {}.\n\n",
        head.trim(),
        base.as_deref().unwrap_or("none")
    );
    if matches!(mode, Mode::Boundaries) {
        let result = scan::run(
            &executable,
            &root,
            &root.join("via_duplication_boundaries.json"),
            &temp.join("boundaries"),
            &[],
            true,
        )?;
        for rule in result.boundaries.unwrap() {
            body += &format!("### {}: {} findings\n\n", rule.name, rule.violations.len());
            for v in rule.violations {
                body += &format!(
                    "- {} ↔ {} ({} tokens, {} lines)\n",
                    report::location(&v.protected),
                    report::location(&v.copy),
                    v.tokens,
                    v.lines
                );
            }
        }
        return Ok((0, body));
    }
    let (mut old_pairs, mut touched, mut renames) =
        (BTreeSet::new(), BTreeSet::new(), BTreeMap::new());
    let baseline = temp.join("base-baseline.json");
    if let Some(base) = &base {
        let export = temp.join("base");
        scan::archive(&root, base, &export)?;
        let before = scan::run(
            &executable,
            &export,
            &config,
            &temp.join("base-report"),
            &["--baseline", baseline.to_str().ok_or("non-UTF8 temp path")?, "--update-baseline"],
            false,
        )?;
        if !baseline.is_file() {
            return Err("base scan did not create its native baseline".into());
        }
        old_pairs
            .extend(before.duplicates.iter().filter(|d| d.kind == Kind::Exact).map(|d| d.pair()));
        (touched, renames) = changed(&root, base)?;
    }
    let flags = if base.is_some() {
        vec!["--baseline", baseline.to_str().ok_or("non-UTF8 temp path")?]
    } else {
        vec![]
    };
    let exact = scan::run(&executable, &root, &config, &temp.join("candidate"), &flags, false)?;
    let mut rows = Vec::new();
    let mut blocked = 0;
    let mut counts = BTreeMap::<&str, usize>::new();
    let (mut via, mut mixed, mut upstream) = (0, 0, 0);
    for d in &exact.duplicates {
        if d.kind != Kind::Exact {
            return Err("non-exact clone in exact scan".into());
        }
        match classify(&d.pair()) {
            Ownership::Via => via += 1,
            Ownership::Mixed => mixed += 1,
            Ownership::Upstream => {
                upstream += 1;
                continue;
            }
        }
        let (rank, label) = if base.is_none() {
            (0, "inventory")
        } else if blocking(d, &old_pairs, &renames) {
            blocked += 1;
            (0, "BLOCKING")
        } else if d.is_new {
            (1, "changed mixed pair (advisory)")
        } else if touched.contains(&d.first_file.name) || touched.contains(&d.second_file.name) {
            (2, "old copy in changed file")
        } else {
            (3, "untouched old copies (not listed)")
        };
        *counts.entry(label).or_default() += 1;
        if rank == 3 {
            continue;
        }
        rows.push((rank, d.first_file.name.clone(), d.first_file.start, report::finding(label, d)));
    }
    body += &format!(
        "Blocking exact copies: {blocked}. Sources: {}.\n\n",
        exact.statistics.total.sources
    );
    body += &format!(
        "Exact Via–Via: {via}; mixed: {mixed}; excluded upstream–upstream: {upstream}.\n\n"
    );
    for (class, count) in counts {
        body += &format!("- {class}: {count}\n");
    }
    rows.sort();
    body += "\n";
    for (_, _, _, row) in rows {
        body += &row;
    }
    body += if base.is_some() {
        "\n### Renamed copies in changed files (advisory)\n\n"
    } else {
        "\n### Renamed copies (advisory)\n\n"
    };
    match scan::run(
        &executable,
        &root,
        &config,
        &temp.join("renamed"),
        &["--kind", "renamed", "--ignore-identifiers=consistent"],
        false,
    ) {
        Ok(renamed) => {
            let mut findings = renamed
                .duplicates
                .iter()
                .filter(|d| classify(&d.pair()) != Ownership::Upstream)
                .filter(|d| {
                    base.is_none()
                        || touched.contains(&d.first_file.name)
                        || touched.contains(&d.second_file.name)
                })
                .collect::<Vec<_>>();
            findings.sort_by_key(|d| (d.pair(), d.first_file.start, d.second_file.start));
            body += &format!("{} findings.\n\n", findings.len());
            for d in findings {
                body += &report::finding("renamed", d);
            }
        }
        Err(error) => body += &format!("Scan failed: {}\n", report::escape(&error.to_string())),
    }
    Ok((u8::from(blocked > 0), body))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mode = match cli.command {
        Mode::Provenance { command } => return provenance::run(command),
        mode => mode,
    };
    let mut version = "not resolved".to_owned();
    // Explicit close reports cleanup failure; Drop remains the fallback on unwinding.
    // https://docs.rs/tempfile/3.14.0/tempfile/struct.TempDir.html
    let mut result = match tempfile::tempdir() {
        Ok(temp) => {
            let result = execute(mode, temp.path(), &mut version);
            match temp.close() {
                Ok(()) => result,
                Err(e) => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    };
    let (code, body) = match result.as_mut() {
        Ok((code, body)) => (*code, std::mem::take(body)),
        Err(error) => {
            eprintln!("Tool error: {error}");
            (2, format!("Tool error: {}\n", report::escape(&error.to_string())))
        }
    };
    use std::io::Write;
    if write!(std::io::stdout().lock(), "# Via duplication\n\njscpd version: {version}\n\n{}", body)
        .is_err()
    {
        eprintln!("Tool error: could not write report");
        return ExitCode::from(2);
    }
    ExitCode::from(code)
}

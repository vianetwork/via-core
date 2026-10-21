//! Real Git/tokenizer fixtures. Run explicitly: cargo +stable test --manifest-path via_duplication/Cargo.toml --test native -- --ignored
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

fn checked(command: &mut Command) -> Output {
    let out = command.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out
}
fn put(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn source(number: usize) -> String {
    format!("pub fn protected_value(input: u64) -> u64 {{\n    let first = input.wrapping_mul({number});\n    let second = first.wrapping_add(991);\n    let third = second.rotate_left(7);\n    let fourth = third.wrapping_mul(3257);\n    let fifth = fourth.wrapping_sub(231);\n    let sixth = fifth.rotate_left(23);\n    let seventh = sixth.wrapping_add(811);\n    let eighth = seventh.wrapping_mul(237);\n    eighth.rotate_right(13)\n}}\n")
}
fn git(root: &Path, args: &[&str]) -> String {
    String::from_utf8(checked(Command::new("git").current_dir(root).args(args)).stdout).unwrap()
}
struct Fixture {
    dir: TempDir,
    tools: PathBuf,
    scanner: PathBuf,
}
impl Fixture {
    fn new(scanner: &Path, paths: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "via_duplication/jscpd.json", include_str!("../jscpd.json"));
        put(
            root,
            "via_duplication_boundaries.json",
            include_str!("../../via_duplication_boundaries.json"),
        );
        put(root, ".gitignore", "ignored/\n");
        for path in paths {
            put(root, path, &source(7123));
        }
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Fixture"]);
        git(root, &["config", "user.email", "fixture@example.invalid"]);
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "fixture"]);
        git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let tools = root.join("tools");
        fs::create_dir(&tools).unwrap();
        put(root, "tools/npm", "#!/bin/sh\necho npm >> \"$VIA_TEST_LOG\"\nwhile [ \"$1\" != --prefix ]; do shift; done\nshift\nmkdir -p \"$1/node_modules/.bin\"\ncat > \"$1/node_modules/.bin/jscpd\" <<'END'\n#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$VIA_TEST_LOG\"\nexec \"$VIA_TEST_SCANNER\" \"$@\"\nEND\nchmod +x \"$1/node_modules/.bin/jscpd\"\n");
        fs::set_permissions(tools.join("npm"), fs::Permissions::from_mode(0o755)).unwrap();
        let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|p| p.join("git"))
            .find(|p| p.is_file())
            .unwrap();
        put(root, "tools/git", &format!("#!/bin/sh\nif [ \"$1\" = archive ]; then echo archive >> \"$VIA_TEST_LOG\"; fi\nexec '{}' \"$@\"\n", real_git.display()));
        fs::set_permissions(tools.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, tools, scanner: scanner.to_owned() }
    }
    #[track_caller]
    fn run(&self, args: &[&str], expected: i32) -> String {
        let log = self.dir.path().join("calls.txt");
        fs::write(&log, "").unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_via-duplication"))
            .current_dir(self.dir.path())
            .args(args)
            .env("PATH", format!("{}:{}", self.tools.display(), std::env::var("PATH").unwrap()))
            .env("VIA_TEST_SCANNER", &self.scanner)
            .env("VIA_TEST_LOG", &log)
            .output()
            .unwrap();
        eprintln!("native CLI {:?}: expected {expected}, actual {:?}", args, out.status.code());
        assert_eq!(
            out.status.code(),
            Some(expected),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let calls = fs::read_to_string(log).unwrap();
        if args.first() == Some(&"check") && expected != 2 {
            assert_eq!(calls.lines().filter(|l| *l == "npm").count(), 1);
            assert_eq!(calls.lines().filter(|l| *l == "archive").count(), 1);
            assert_eq!(calls.lines().filter(|l| *l == "--version").count(), 1);
            assert_eq!(calls.lines().filter(|l| l.contains("--baseline")).count(), 2);
            assert_eq!(calls.lines().filter(|l| l.contains("--update-baseline")).count(), 1);
        }
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
#[ignore = "installs registry latest once; exercises the real native detector"]
fn real_comparison_and_boundary_contracts() {
    let npm = tempfile::tempdir().unwrap();
    checked(
        Command::new("npm")
            .args([
                "install",
                "--no-save",
                "--no-package-lock",
                "--no-audit",
                "--no-fund",
                "--prefix",
            ])
            .arg(npm.path())
            .arg("jscpd@latest"),
    );
    let scanner = npm.path().join("node_modules/.bin/jscpd");
    let a = "core/lib/via_a/src/lib.rs";
    let b = "via_verifier/bin/b.rs";
    let f = Fixture::new(&scanner, &[a, b]);
    let root = f.dir.path();
    f.run(&["--help"], 0);
    let clean = f.run(&["check"], 0);
    assert!(clean.contains("Blocking exact copies: 0"));
    assert!(clean.contains("untouched old copies (not listed): 1"));
    assert!(!clean.contains("- **old copy"));
    assert!(f.run(&["inventory"], 0).contains(&format!("`{a}:")));
    put(root, a, &format!("// prefix\n\n{}", source(7123)));
    let shifted = f.run(&["check"], 0);
    assert!(shifted.contains("- **old copy in changed file**"));
    assert!(shifted.contains(&format!("`{a}:")));
    assert!(!shifted.contains("&#95;"));
    put(root, a, &source(7123).replace('\n', "\r\n"));
    f.run(&["check"], 0);
    put(root, a, &source(7123).replace("let first", "let  first"));
    f.run(&["check"], 1);
    put(root, a, &source(7123).replace("let first", "// explanation\n    let first"));
    f.run(&["check"], 1);
    put(root, a, &source(7123));
    git(root, &["mv", b, "via_verifier/bin/renamed.rs"]);
    f.run(&["check"], 0);
    put(root, a, &source(8888));
    f.run(&["check"], 1);
    put(root, "via_verifier/bin/renamed.rs", &source(8888));
    f.run(&["check"], 1);
    let shrinking = source(7123)
        .replace("    let sixth = fifth.rotate_left(23);\n", "")
        .replace("let seventh = sixth", "let seventh = fifth");
    put(root, a, &shrinking);
    put(root, "via_verifier/bin/renamed.rs", &shrinking);
    f.run(&["check"], 1);
    put(root, a, &source(7123));
    put(root, "via_verifier/bin/renamed.rs", &source(7123));
    put(root, "via_indexer/tests/third.rs", &source(7123));
    let ordered = f.run(&["check"], 1);
    assert!(
        ordered.find("- **BLOCKING**").unwrap()
            < ordered.find("- **old copy in changed file**").unwrap()
    );
    git(root, &["add", "via_indexer/tests/third.rs"]);
    f.run(&["check"], 1);
    git(root, &["reset", "-q", "HEAD", "--", "via_indexer/tests/third.rs"]);
    for reason in ["intentional test matrix", ""] {
        let ignored = format!(
            "// jscpd:{} {}\n{}// jscpd:{}\n",
            "ignore-start",
            reason,
            source(7123),
            "ignore-end"
        );
        put(root, "via_indexer/tests/third.rs", &ignored);
        f.run(&["check"], 0);
    }
    fs::remove_file(root.join("via_indexer/tests/third.rs")).unwrap();
    put(root, "generated/copy.rs", &source(7123));
    put(root, "target/copy.rs", &source(7123));
    put(root, "ignored/copy.rs", &source(7123));
    // Git archives omit gitlinks; checked-out submodule contents must not create candidate-only copies.
    put(root, "contracts/checked-out.rs", &source(7123));
    put(root, "via-core-ext/checked-out.rs", &source(7123));
    f.run(&["check"], 0);
    git(root, &["update-ref", "-d", "refs/remotes/origin/main"]);
    f.run(&["check"], 2);
    f.run(&["check", "--base", "HEAD"], 0);
    f.run(&["inventory"], 0);
    let mixed = Fixture::new(&scanner, &[a, "core/lib/upstream/src/lib.rs"]);
    for path in [a, "core/lib/upstream/src/lib.rs"] {
        put(mixed.dir.path(), path, &source(9999));
    }
    mixed.run(&["check"], 0);
    let new_mixed = Fixture::new(&scanner, &["core/lib/upstream/src/lib.rs"]);
    put(new_mixed.dir.path(), a, &source(7123));
    new_mixed.run(&["check"], 1);
    let upstream = Fixture::new(&scanner, &["core/lib/first/lib.rs", "core/lib/second/lib.rs"]);
    for path in ["core/lib/first/lib.rs", "core/lib/second/lib.rs"] {
        put(upstream.dir.path(), path, &source(1234));
    }
    upstream.run(&["check"], 0);
    // Depth two contains the merge and its parents, without fetching older history.
    let merge = Fixture::new(&scanner, &[a, b]);
    let merge_root = merge.dir.path();
    let primary = git(merge_root, &["branch", "--show-current"]);
    git(merge_root, &["switch", "-qc", "feature"]);
    put(merge_root, "feature.txt", "feature");
    git(merge_root, &["add", "feature.txt"]);
    git(merge_root, &["commit", "-qm", "feature"]);
    git(merge_root, &["switch", "-q", primary.trim()]);
    put(merge_root, "base.txt", "base");
    git(merge_root, &["add", "base.txt"]);
    git(merge_root, &["commit", "-qm", "base"]);
    git(merge_root, &["merge", "-q", "--no-ff", "feature", "-m", "merge"]);
    let shallow = tempfile::tempdir().unwrap();
    checked(
        Command::new("git")
            .args(["clone", "-q", "--depth", "2", &format!("file://{}", merge_root.display())])
            .arg(shallow.path()),
    );
    assert_eq!(git(shallow.path(), &["rev-parse", "--is-shallow-repository"]).trim(), "true");
    let shallow = Fixture { dir: shallow, tools: merge.tools.clone(), scanner: scanner.clone() };
    shallow.run(&["check", "--base", "HEAD^1"], 0);
    // Preserve the stop's real fixture: the root config must find both protected owners.
    for owner in ["via_reorg", "via_btc_client"] {
        let protected = format!("core/lib/{owner}/src/lib.rs");
        let f = Fixture::new(&scanner, &[&protected, "core/node/via_node/copy.rs"]);
        let other = if owner == "via_reorg" { "via_btc_client" } else { "via_reorg" };
        put(f.dir.path(), &format!("core/lib/{other}/src/lib.rs"), &source(1249));
        let text = f.run(&["boundaries"], 0);
        assert!(text.contains(&format!("reuse-shared-{}: 1 findings", owner.replace('_', "-"))));
        let mut independent = source(6431);
        for (from, to) in [
            ("protected_value", "unrelated_node"),
            ("input", "argument"),
            ("first", "alpha"),
            ("second", "beta"),
            ("third", "gamma"),
            ("fourth", "delta"),
            ("fifth", "epsilon"),
            ("sixth", "zeta"),
            ("seventh", "eta"),
            ("eighth", "theta"),
        ] {
            independent = independent.replace(from, to);
        }
        put(f.dir.path(), "core/node/via_node/copy.rs", &independent);
        f.run(&["boundaries"], 0);
        git(
            f.dir.path(),
            &["add", "core", "via_duplication", "via_duplication_boundaries.json", ".gitignore"],
        );
        git(f.dir.path(), &["commit", "-qm", "renamed fixture"]);
        git(f.dir.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let clean = f.run(&["check"], 0);
        assert!(clean.ends_with("0 findings.\n\n"));
        let inventory = f.run(&["inventory"], 0);
        assert!(inventory.contains("- **renamed**"));
        put(
            f.dir.path(),
            "core/node/via_node/copy.rs",
            &format!("// unrelated prefix\n{independent}"),
        );
        assert!(f.run(&["check"], 0).contains("- **renamed**"));
        let nested = f.dir.path().join("via_duplication/wrong-boundaries.json");
        fs::copy(f.dir.path().join("via_duplication_boundaries.json"), &nested).unwrap();
        let out = checked(
            Command::new(&scanner)
                .current_dir(f.dir.path())
                .arg(f.dir.path())
                .arg("--config")
                .arg(nested)
                .args(["--silent", "--reporters", "json", "--output"])
                .arg(f.dir.path().join("control")),
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("protects no code"));
    }
}

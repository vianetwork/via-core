#!/usr/bin/env python3
import argparse, json, re, subprocess, sys, tempfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]; VERSION = "cpd 5.0.12"
HEADER = "# schema=1 jscpd=5.0.12 format=rust pattern=**/*.rs mode=weak min_tokens=50 min_lines=1 max_lines=100000 max_size=10mb roots=node-lib-v1"
ALLOW_HEADER = "scope\tcore_path\tverifier_path\tmax_tokens\towner\tissue\treason"; ROOTS = {"node": ("core/node", "via_verifier/node"), "lib": ("core/lib", "via_verifier/lib")}
FLAGS = "--format rust --pattern **/*.rs --mode weak --min-lines 1 --min-tokens 50 --max-lines 100000 --max-size 10mb --skip-local --absolute --reporters json --silent --no-tips".split()


def require(condition, message):
    if not condition: raise ValueError(message)


def canonical(scope, core, verifier):
    require(scope in ROOTS, f"unknown scope {scope!r}"); paths = tuple(PurePosixPath(path) for path in (core, verifier)); roots = tuple(PurePosixPath(path) for path in ROOTS[scope])
    require(all(path.as_posix() == raw and not path.is_absolute() and ".." not in path.parts and path.suffix == ".rs" for path, raw in zip(paths, (core, verifier))), "policy paths must be canonical repository-relative Rust paths")
    require(all(path.is_relative_to(root) for path, root in zip(paths, roots)), "policy paths do not match their declared scope")
    return scope, core, verifier


def rows(path, header, width):
    lines = Path(path).read_text(encoding="utf-8").splitlines(); require(lines and lines[0] == header and all(lines[1:]), f"invalid header or blank row in {path}")
    parsed = [line.split("\t") for line in lines[1:]]; require(all(len(row) == width for row in parsed), f"invalid TSV row width in {path}")
    return parsed


def policies(baseline_path, allowlist_path):
    base_rows = rows(baseline_path, HEADER, 4); base_keys = [canonical(*row[:3]) for row in base_rows]
    require(all(row[3].isdigit() for row in base_rows) and base_keys == sorted(set(base_keys)), "baseline rows must be sorted, unique, and nonnegative"); baseline = {key: int(row[3]) for key, row in zip(base_keys, base_rows)}
    allow_rows = rows(allowlist_path, ALLOW_HEADER, 7); allow_keys = [canonical(*row[:3]) for row in allow_rows]
    valid = all(row[3].isdigit() and all(value.strip() for value in row[4:]) for row in allow_rows)
    valid &= allow_keys == sorted(set(allow_keys)) and all(int(row[3]) <= baseline.get(key, -1) for key, row in zip(allow_keys, allow_rows))
    require(valid, "allowlist rows must be sorted, unique, bounded, owned, and justified")
    return baseline, set(allow_keys)


def part(value, roots):
    require(isinstance(value, dict) and isinstance(value.get("name"), str), "malformed clone file object")
    start, end = value.get("start"), value.get("end")
    require(type(start) is int and type(end) is int and 0 < start <= end, "malformed clone line range")
    source = Path(value["name"]); require(source.is_absolute(), "scanner returned a non-absolute path")
    source = source.resolve(strict=True); matches = [side for side, root in enumerate(roots) if source.is_relative_to(root)]; require(len(matches) == 1, f"scanner path is outside or ambiguous for declared roots: {source}")
    return matches[0], source.relative_to(ROOT).as_posix(), start, end


def scan():
    try: version = subprocess.run(["jscpd", "--version"], text=True, capture_output=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError) as error: raise ValueError("install jscpd with `cargo install jscpd --version 5.0.12 --locked`") from error
    require(version == VERSION, f"expected jscpd version {VERSION!r}, got {version!r}; install v5.0.12"); records, debt = [], {}
    with tempfile.TemporaryDirectory(prefix="via-dup-") as temporary:
        for scope, names in ROOTS.items():
            roots = tuple((ROOT / name).resolve(strict=True) for name in names); output = Path(temporary) / scope
            result = subprocess.run(["jscpd", *FLAGS, "--output", str(output), *map(str, roots)], cwd=ROOT, text=True, capture_output=True); detail = result.stderr.strip()[-1000:]; require(result.returncode == 0, f"jscpd {scope} scan exited {result.returncode}{': ' + detail if detail else ''}")
            report = output / "jscpd-report.json"; data = json.loads(report.read_text(encoding="utf-8")); require(isinstance(data, dict) and isinstance(data.get("duplicates"), list), f"malformed duplicate list in {report}")
            for duplicate in data["duplicates"]:
                require(isinstance(duplicate, dict) and duplicate.get("format") == "rust", "malformed clone record or format mismatch")
                tokens, lines = duplicate.get("tokens"), duplicate.get("lines"); require(type(tokens) is int and tokens >= 50 and type(lines) is int and lines >= 1, "malformed clone token or line count")
                first, second = (part(duplicate.get(field), roots) for field in ("firstFile", "secondFile"))
                if first[0] == second[0]: continue
                core, verifier = (first, second) if first[0] == 0 else (second, first); key = canonical(scope, core[1], verifier[1])
                records.append((key, core[2], core[3], verifier[2], verifier[3], tokens, lines)); debt[key] = debt.get(key, 0) + tokens
    return sorted(records), debt


def inventory(records, debt, allowed):
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True, capture_output=True, check=True).stdout.strip()
    print(f"# Via cross-tree duplication inventory\n\n- Commit: `{commit}`\n- Detector: `{VERSION}`\n- Invocation: `jscpd {' '.join(FLAGS)} --output OUT ROOT_A ROOT_B`\n- Roots: `core/node` vs `via_verifier/node`; `core/lib` vs `via_verifier/lib`")
    print("\n## Scope totals\n\n| Scope | Records | Pairs | Duplicate tokens |\n|---|---:|---:|---:|")
    for scope in ROOTS:
        scoped = [row for row in records if row[0][0] == scope]; print(f"| {scope} | {len(scoped)} | {sum(key[0] == scope for key in debt)} | {sum(row[5] for row in scoped)} |")
    print("\n## Pair debt\n\n| Scope | Core path | Verifier path | Tokens | Disposition |\n|---|---|---|---:|---|")
    for key, value in sorted(debt.items(), key=lambda item: (-item[1], item[0])): print(f"| {key[0]} | `{key[1]}` | `{key[2]}` | {value} | {'bounded-allowlist' if key in allowed else 'actionable'} |")
    for title, selected in (("Actionable", False), ("Bounded-allowlist", True)):
        print(f"\n## {title} clone records\n"); matches = [row for row in records if (row[0] in allowed) is selected]
        if not matches: print("_None._")
        for key, cs, ce, vs, ve, tokens, lines in matches: print(f"- `{key[0]}` `{key[1]}:{cs}-{ce}` ↔ `{key[2]}:{vs}-{ve}` - {tokens} tokens, {lines} lines; `{'bounded-allowlist' if selected else 'actionable'}`")
    actionable = [(key, value) for key, value in sorted(debt.items(), key=lambda item: (-item[1], item[0])) if key not in allowed]
    print("\n## Issue #382 actionable pair work-list\n\n" + ("\n".join(f"- [ ] `{key[1]}` ↔ `{key[2]}` - {value} duplicate tokens" for key, value in actionable) or "_None._"))
    metadata = (ROOT / ".github/sibling-paths.yml").read_text(encoding="utf-8"); pair_lines = [line for line in metadata.splitlines() if re.match(r"^\s*-(?:\s+|\[)", line)]
    registered = {tuple(value.strip() for value in pair) for pair in re.findall(r"(?m)^\s*-\s*\[\s*([^,\]\n]+?)\s*,\s*([^,\]\n]+?)\s*\]\s*(?:#.*)?$", metadata)}; require(registered and len(registered) == len(pair_lines) and all(all(pair) for pair in registered), "could not read every registered sibling family")
    families = {tuple("/".join(path.split("/")[:3]) for path in key[1:]) for key, _ in actionable}
    for core, verifier in ROOTS.values():
        shared = {path.name for path in (ROOT / core).iterdir() if path.is_dir()} & {path.name for path in (ROOT / verifier).iterdir() if path.is_dir()}; families |= {(f"{core}/{name}", f"{verifier}/{name}") for name in shared}
    print("\n## Candidate sibling-path additions\n\n" + ("\n".join(f"- `{core}` ↔ `{verifier}`" for core, verifier in sorted(families - registered)) or "_None._"))


def main():
    parser = argparse.ArgumentParser(); parser.add_argument("mode", choices=("inventory", "baseline", "ratchet")); policy = ROOT / ".github/lint/via-structural/duplication"
    parser.add_argument("--baseline", type=Path, default=policy / "baseline.tsv"); parser.add_argument("--allowlist", type=Path, default=policy / "allowlist.tsv")
    args = parser.parse_args(); records, current = scan()
    if args.mode == "baseline":
        print(HEADER, *("\t".join((*key, str(value))) for key, value in sorted(current.items())), sep="\n"); return 0
    baseline, allowed = policies(args.baseline, args.allowlist)
    if args.mode == "inventory": inventory(records, current, allowed); return 0
    for key in sorted(baseline):
        if current.get(key, 0) < baseline[key]: print(f"NOTE: baseline can shrink: {' '.join(key)} old={baseline[key]} new={current.get(key, 0)} delta={current.get(key, 0) - baseline[key]}")
    increases = sorted((key, baseline.get(key, 0), value) for key, value in current.items() if value > baseline.get(key, 0))
    if not increases: print("PASS: cross-tree duplicate token debt did not increase"); return 0
    print("FAIL: cross-tree duplication increased")
    for key, old, new in increases: print(f"{' '.join(key)} old={old} new={new} delta=+{new - old}")
    return 1


try: raise SystemExit(main())
except (OSError, ValueError, subprocess.CalledProcessError) as error:
    print(f"ERROR: scanner/policy failure: {error}", file=sys.stderr); raise SystemExit(2)

#!/usr/bin/env python3
"""Build the refork port inventory for via-core.

Compares the via fork (HEAD) against the fork point (MERGE_BASE) and joins
every diverged path against the upstream refork pin (UPSTREAM_PIN) to record
its "v29 fate". Output is etc/refork/inventory.csv plus a summary on stdout.

Run from the repo root after `git fetch upstream tag core-v29.19.2 --no-tags`:

    python3 etc/refork/build_inventory.py
"""

import csv
import os
import subprocess
import sys
from collections import Counter, defaultdict

MERGE_BASE = "f37b84ac75"
UPSTREAM_PIN = "core-v29.19.2"
OUT_CSV = "etc/refork/inventory.csv"

# Paths whose diffs are tool-generated artifacts, not hand-written changes.
GENERATED_BASENAMES = {"Cargo.lock", "yarn.lock", "package-lock.json", "flake.lock"}
NOISE_PREFIXES = (".github/",)
NOISE_BASENAMES = {"CHANGELOG.md", "renovate.json", ".gitignore", "CODEOWNERS"}
WIRING_BASENAMES = {"Cargo.toml", "justfile", "Makefile", "rust-toolchain", "package.json"}
WIRING_PREFIXES = ("etc/env/", "configs/", "chains/", "docker/", "bin/", "etc/lint-config/")
SUBMODULES = {"contracts", "via-core-ext"}
SMALL_DIFF_LINES = 12  # at or below this, an M to an upstream file is "wiring"


def git(*args):
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def is_via_named(path):
    """Any path segment (or compose file) that names it as via-authored."""
    for seg in path.split("/"):
        if seg.startswith(("via_", "via-", "docker-compose-via", "viaArchitecture", "viaBanner")):
            return True
    return path.startswith("docs/via_guides/")


def via_crate_root(path):
    """Return the via crate directory a path belongs to, or None."""
    parts = path.split("/")
    for i, seg in enumerate(parts[:-1]):
        if seg.startswith(("via_", "via-")) and seg not in ("via_guides",):
            # top-level workspaces keep their inner crate as the unit
            if seg in ("via_verifier", "via_indexer") and len(parts) > i + 2:
                return parts[i + 2]
            return seg
    return None


def port_unit(path):
    crate = via_crate_root(path)
    if crate:
        return crate
    p = path.split("/")
    if p[0] == "core" and len(p) > 2 and p[1] in ("lib", "node", "bin", "tests"):
        return p[2]
    if p[0] in ("zk_toolbox", "zkstack_cli"):
        return "zkstack_cli"
    if p[0] == "prover":
        return "prover"
    if p[0] == "infrastructure" and len(p) > 1:
        return f"infra:{p[1]}"
    if p[0] in ("etc", "configs", "chains") or path.startswith("docker"):
        return "config+docker"
    if p[0] == "docs":
        return "docs"
    if p[0] == ".github":
        return "ci"
    return p[0]


def subclass_b(path, changed):
    base = os.path.basename(path)
    if path.split("/")[0] in SUBMODULES:
        return "submodule-pointer"
    if "/.sqlx/" in path:
        return "generated"
    if base in GENERATED_BASENAMES:
        return "generated"
    if path.startswith(NOISE_PREFIXES) or base in NOISE_BASENAMES or base.endswith(".snap"):
        return "noise"
    if base in WIRING_BASENAMES or path.startswith(WIRING_PREFIXES):
        return "wiring"
    if changed <= SMALL_DIFF_LINES:
        return "wiring"
    return "feature"


def main():
    os.makedirs(os.path.dirname(OUT_CSV), exist_ok=True)
    rng = f"{MERGE_BASE}..HEAD"
    name_status = git("-c", "diff.renameLimit=4000", "diff", "--name-status", "-M", rng)
    numstat = git("-c", "diff.renameLimit=4000", "diff", "--numstat", "-M", rng)
    def tree_blobs(rev):
        out = {}
        for line in git("ls-tree", "-r", rev).splitlines():
            meta, path = line.split("\t", 1)
            out[path] = meta.split()[2]  # blob hash
        return out

    v29_blobs = tree_blobs(UPSTREAM_PIN)
    head_blobs = tree_blobs("HEAD")
    v29_tree = set(v29_blobs)
    v29_by_base = defaultdict(list)
    for p in v29_tree:
        v29_by_base[os.path.basename(p)].append(p)

    loc = {}
    for line in numstat.splitlines():
        add, dele, rest = line.split("\t", 2)
        if "=>" in rest:  # rename: "old => new" possibly with {…} braces
            rest = rest.split("\t")[-1]
        loc[rest] = (add, dele)

    def fate(old_path, new_path):
        # via's final content already equals upstream v29 -> the diff was a
        # backport of upstream work; nothing to port after the refork.
        if v29_blobs.get(new_path) is not None and v29_blobs.get(new_path) == head_blobs.get(new_path):
            return "identical-to-v29"
        path = old_path or new_path
        if path in v29_tree:
            return "present"
        cands = v29_by_base.get(os.path.basename(path))
        if cands:
            return f"moved?:{cands[0]}"
        return "gone"

    rows = []
    for line in name_status.splitlines():
        parts = line.split("\t")
        status = parts[0]
        if status.startswith("R"):
            old_path, path = parts[1], parts[2]
        else:
            old_path, path = "", parts[1]
        add, dele = loc.get(path, loc.get(f"{old_path} => {path}", ("0", "0")))
        try:
            changed = int(add) + int(dele)
        except ValueError:  # binary files report "-"
            changed = 0

        if status == "A" and path.endswith(".sql") and "/migrations/" in path:
            cat, sub = "D", "via-migration"
        elif status == "A":
            if "/.sqlx/" in path:
                cat, sub = "A", "generated"
            elif via_crate_root(path):
                cat, sub = "A", "via-crate"
            elif is_via_named(path):
                cat, sub = "A", "via-named-embedded"
            else:
                cat, sub = "A", "unmarked-addition"
        elif status == "D":
            cat, sub = "C", "deleted-upstream-file"
        else:  # M or R*
            cat = "B"
            sub = subclass_b(path, changed)

        # Fate matters for modified/deleted upstream files, and also exposes
        # backported additions (via-added file that exists verbatim in v29).
        f = fate(old_path, path) if cat in ("B", "C") or sub == "unmarked-addition" else ""
        rows.append(
            dict(path=path, old_path=old_path, status=status, category=cat,
                 subclass=sub, v29_fate=f, loc_added=add, loc_deleted=dele,
                 port_unit=port_unit(path))
        )

    with open(OUT_CSV, "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=list(rows[0].keys()))
        w.writeheader()
        w.writerows(rows)

    summary = Counter((r["category"], r["subclass"]) for r in rows)
    print(f"{len(rows)} paths -> {OUT_CSV}")
    for (cat, sub), n in sorted(summary.items()):
        print(f"  {cat:>2} {sub:<22} {n}")
    fates = Counter(r["v29_fate"].split(":")[0] for r in rows if r["v29_fate"])
    print("v29 fate (B+C):", dict(fates))


if __name__ == "__main__":
    main()

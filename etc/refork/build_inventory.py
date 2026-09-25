#!/usr/bin/env python3
"""Build the refork port inventory for via-core.

Compares the via fork (HEAD) against the fork point (MERGE_BASE) and joins
every diverged path against the upstream refork pin (UPSTREAM_PIN) to record
its "v29 fate". Output is etc/refork/inventory.csv plus a summary on stdout.

Run from the repo root after `git fetch upstream tag core-v29.20.0 --no-tags`:

    python3 etc/refork/build_inventory.py
"""

import csv
import os
import re
import subprocess
from collections import Counter, defaultdict

MERGE_BASE = "f37b84ac75"
UPSTREAM_PIN = os.environ.get("REFORK_PIN", "core-v29.20.0")
OUT_CSV = "etc/refork/inventory.csv"

# Paths whose diffs are tool-generated artifacts, not hand-written changes.
GENERATED_BASENAMES = {"Cargo.lock", "yarn.lock", "package-lock.json", "flake.lock"}
NOISE_PREFIXES = (".github/",)
NOISE_BASENAMES = {"CHANGELOG.md", "renovate.json", ".gitignore", "CODEOWNERS"}
WIRING_BASENAMES = {"Cargo.toml", "justfile", "Makefile", "rust-toolchain", "package.json"}
WIRING_PREFIXES = ("etc/env/", "configs/", "chains/", "docker/", "bin/", "etc/lint-config/")
SUBMODULES = {"contracts", "via-core-ext"}
# Phase 0 artifacts themselves; not port surface.
EXCLUDE_PREFIXES = ("docs/refork/", "etc/refork/")
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


CRATE_NAMES = {}  # via crate dir -> Cargo.toml package name; filled in main()


def via_crate_root(path):
    """Return the via crate unit a path belongs to, or None.

    A unit is only a crate if its directory has a Cargo.toml with a package
    name (tooling/CI/docker dirs that merely contain "via" are not crates),
    and the unit key is that package name so it matches the docs' waves."""
    parts = path.split("/")
    for i, seg in enumerate(parts[:-1]):
        if seg.startswith(("via_", "via-")) and seg not in ("via_guides",):
            # top-level workspaces keep their inner crate as the unit
            if seg in ("via_verifier", "via_indexer") and len(parts) > i + 2:
                return CRATE_NAMES.get("/".join(parts[: i + 3]))
            return CRATE_NAMES.get("/".join(parts[: i + 1]))
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
    base_blobs = tree_blobs(MERGE_BASE)
    for p in head_blobs:
        if p.endswith("/Cargo.toml") and is_via_named(p):
            m = re.search(r'^name\s*=\s*"([^"]+)"', git("show", f"HEAD:{p}"), re.M)
            if m:
                CRATE_NAMES[p[: -len("/Cargo.toml")]] = m.group(1)
    v29_tree = set(v29_blobs)
    v29_by_blob = defaultdict(list)
    for p, h in v29_blobs.items():
        v29_by_blob[h].append(p)
    v29_by_base = defaultdict(list)
    for p in sorted(v29_tree):
        v29_by_base[os.path.basename(p)].append(p)

    def resolve_renamed(p):
        """numstat -M prints renames brace-compressed ('pre{old => new}post')
        or plain ('old => new'); resolve to the post-rename path."""
        if " => " not in p:
            return p
        if "{" in p and "}" in p:
            prefix, rest = p.split("{", 1)
            rename, suffix = rest.split("}", 1)
            _, new = rename.split(" => ", 1)
            return (prefix + new + suffix).replace("//", "/")
        return p.split(" => ", 1)[1]

    loc = {}
    for line in numstat.splitlines():
        add, dele, rest = line.split("\t", 2)
        loc[resolve_renamed(rest)] = (add, dele)

    def fate(old_path, new_path):
        # via's final content already equals upstream v29 -> the diff was a
        # backport of upstream work; nothing to port after the refork.
        if v29_blobs.get(new_path) is not None and v29_blobs.get(new_path) == head_blobs.get(new_path):
            return "identical-to-v29"
        # the post-rename path existing in v29 beats old-path lineage.
        if new_path in v29_tree:
            return "present"
        path = old_path or new_path
        if path in v29_tree:
            return "present"
        # exact-content move: the file's pre-fork (or current) blob exists
        # verbatim at another v29 path.
        hits = v29_by_blob.get(base_blobs.get(path) or head_blobs.get(new_path))
        if hits:
            return f"moved:{min(hits)}"
        # a basename match is only a hint; ambiguous names stay unresolved.
        cands = v29_by_base.get(os.path.basename(path))
        if cands and len(cands) == 1:
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
        if path.startswith(EXCLUDE_PREFIXES):
            continue
        add, dele = loc.get(path, ("0", "0"))
        try:
            changed = int(add) + int(dele)
        except ValueError:  # binary files report "-"
            changed = 0

        if status == "A" and path.endswith(".sql") and "/migrations/" in path:
            # via's history cherry-picked upstream migrations; only via-owned
            # ones are port work, backports already sit in the v29 schema.
            # Path-level match only: generic down-script placeholders blob-match
            # unrelated migrations.
            if path in v29_tree:
                cat, sub = "A", "backported-migration"
            else:
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
        needs_fate = cat in ("B", "C", "D") or sub in ("unmarked-addition", "backported-migration")
        f = fate(old_path, path) if needs_fate else ""
        rows.append(
            dict(path=path, old_path=old_path, status=status, category=cat,
                 subclass=sub, v29_fate=f, loc_added=add, loc_deleted=dele,
                 port_unit=port_unit(path))
        )

    fieldnames = ["path", "old_path", "status", "category", "subclass",
                  "v29_fate", "loc_added", "loc_deleted", "port_unit"]
    with open(OUT_CSV, "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=fieldnames, lineterminator="\n")
        w.writeheader()
        w.writerows(rows)

    summary = Counter((r["category"], r["subclass"]) for r in rows)
    print(f"{len(rows)} paths -> {OUT_CSV}")
    for (cat, sub), n in sorted(summary.items()):
        print(f"  {cat:>2} {sub:<22} {n}")
    fates = Counter(r["v29_fate"].split(":")[0] for r in rows if r["v29_fate"])
    print("v29 fate:", dict(fates))


if __name__ == "__main__":
    main()

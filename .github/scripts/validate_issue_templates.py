#!/usr/bin/env python3
"""Validate GitHub issue forms and their repo-local source references.

This intentionally stays lightweight: no Docker, no Rust build, no network.
Every component/source claim in issue forms should either resolve in this
checkout or be declared here as an intentional component label.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Any

import yaml

ROOT = Path(__file__).resolve().parents[2]
ISSUE_TEMPLATE_DIR = ROOT / ".github" / "ISSUE_TEMPLATE"

FORM_ID_RE = re.compile(r"^[A-Za-z0-9_-]+$")
FORBIDDEN_FIELD_LABEL_RE = re.compile(r"\b(password|passwd|token|secret|credential|private key)\b", re.IGNORECASE)

ALLOWED_FORM_KEYS = {
    "assignees",
    "body",
    "description",
    "labels",
    "name",
    "projects",
    "title",
    "type",
}

ALLOWED_BODY_KEYS = {"attributes", "id", "type", "validations"}

ALLOWED_ATTRIBUTE_KEYS_BY_TYPE = {
    "checkboxes": {"description", "label", "options"},
    "dropdown": {"default", "description", "label", "multiple", "options"},
    "input": {"description", "label", "placeholder", "value"},
    "markdown": {"value"},
    "textarea": {"description", "label", "placeholder", "render", "value"},
    "upload": {"description", "label"},
}

ALLOWED_VALIDATION_KEYS = {"required"}

ALLOWED_FORM_TYPES = {
    "checkboxes",
    "dropdown",
    "input",
    "markdown",
    "textarea",
    "upload",
}

# Script-local component registry. Values are repo paths that must exist. An
# empty list means the entry is an intentional non-path label.
COMPONENT_LABELS: dict[str, list[str]] = {
    # L1 / BTC / reorg source paths.
    "core/node/via_main_node_reorg_detector": ["core/node/via_main_node_reorg_detector"],
    "via_verifier/node/via_reorg_detector": ["via_verifier/node/via_reorg_detector"],
    "core/node/via_btc_watch": ["core/node/via_btc_watch"],
    "via_verifier/node/via_btc_watch": ["via_verifier/node/via_btc_watch"],
    "core/node/via_btc_sender": ["core/node/via_btc_sender"],
    "via_verifier/node/via_btc_sender": ["via_verifier/node/via_btc_sender"],
    "core/lib/via_btc_client": ["core/lib/via_btc_client"],
    "core/lib/dal/src/via_l1_block_dal.rs": ["core/lib/dal/src/via_l1_block_dal.rs"],
    "via_verifier/lib/verifier_dal/src/via_l1_block_dal.rs": [
        "via_verifier/lib/verifier_dal/src/via_l1_block_dal.rs"
    ],
    "core/lib/config via_btc_* / via_reorg_detector": ["core/lib/config"],
    "deployment / kube-state / helm values": [],
    "deployment/kube-state or helm-charts desired state": [],
    "external-node bootstrap": [],
    "not sure": [],
    # Operator evidence component labels.
    "Main node / runtime": ["core/node"],
    "External node": ["core/node"],
    "Verifier": ["via_verifier"],
    "Prover": ["prover"],
    "BTC sender": ["core/node/via_btc_sender"],
    "BTC watch": ["core/node/via_btc_watch"],
    "BTC client": ["core/lib/via_btc_client"],
    "DA / Celestia": ["core/node/via_da_dispatcher"],
    "Database / migrations": ["core/lib/dal", "core/lib/storage"],
    "zkstack_cli": ["zkstack_cli"],
    "Docker / bootstrap": ["docker"],
    "Kubernetes / Helm / deployment": [],
    "Docs / operator guide": ["docs"],
    "Not sure": [],
}

REQUIRED_CONCEPTS: dict[str, tuple[str, ...]] = {
    "raw values": ("raw observed values", "raw values", "raw evidence"),
    "value provenance": ("where each value came from", "provenance", "source / command"),
    "facts vs hypotheses": ("proven versus inferred", "proven or inferred", "hypotheses"),
    "invariant / contract": ("invariant", "contract"),
    "ordering / identity assumptions": ("ordering / identity", "ordering", "identity"),
    "regression-test expectation": ("regression test", "fail before", "pass after"),
    "live read/write boundary": (
        "live read/write boundary",
        "read-only live checks",
        "live changes were run",
        "live infrastructure",
    ),
    "secret redaction": ("do not include secrets", "redact", "credentials"),
}


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def repo_path_exists(path: str) -> bool:
    return (ROOT / path).exists()


def validate_component_label(errors: list[str], label: str, where: str) -> None:
    label = label.strip().strip("`")
    if not label:
        return

    if repo_path_exists(label):
        return

    if label in COMPONENT_LABELS:
        for mapped_path in COMPONENT_LABELS[label]:
            if not repo_path_exists(mapped_path):
                fail(errors, f"{where}: registry target for '{label}' does not exist: {mapped_path}")
        return

    fail(
        errors,
        f"{where}: '{label}' is neither an existing path nor a registered component label",
    )


def normalize_label(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", value.strip().lower()).strip("-")


def fail_duplicate(errors: list[str], values: list[str], where: str) -> None:
    seen: set[str] = set()
    for value in values:
        normalized = normalize_label(value)
        if normalized in seen:
            fail(errors, f"{where}: duplicate option value: {value!r}")
        seen.add(normalized)


def load_yaml(errors: list[str], path: Path) -> Any | None:
    try:
        doc = yaml.safe_load(path.read_text())
    except Exception as exc:  # noqa: BLE001 - show parse error clearly in CI
        fail(errors, f"{path.relative_to(ROOT)}: YAML parse failed: {exc}")
        return None
    if doc is None:
        fail(errors, f"{path.relative_to(ROOT)}: YAML document must not be empty")
    return doc


def body_text(form: dict[str, Any]) -> str:
    chunks: list[str] = []
    for item in form.get("body", []):
        attrs = item.get("attributes", {}) if isinstance(item, dict) else {}
        for key in ("label", "description", "placeholder", "value"):
            value = attrs.get(key)
            if isinstance(value, str):
                chunks.append(value)
    return "\n".join(chunks)


def table_first_column_values(markdown: str) -> list[str]:
    values: list[str] = []
    in_table = False
    for line in markdown.splitlines():
        stripped = line.strip()
        if not stripped.startswith("|"):
            in_table = False
            continue
        cells = [cell.strip() for cell in re.split(r"(?<!\\)\|", stripped.strip("|"))]
        if not cells:
            continue
        if all(set(cell) <= {"-", ":"} for cell in cells if cell):
            continue
        if not in_table:
            in_table = True
            continue
        if cells[0]:
            values.append(cells[0])
    return values


def validate_form(errors: list[str], path: Path, form: dict[str, Any]) -> None:
    rel = path.relative_to(ROOT)
    for key in form:
        if key not in ALLOWED_FORM_KEYS:
            fail(errors, f"{rel}: unsupported top-level key '{key}'")
    for key in ("name", "description", "title", "labels", "body"):
        if key not in form:
            fail(errors, f"{rel}: missing required top-level key '{key}'")

    for key in ("name", "description", "title"):
        value = form.get(key)
        if not isinstance(value, str) or not value.strip():
            fail(errors, f"{rel}: top-level key '{key}' must be a non-empty string")
    labels = form.get("labels")
    if not isinstance(labels, list) or not labels or not all(isinstance(label, str) and label.strip() for label in labels):
        fail(errors, f"{rel}: top-level key 'labels' must be a non-empty list of strings")

    body = form.get("body")
    if not isinstance(body, list) or not body:
        fail(errors, f"{rel}: body must be a non-empty list")
        return

    seen_ids: set[str] = set()
    seen_labels: set[str] = set()
    non_markdown_items = 0
    for index, item in enumerate(body):
        where = f"{rel}: body[{index}]"
        if not isinstance(item, dict):
            fail(errors, f"{where}: item must be a mapping")
            continue
        for key in item:
            if key not in ALLOWED_BODY_KEYS:
                fail(errors, f"{where}: unsupported body key '{key}'")
        item_type = item.get("type")
        if item_type not in ALLOWED_FORM_TYPES:
            fail(errors, f"{where}: unsupported type {item_type!r}")
        attrs = item.get("attributes")
        if not isinstance(attrs, dict):
            fail(errors, f"{where}: attributes must be present")
            attrs = {}
        for key in attrs:
            if key not in ALLOWED_ATTRIBUTE_KEYS_BY_TYPE.get(str(item_type), set()):
                fail(errors, f"{where}: unsupported attribute for {item_type!r}: '{key}'")
        validations = item.get("validations")
        if isinstance(validations, dict):
            for key in validations:
                if key not in ALLOWED_VALIDATION_KEYS:
                    fail(errors, f"{where}: unsupported validation key '{key}'")
            if "required" in validations and not isinstance(validations["required"], bool):
                fail(errors, f"{where}: validations.required must be a boolean")
        item_id = item.get("id")
        label = attrs.get("label")
        if item_type == "markdown":
            value = attrs.get("value")
            if not isinstance(value, str) or not value.strip():
                fail(errors, f"{where}: markdown blocks must include a non-empty attributes.value")
        elif not isinstance(label, str) or not label.strip():
            fail(errors, f"{where}: non-markdown item must have a non-empty attributes.label")
        else:
            normalized_label = normalize_label(label)
            if normalized_label in seen_labels:
                fail(errors, f"{where}: duplicate field label: {label!r}")
            seen_labels.add(normalized_label)
            if item_type in {"input", "textarea"} and FORBIDDEN_FIELD_LABEL_RE.search(label):
                fail(errors, f"{where}: input/textarea label contains a forbidden credential word: {label!r}")
        if item_type != "markdown":
            non_markdown_items += 1
            if not isinstance(item_id, str) or not item_id:
                fail(errors, f"{where}: non-markdown item missing id")
            else:
                if not FORM_ID_RE.fullmatch(item_id):
                    fail(errors, f"{where}: id contains unsupported characters: {item_id!r}")
                if item_id in seen_ids:
                    fail(errors, f"{where}: duplicate field id: {item_id!r}")
                seen_ids.add(item_id)
        if item_type in {"input", "textarea", "dropdown", "upload"}:
            if not isinstance(validations, dict) or "required" not in validations:
                fail(errors, f"{where}: field should explicitly set validations.required")
        if item_type == "checkboxes":
            options = attrs.get("options", []) if isinstance(attrs, dict) else []
            if not isinstance(options, list) or not options:
                fail(errors, f"{where}: checkbox options must be a non-empty list")
                options = []
            labels: list[str] = []
            for option_index, option in enumerate(options):
                if not isinstance(option, dict):
                    fail(errors, f"{where}: checkbox option[{option_index}] must be a mapping")
                    continue
                label = option.get("label")
                if not isinstance(label, str) or not label.strip():
                    fail(errors, f"{where}: checkbox option[{option_index}] must have a non-empty label")
                else:
                    labels.append(label)
                if option.get("required") is not True:
                    fail(errors, f"{where}: checkbox option[{option_index}] should be required")
            fail_duplicate(errors, labels, where)

        if item_type == "dropdown":
            options = attrs.get("options", []) if isinstance(attrs, dict) else []
            if not isinstance(options, list) or not options:
                fail(errors, f"{where}: dropdown options must be a non-empty list")
                options = []
            string_options: list[str] = []
            for option in options:
                if not isinstance(option, str):
                    fail(errors, f"{where}: dropdown option must be a string: {option!r}")
                    continue
                if not option.strip():
                    fail(errors, f"{where}: dropdown option must not be empty")
                    continue
                if option.strip().lower() in {"none", "n/a"}:
                    fail(errors, f"{where}: dropdown option uses a GitHub-reserved value: {option!r}")
                string_options.append(option)
                if item_id in {"affected_path", "affected_area"}:
                    validate_component_label(errors, option, f"{where}: dropdown option")
            fail_duplicate(errors, string_options, where)
            if "multiple" in attrs and not isinstance(attrs["multiple"], bool):
                fail(errors, f"{where}: dropdown attributes.multiple must be a boolean")
            if "default" in attrs:
                default = attrs["default"]
                if not isinstance(default, int) or isinstance(default, bool):
                    fail(errors, f"{where}: dropdown attributes.default must be an integer option index")
                elif not 0 <= default < len(string_options):
                    fail(errors, f"{where}: dropdown attributes.default index is out of range")

        if item_type == "textarea" and item_id in {"source_surfaces_checked", "source_references"}:
            value = attrs.get("value", "") if isinstance(attrs, dict) else ""
            for label in table_first_column_values(value):
                validate_component_label(errors, label, f"{where}: {item_id}")

    if non_markdown_items == 0:
        fail(errors, f"{rel}: form must include at least one non-markdown field")


def validate_config(errors: list[str], path: Path, config: dict[str, Any]) -> None:
    rel = path.relative_to(ROOT)
    if config.get("blank_issues_enabled") is not False:
        fail(errors, f"{rel}: blank_issues_enabled must remain false")
    links = config.get("contact_links")
    if not isinstance(links, list) or not links:
        fail(errors, f"{rel}: contact_links must be present for non-bug paths")
        return
    names = [str(link.get("name", "")).lower() for link in links if isinstance(link, dict)]
    patterns = {
        "security": re.compile(r"\bsecurity\b"),
        "feature": re.compile(r"\bfeature\b"),
        "question": re.compile(r"\bquestions?\b"),
    }
    for expected, pattern in patterns.items():
        if not any(pattern.search(name) for name in names):
            fail(errors, f"{rel}: missing contact link covering {expected!r}")


def validate_required_concepts(errors: list[str], forms: list[tuple[Path, dict[str, Any]]]) -> None:
    for path, form in forms:
        if path.name == "bug_report.yml":
            continue
        lower = " ".join(body_text(form).lower().split())
        for concept, needles in REQUIRED_CONCEPTS.items():
            if not any(" ".join(needle.lower().split()) in lower for needle in needles):
                fail(errors, f"{path.relative_to(ROOT)}: required evidence concept not found: {concept}")


def main() -> int:
    errors: list[str] = []

    if not ISSUE_TEMPLATE_DIR.exists():
        fail(errors, f"missing issue template directory: {ISSUE_TEMPLATE_DIR.relative_to(ROOT)}")
    else:
        for md_path in sorted(ISSUE_TEMPLATE_DIR.glob("*.md")):
            if md_path.name.lower() == "readme.md":
                continue
            fail(errors, f"legacy Markdown issue template should not be present: {md_path.relative_to(ROOT)}")
        for yaml_path in sorted(ISSUE_TEMPLATE_DIR.glob("*.yaml")):
            fail(errors, f"issue forms must use the .yml extension: {yaml_path.relative_to(ROOT)}")

    form_docs: list[tuple[Path, dict[str, Any]]] = []
    form_names: dict[str, Path] = {}
    config_seen = False
    for path in sorted(ISSUE_TEMPLATE_DIR.glob("*.yml")):
        doc = load_yaml(errors, path)
        if doc is None:
            continue
        if not isinstance(doc, dict):
            fail(errors, f"{path.relative_to(ROOT)}: YAML root must be a mapping")
            continue
        if path.name == "config.yml":
            config_seen = True
            validate_config(errors, path, doc)
        else:
            validate_form(errors, path, doc)
            name = doc.get("name")
            if isinstance(name, str):
                normalized_name = name.strip().lower()
                if normalized_name in form_names:
                    fail(
                        errors,
                        f"{path.relative_to(ROOT)}: duplicate issue form name also used by "
                        f"{form_names[normalized_name].relative_to(ROOT)}: {name!r}",
                    )
                else:
                    form_names[normalized_name] = path
            form_docs.append((path, doc))

    if not config_seen:
        fail(errors, ".github/ISSUE_TEMPLATE/config.yml must be present")
    if not form_docs:
        fail(errors, "at least one non-config issue form must be present")

    validate_required_concepts(errors, form_docs)

    if errors:
        print("Issue template validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    print("Issue template validation passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Inventory Rust callables for the backend maintainability review.

This is a dependency-free structural inventory, not a Rust parser or a review.
It masks comments and literals while preserving offsets, records concrete functions
and owned-work closures, and deliberately emits every discovered row as pending.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any, Iterable

SCHEMA_VERSION = 1
REVIEW_ARRAYS = (
    "callers", "inputs", "sideEffects", "errors", "ownership", "locks",
    "invariants", "tests", "evidence",
)
REVIEW_FIELDS = (
    "purpose", *REVIEW_ARRAYS, "output", "cancellation", "findingStatus",
    "disposition", "destination", "reviewer", "reviewedAt", "status", "notes",
)
OWNING_CALLS = {
    "spawn", "spawn_blocking", "spawn_cleanup_continuation", "spawn_tracked",
    "spawn_startup",
}


def mask_rust(text: str) -> str:
    """Mask comments/string/char contents with spaces, retaining newlines/offsets."""
    out = list(text)
    n = len(text)
    i = 0

    def blank(start: int, end: int) -> None:
        for pos in range(start, end):
            if out[pos] not in "\r\n":
                out[pos] = " "

    while i < n:
        if text.startswith("//", i):
            end = text.find("\n", i + 2)
            if end < 0:
                end = n
            blank(i, end)
            i = end
            continue
        if text.startswith("/*", i):
            start = i
            depth = 1
            i += 2
            while i < n and depth:
                if text.startswith("/*", i):
                    depth += 1
                    i += 2
                elif text.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            blank(start, i)
            continue

        raw = re.match(r"(?:b?r)(#{0,255})\"", text[i:])
        if raw:
            hashes = raw.group(1)
            start = i
            i += raw.end()
            terminator = '"' + hashes
            end = text.find(terminator, i)
            i = n if end < 0 else end + len(terminator)
            blank(start, i)
            continue

        prefix = 0
        if text.startswith('b"', i) or text.startswith('c"', i):
            prefix = 1
        if text[i + prefix:i + prefix + 1] == '"':
            start = i
            i += prefix + 1
            escaped = False
            while i < n:
                char = text[i]
                i += 1
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == '"':
                    break
            blank(start, i)
            continue

        # A Rust character literal has a closing quote close by. Lifetimes do not.
        if text[i] == "'" or (text.startswith("b'", i)):
            prefix = 1 if text.startswith("b'", i) else 0
            quote = i + prefix
            cursor = quote + 1
            escaped = False
            closing = -1
            while cursor < min(n, quote + 16):
                char = text[cursor]
                if char in "\r\n":
                    break
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == "'":
                    closing = cursor
                    break
                cursor += 1
            if closing >= 0:
                blank(i, closing + 1)
                i = closing + 1
                continue
        i += 1
    return "".join(out)


TOKEN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*|::|->|=>|\.\.|[^\s]")


def tokens(masked: str) -> list[tuple[str, int, int]]:
    return [(match.group(0), match.start(), match.end()) for match in TOKEN_RE.finditer(masked)]


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def normalized_digest(fragment: str) -> str:
    normalized = fragment.replace("\r\n", "\n").replace("\r", "\n")
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()


def collapse(fragment: str) -> str:
    return re.sub(r"\s+", " ", fragment).strip()


def delimiter_pairs(stream: list[tuple[str, int, int]], opening_value: str, closing_value: str) -> dict[int, int]:
    stack: list[int] = []
    forward: dict[int, int] = {}
    for index, (value, _, _) in enumerate(stream):
        if value == opening_value:
            stack.append(index)
        elif value == closing_value and stack:
            opening = stack.pop()
            forward[opening] = index
    return forward


def previous_boundary(stream: list[tuple[str, int, int]], index: int) -> int:
    depth = 0
    cursor = index - 1
    while cursor >= 0:
        value = stream[cursor][0]
        if value in ("}", ")", "]"):
            depth += 1
        elif value in ("{", "(", "["):
            if depth:
                depth -= 1
            elif value == "{":
                break
        elif depth == 0 and value == ";":
            break
        cursor -= 1
    return cursor + 1


def attributes_before(masked: str, offset: int) -> str:
    window = masked[max(0, offset - 4000):offset]
    match = re.search(
        r"((?:#\s*\[[^\]]*\]\s*)+)"
        r"(?:(?:pub(?:\s*\([^)]*\))?|async|unsafe|const|default|extern)\s*)*$",
        window,
        re.S,
    )
    return match.group(1) if match else ""


def cfgs_before(text: str, masked: str, offset: int) -> list[str]:
    del text
    return [
        collapse(item)
        for item in re.findall(r"#\s*\[\s*cfg\s*\((.*?)\)\s*\]", attributes_before(masked, offset), re.S)
    ]


def find_header_end(stream: list[tuple[str, int, int]], start: int) -> tuple[int | None, str | None]:
    paren = bracket = angle = 0
    for index in range(start, len(stream)):
        value = stream[index][0]
        if value == "(": paren += 1
        elif value == ")": paren = max(0, paren - 1)
        elif value == "[": bracket += 1
        elif value == "]": bracket = max(0, bracket - 1)
        elif value == "<": angle += 1
        elif value == ">" and angle: angle -= 1
        elif paren == 0 and bracket == 0 and angle == 0 and value in ("{", ";"):
            return index, value
    return None, None


def classify(path: str, name: str, cfg: Iterable[str], attributes: str, containers: list[dict[str, Any]]) -> str:
    joined_cfg = " ".join(cfg)
    module_names = {container.get("name", "") for container in containers if container["kind"] == "module"}
    if re.search(r"#\s*\[\s*(?:tokio::)?test(?:\s*\(|\s*\])", attributes):
        return "test"
    if path.endswith("backend_lifecycle_tests.rs") or name.startswith("test_"):
        return "test"
    if path.endswith(("performance_harness.rs", "soak_harness.rs")):
        return "test_support"
    if "test" in joined_cfg or "tests" in module_names or name.endswith("_for_test") or name.startswith("assert_"):
        return "test_support"
    return "production"


def empty_review() -> dict[str, Any]:
    review: dict[str, Any] = {field: [] for field in REVIEW_ARRAYS}
    review.update({
        "purpose": None, "output": None, "cancellation": None,
        "findingStatus": None, "disposition": None, "destination": None,
        "reviewer": None, "reviewedAt": None, "status": "pending", "notes": None,
    })
    return review


def scan_file(root: Path, path: Path) -> list[dict[str, Any]]:
    text = path.read_text(encoding="utf-8")
    masked = mask_rust(text)
    stream = tokens(masked)
    pairs = delimiter_pairs(stream, "{", "}")
    paren_pairs = delimiter_pairs(stream, "(", ")")
    relative = path.relative_to(root).as_posix()
    containers: list[dict[str, Any]] = []

    for index, (value, start, _) in enumerate(stream):
        if value not in ("mod", "impl", "fn", "extern"):
            continue
        header_end, terminator = find_header_end(stream, index + 1)
        if header_end is None or terminator != "{" or header_end not in pairs:
            continue
        end_index = pairs[header_end]
        if value == "mod" and index + 1 < len(stream):
            name = stream[index + 1][0]
            kind = "module"
        elif value == "impl":
            name = collapse(masked[stream[index][1]:stream[header_end][1]])
            kind = "impl"
        elif value == "fn":
            name = stream[index + 1][0] if index + 1 < len(stream) else "anonymous"
            kind = "function"
        else:
            name = collapse(masked[stream[index][1]:stream[header_end][1]])
            kind = "extern"
        containers.append({
            "kind": kind, "name": name, "token": index, "body": header_end,
            "end": end_index, "startOffset": start,
            "cfg": cfgs_before(text, masked, start),
        })

    entries: list[dict[str, Any]] = []
    function_ranges: list[dict[str, Any]] = []
    for index, (value, start, _) in enumerate(stream):
        if value != "fn" or index + 1 >= len(stream):
            continue
        name = stream[index + 1][0]
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
            # Function-pointer types use `fn(...)` and do not declare a callable.
            continue
        header_end, terminator = find_header_end(stream, index + 2)
        if header_end is None or terminator not in ("{", ";"):
            continue
        end_index = pairs.get(header_end, header_end)
        end_offset = stream[end_index][2]
        start_line_offset = text.rfind("\n", 0, start) + 1
        enclosing = [
            container for container in containers
            if container["body"] < index < container["end"] and container["token"] != index
        ]
        enclosing.sort(key=lambda item: item["body"])
        inherited_cfg = [cfg for container in enclosing for cfg in container["cfg"]]
        own_cfg = cfgs_before(text, masked, start)
        cfg = list(dict.fromkeys(inherited_cfg + own_cfg))
        impls = [container for container in enclosing if container["kind"] == "impl"]
        functions = [container for container in enclosing if container["kind"] == "function"]
        modules = [container["name"] for container in enclosing if container["kind"] == "module"]
        qualifier = "::".join(modules)
        externs = [container for container in enclosing if container["kind"] == "extern"]
        kind = "free_function"
        if functions:
            kind = "local_function"
            qualifier = "::".join(filter(None, (qualifier, functions[-1]["name"], "local")))
        elif impls:
            impl_name = impls[-1]["name"]
            qualifier = "::".join(filter(None, (qualifier, impl_name)))
            if re.search(r"\bDrop\s+for\b", impl_name) and name == "drop":
                kind = "destructor"
            elif re.search(r"\bfor\b", impl_name):
                kind = "trait_method"
            else:
                kind = "method"
        if terminator == ";":
            if externs:
                kind = "ffi_declaration"
            elif kind == "trait_method":
                kind = "trait_method_declaration"
            else:
                kind = "function_declaration"
        qualified = "::".join(filter(None, (qualifier, name)))
        attributes = attributes_before(masked, start)
        classification = classify(relative, name, cfg, attributes, enclosing)
        signature_end = stream[header_end][1]
        signature = collapse(masked[start_line_offset:signature_end])
        fragment = text[start_line_offset:end_offset]
        cfg_key = ",".join(cfg) if cfg else "all"
        entry_id = f"{relative}::{qualified}[cfg={cfg_key}]"
        entry = {
            "id": entry_id,
            "kind": kind,
            "classification": classification,
            "file": relative,
            "line": line_number(text, start_line_offset),
            "endLine": line_number(text, max(start_line_offset, end_offset - 1)),
            "symbol": name,
            "qualifiedName": qualified,
            "signature": signature,
            "sourceDigest": normalized_digest(fragment),
            "cfg": cfg,
            **empty_review(),
        }
        entries.append(entry)
        if terminator == "{":
            function_ranges.append({"start": index, "body": header_end, "end": end_index, "entry": entry})

    for container in containers:
        if container["kind"] != "impl" or not re.search(r"\bfor\b", container["name"]):
            continue
        start_offset = container["startOffset"]
        start_line_offset = text.rfind("\n", 0, start_offset) + 1
        end_offset = stream[container["end"]][2]
        enclosing = [
            candidate for candidate in containers
            if candidate["body"] < container["token"] < candidate["end"]
            and candidate is not container
        ]
        enclosing.sort(key=lambda item: item["body"])
        cfg = list(dict.fromkeys(
            [value for candidate in enclosing for value in candidate["cfg"]] + list(container["cfg"])
        ))
        classification = classify(relative, container["name"], cfg, "", enclosing)
        cfg_key = ",".join(cfg) if cfg else "all"
        entry_id = f"{relative}::{container['name']}[trait-impl,cfg={cfg_key}]"
        entries.append({
            "id": entry_id,
            "kind": "trait_impl",
            "classification": classification,
            "file": relative,
            "line": line_number(text, start_line_offset),
            "endLine": line_number(text, max(start_line_offset, end_offset - 1)),
            "symbol": container["name"],
            "qualifiedName": container["name"],
            "signature": container["name"],
            "sourceDigest": normalized_digest(text[start_line_offset:end_offset]),
            "cfg": cfg,
            **empty_review(),
        })

    owning_ranges: list[tuple[int, int, str]] = []
    for index, (value, _, _) in enumerate(stream):
        if value not in OWNING_CALLS:
            continue
        open_call = next((pos for pos in range(index + 1, min(len(stream), index + 6)) if stream[pos][0] == "("), None)
        if open_call is None or open_call not in paren_pairs:
            continue
        owning_ranges.append((open_call, paren_pairs[open_call], value))

    def is_direct_call_argument(opening: int, token_index: int) -> bool:
        paren_depth = 0
        brace_depth = 0
        bracket_depth = 0
        for pos in range(opening + 1, token_index):
            value = stream[pos][0]
            if value == "(": paren_depth += 1
            elif value == ")" and paren_depth: paren_depth -= 1
            elif value == "{": brace_depth += 1
            elif value == "}" and brace_depth: brace_depth -= 1
            elif value == "[": bracket_depth += 1
            elif value == "]" and bracket_depth: bracket_depth -= 1
        return paren_depth == 0 and brace_depth == 0 and bracket_depth == 0

    def containing_function(token_index: int) -> dict[str, Any] | None:
        return next((item for item in sorted(function_ranges, key=lambda item: item["body"], reverse=True)
                     if item["body"] < token_index < item["end"]), None)

    async_ordinal: Counter[str] = Counter()
    recorded_bodies: set[int] = set()
    for index, (value, start_offset, _) in enumerate(stream):
        if value != "async" or (index + 1 < len(stream) and stream[index + 1][0] == "fn"):
            continue
        body = next((pos for pos in range(index + 1, min(len(stream), index + 12)) if stream[pos][0] == "{"), None)
        if body is None or body not in pairs:
            continue
        # Do not walk through a statement while trying to find the async body.
        if any(stream[pos][0] == ";" for pos in range(index + 1, body)):
            continue
        recorded_bodies.add(body)
        end_index = pairs[body]
        end_offset = stream[end_index][2]
        owner = containing_function(index)
        qualified_owner = owner["entry"]["qualifiedName"] if owner else relative
        cfg = list(owner["entry"]["cfg"]) if owner else []
        classification = owner["entry"]["classification"] if owner else "production"
        owning_call = next((name for opening, closing, name in owning_ranges
                            if opening < index < closing and is_direct_call_argument(opening, index)), None)
        closure_kind = "owned_async_closure" if owning_call else "async_block"
        line = line_number(text, start_offset)
        fragment = text[start_offset:end_offset]
        ordinal_key = f"{qualified_owner}::{closure_kind}"
        async_ordinal[ordinal_key] += 1
        ordinal = async_ordinal[ordinal_key]
        entry_id = f"{relative}::{qualified_owner}::{closure_kind}#{ordinal}@{line}[cfg={','.join(cfg) if cfg else 'all'}]"
        entries.append({
            "id": entry_id,
            "kind": closure_kind,
            "classification": classification,
            "file": relative,
            "line": line,
            "endLine": line_number(text, max(start_offset, end_offset - 1)),
            "symbol": f"{closure_kind}#{ordinal}",
            "qualifiedName": f"{qualified_owner}::{closure_kind}#{ordinal}",
            "signature": collapse(masked[start_offset:stream[body][1]]),
            "sourceDigest": normalized_digest(fragment),
            "cfg": cfg,
            "ownerCall": owning_call,
            **empty_review(),
        })

    # Blocking/thread spawn closures are work owners even though they are not async.
    closure_ordinal: Counter[str] = Counter()
    for opening, closing, call_name in owning_ranges:
        for pipe_index in range(opening + 1, closing):
            if stream[pipe_index][0] != "|":
                continue
            if not is_direct_call_argument(opening, pipe_index):
                continue
            body = next((pos for pos in range(pipe_index + 1, min(closing, pipe_index + 12)) if stream[pos][0] == "{"), None)
            if body is None or body not in pairs or body in recorded_bodies:
                continue
            if any(stream[pos][0] == ";" for pos in range(pipe_index + 1, body)):
                continue
            recorded_bodies.add(body)
            anchor = pipe_index
            if pipe_index > opening + 1 and stream[pipe_index - 1][0] == "move":
                anchor = pipe_index - 1
            start_offset = stream[anchor][1]
            end_offset = stream[pairs[body]][2]
            owner = containing_function(opening)
            qualified_owner = owner["entry"]["qualifiedName"] if owner else relative
            cfg = list(owner["entry"]["cfg"]) if owner else []
            classification = owner["entry"]["classification"] if owner else "production"
            ordinal_key = f"{qualified_owner}::owned_work_closure"
            closure_ordinal[ordinal_key] += 1
            ordinal = closure_ordinal[ordinal_key]
            line = line_number(text, start_offset)
            fragment = text[start_offset:end_offset]
            entries.append({
                "id": f"{relative}::{qualified_owner}::owned_work_closure#{ordinal}@{line}[cfg={','.join(cfg) if cfg else 'all'}]",
                "kind": "owned_work_closure",
                "classification": classification,
                "file": relative,
                "line": line,
                "endLine": line_number(text, max(start_offset, end_offset - 1)),
                "symbol": f"owned_work_closure#{ordinal}",
                "qualifiedName": f"{qualified_owner}::owned_work_closure#{ordinal}",
                "signature": collapse(masked[start_offset:stream[body][1]]),
                "sourceDigest": normalized_digest(fragment),
                "cfg": cfg,
                "ownerCall": call_name,
                **empty_review(),
            })
            break
    return entries


def scan(source_root: Path) -> dict[str, Any]:
    rust_files = sorted(
        [path for path in (source_root / "src").rglob("*.rs")]
        + [path for path in (source_root / "build.rs", source_root / "build_config.rs") if path.is_file()]
    )
    entries = [entry for path in rust_files for entry in scan_file(source_root, path)]
    entries.sort(key=lambda entry: (entry["file"], entry["line"], entry["kind"], entry["id"]))
    ids = [entry["id"] for entry in entries]
    duplicates = [key for key, count in Counter(ids).items() if count > 1]
    if duplicates:
        raise ValueError(f"duplicate inventory identities: {duplicates[:5]}")
    counts = Counter(entry["classification"] for entry in entries)
    source_commit = None
    try:
        source_commit = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=source_root, check=True,
            capture_output=True, text=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        pass
    try:
        displayed_root = source_root.relative_to(Path.cwd().resolve()).as_posix()
    except ValueError:
        displayed_root = source_root.as_posix()
    return {
        "schemaVersion": SCHEMA_VERSION,
        "sourceCommit": source_commit,
        "sourceRoot": displayed_root,
        "digestConvention": "sha256-lowercase(exact item span, CRLF/CR normalized to LF, no trimming)",
        "reviewSemantics": "Discovery is pending. Only complete reviewer-authored source review fields may set reviewed.",
        "summary": {"files": len(rust_files), "entries": len(entries), **dict(sorted(counts.items()))},
        "entries": entries,
    }


def load_json(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        value = json.load(handle)
    if value.get("schemaVersion") != SCHEMA_VERSION or not isinstance(value.get("entries"), list):
        raise ValueError(f"unsupported or malformed review JSON: {path}")
    return value


def reconcile(current: dict[str, Any], previous: dict[str, Any]) -> None:
    prior_by_id = {entry["id"]: entry for entry in previous["entries"]}
    prior_by_digest: dict[str, list[dict[str, Any]]] = {}
    for entry in previous["entries"]:
        prior_by_digest.setdefault(entry.get("sourceDigest", ""), []).append(entry)
    current_ids = {entry["id"] for entry in current["entries"]}
    stale: list[dict[str, Any]] = []
    for entry in current["entries"]:
        prior = prior_by_id.get(entry["id"])
        if prior and prior.get("sourceDigest") == entry["sourceDigest"]:
            for field in REVIEW_FIELDS:
                entry[field] = prior.get(field, entry[field])
            continue
        if prior:
            entry["previousDigest"] = prior.get("sourceDigest")
            entry["staleReason"] = "source_changed"
            stale.append({"id": entry["id"], "reason": "source_changed"})
            continue
        moved = [candidate for candidate in prior_by_digest.get(entry["sourceDigest"], [])
                 if candidate["id"] not in current_ids]
        if len(moved) == 1:
            entry["movedFrom"] = moved[0]["id"]
            entry["staleReason"] = "moved_requires_context_review"
            stale.append({"id": entry["id"], "reason": "moved_requires_context_review"})
    removed = [entry["id"] for entry in previous["entries"] if entry["id"] not in current_ids]
    current["reconciliation"] = {"stale": stale, "removed": removed}


def merge_sidecars(current: dict[str, Any], reviews_dir: Path) -> None:
    by_id = {entry["id"]: entry for entry in current["entries"]}
    seen: set[str] = set()
    for path in sorted(reviews_dir.glob("*.json")):
        sidecar = load_json(path)
        for review in sidecar["entries"]:
            entry_id = review.get("id")
            if entry_id in seen:
                raise ValueError(f"duplicate sidecar review for {entry_id}")
            seen.add(entry_id)
            target = by_id.get(entry_id)
            if target is None:
                raise ValueError(f"stale sidecar entry not in source inventory: {entry_id}")
            for identity in ("file", "line", "endLine", "qualifiedName", "sourceDigest"):
                supplied = review.get(identity)
                if supplied is not None and supplied != target[identity]:
                    raise ValueError(f"stale {identity} for {entry_id} in {path}")
            for field in REVIEW_FIELDS:
                if field in review:
                    target[field] = review[field]
    current["reviewSidecars"] = [path.as_posix() for path in sorted(reviews_dir.glob("*.json"))]


def validate(ledger: dict[str, Any], current: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    actual = {entry["id"]: entry for entry in current["entries"]}
    recorded = {entry["id"]: entry for entry in ledger["entries"]}
    for missing in sorted(actual.keys() - recorded.keys()):
        errors.append(f"unreviewed source entry: {missing}")
    for stale in sorted(recorded.keys() - actual.keys()):
        errors.append(f"stale ledger entry: {stale}")
    for entry_id in sorted(actual.keys() & recorded.keys()):
        source = actual[entry_id]
        entry = recorded[entry_id]
        if entry.get("sourceDigest") != source["sourceDigest"]:
            errors.append(f"stale source digest: {entry_id}")
        status = entry.get("status")
        if status != "reviewed":
            errors.append(f"unreviewed status: {entry_id}")
            continue
        for field in ("purpose", "output", "findingStatus", "disposition", "reviewer", "reviewedAt"):
            if not entry.get(field):
                errors.append(f"missing {field}: {entry_id}")
        for field in REVIEW_ARRAYS:
            if not isinstance(entry.get(field), list):
                errors.append(f"{field} must be an array: {entry_id}")
            elif entry.get("classification") == "production" and not entry[field]:
                errors.append(f"{field} must record an explicit reviewed value: {entry_id}")
        if entry.get("classification") == "production" and not entry.get("cancellation"):
            errors.append(f"missing cancellation review: {entry_id}")
        if entry.get("findingStatus") not in ("none", "resolved"):
            errors.append(f"unresolved findingStatus: {entry_id}")
    return errors


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("scan", "merge", "check"))
    parser.add_argument("--source-root", type=Path, default=Path("nuclear-app/src-tauri"))
    parser.add_argument("--ledger", type=Path, default=Path("docs/backend-method-review.json"))
    parser.add_argument("--previous", type=Path)
    parser.add_argument("--reviews-dir", type=Path, default=Path("docs/backend-method-reviews"))
    args = parser.parse_args()

    current = scan(args.source_root.resolve())
    if args.mode == "scan":
        if args.previous:
            reconcile(current, load_json(args.previous))
        write_json(args.ledger, current)
        print(json.dumps(current["summary"], sort_keys=True))
        return 0
    if args.mode == "merge":
        previous = load_json(args.ledger) if args.ledger.exists() else None
        if previous:
            reconcile(current, previous)
        merge_sidecars(current, args.reviews_dir)
        write_json(args.ledger, current)
        errors = validate(current, current)
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        return 0

    ledger = load_json(args.ledger)
    errors = validate(ledger, current)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"review ledger covers {len(current['entries'])} current entries")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

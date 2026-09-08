#!/usr/bin/env python3
"""Check the reviewed backend module dependency contract.

This is a source contract built on the inventory lexer's masking and conventional
module-context logic. It is not a Rust name resolver.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable


INVENTORY_PATH = Path(__file__).with_name("inventory-backend-methods.py")
SPEC = importlib.util.spec_from_file_location("backend_inventory", INVENTORY_PATH)
INVENTORY = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(INVENTORY)

PRODUCTION_ROLES = ("applicationBoundary", "bootstrap", "services", "engines", "build")
RESTRICTED_ROLES = ("bootstrap", "services", "engines")
TAURI_BOUNDARY_TYPES = {"AppHandle", "Manager", "State"}


@dataclass(frozen=True, order=True)
class Violation:
    file: str
    line: int
    column: int
    rule: str
    message: str


def has_test_cfg(cfgs: Iterable[str]) -> bool:
    return INVENTORY.cfgs_require_test(cfgs)


def source_files(source_root: Path) -> list[Path]:
    return sorted(
        [path for path in (source_root / "src").rglob("*.rs")]
        + [path for path in (source_root / "build.rs", source_root / "build_config.rs") if path.is_file()]
    )


def expand_patterns(source_root: Path, patterns: list[str]) -> tuple[set[Path], list[str]]:
    matched: set[Path] = set()
    missing: list[str] = []
    for pattern in patterns:
        candidates = [path.resolve() for path in source_root.glob(pattern) if path.is_file()]
        if not candidates and not any(character in pattern for character in "*?["):
            missing.append(pattern)
        matched.update(candidates)
    return matched, missing


def test_container_ranges(text: str, masked: str, external_cfg: list[str]) -> list[tuple[int, int]]:
    if has_test_cfg(external_cfg):
        return [(0, len(masked))]
    stream = INVENTORY.tokens(masked)
    pairs = INVENTORY.delimiter_pairs(stream, "{", "}")
    containers: list[dict[str, object]] = []
    for index, (value, start, _) in enumerate(stream):
        if value not in ("mod", "impl", "fn", "extern"):
            continue
        header_end, terminator = INVENTORY.find_header_end(stream, index + 1)
        if header_end is None or terminator != "{" or header_end not in pairs:
            continue
        containers.append({
            "token": index,
            "body": header_end,
            "end": pairs[header_end],
            "start": start,
            "cfg": INVENTORY.cfgs_before(text, masked, start),
            "testAttribute": bool(
                value == "fn"
                and re.search(
                    r"#\s*\[\s*(?:tokio::)?test(?:\s*\(|\s*\])",
                    INVENTORY.attributes_before(masked, start),
                )
            ),
        })
    ranges: list[tuple[int, int]] = []
    for container in containers:
        enclosing = [
            candidate for candidate in containers
            if int(candidate["body"]) < int(container["token"]) < int(candidate["end"])
        ]
        inherited = [cfg for candidate in enclosing for cfg in candidate["cfg"]]
        if has_test_cfg([*external_cfg, *inherited, *container["cfg"]]) or container["testAttribute"]:
            ranges.append((int(container["start"]), stream[int(container["end"])][2]))
    return ranges


def offset_is_test(offset: int, ranges: list[tuple[int, int]]) -> bool:
    return any(start <= offset < end for start, end in ranges)


def statement_end(stream: list[tuple[str, int, int]], start: int) -> int | None:
    paren = brace = bracket = 0
    for index in range(start, len(stream)):
        value = stream[index][0]
        if value == "(": paren += 1
        elif value == ")": paren = max(0, paren - 1)
        elif value == "{": brace += 1
        elif value == "}": brace = max(0, brace - 1)
        elif value == "[": bracket += 1
        elif value == "]": bracket = max(0, bracket - 1)
        elif value == ";" and paren == 0 and brace == 0 and bracket == 0:
            return index
    return None


def split_group(values: list[str]) -> list[list[str]]:
    parts: list[list[str]] = []
    start = 0
    depth = 0
    for index, value in enumerate(values):
        if value == "{": depth += 1
        elif value == "}": depth -= 1
        elif value == "," and depth == 0:
            parts.append(values[start:index])
            start = index + 1
    parts.append(values[start:])
    return [part for part in parts if part]


def expand_use_tree(values: list[str], prefix: tuple[str, ...] = ()) -> list[tuple[str, ...]]:
    while values and values[0] == "::":
        values = values[1:]
    if not values:
        return []
    if values[0] == "{" and values[-1] == "}":
        return [
            leaf
            for part in split_group(values[1:-1])
            for leaf in expand_use_tree(part, prefix)
        ]
    head = values[0]
    path = (*prefix, head)
    if len(values) == 1 or (len(values) >= 2 and values[1] == "as"):
        return [path]
    if len(values) >= 2 and values[1] == "::":
        return expand_use_tree(values[2:], path)
    return [path]


def line_column(text: str, offset: int) -> tuple[int, int]:
    line = text.count("\n", 0, offset) + 1
    previous = text.rfind("\n", 0, offset)
    return line, offset - previous


def source_violations(
    source_root: Path,
    path: Path,
    role: str,
    external_cfg: list[str],
    workflows: set[str],
) -> list[Violation]:
    if role == "applicationBoundary" or has_test_cfg(external_cfg):
        return []
    text = path.read_text(encoding="utf-8")
    masked = INVENTORY.mask_rust(text)
    stream = INVENTORY.tokens(masked)
    test_ranges = test_container_ranges(text, masked, external_cfg)
    relative = path.relative_to(source_root).as_posix()
    violations: set[Violation] = set()
    use_ranges: list[tuple[int, int]] = []

    def add(index: int, rule: str, message: str) -> None:
        offset = stream[index][1]
        line, column = line_column(text, offset)
        violations.add(Violation(relative, line, column, rule, message))

    for index, (value, offset, _) in enumerate(stream):
        if value != "use" or offset_is_test(offset, test_ranges):
            continue
        if has_test_cfg(INVENTORY.cfgs_before(text, masked, offset)):
            continue
        end = statement_end(stream, index + 1)
        if end is None:
            add(index, "ARCH000", "unterminated use statement in restricted module")
            continue
        use_ranges.append((index, end))
        leaves = expand_use_tree([item[0] for item in stream[index + 1:end]])
        if any(leaf and leaf[-1] == "*" for leaf in leaves):
            add(index, "ARCH003", "production wildcard import in extracted module")
        tauri_types = sorted({segment for leaf in leaves if leaf and leaf[0] == "tauri" for segment in leaf if segment in TAURI_BOUNDARY_TYPES})
        if tauri_types:
            add(index, "ARCH001", f"Tauri application boundary import: {', '.join(tauri_types)}")
        root_items = sorted({leaf[1] for leaf in leaves if len(leaf) == 2 and leaf[0] == "crate" and leaf[1] in workflows})
        if root_items:
            add(index, "ARCH002", f"crate-root workflow import: {', '.join(root_items)}")

    def in_use(index: int) -> bool:
        return any(start <= index <= end for start, end in use_ranges)

    for index, (value, offset, _) in enumerate(stream):
        if offset_is_test(offset, test_ranges) or in_use(index):
            continue
        if value == "tauri" and index + 2 < len(stream) and stream[index + 1][0] == "::":
            boundary = stream[index + 2][0]
            if boundary in TAURI_BOUNDARY_TYPES:
                add(index, "ARCH001", f"Tauri application boundary path: {boundary}")
        if value == "crate" and index + 2 < len(stream) and stream[index + 1][0] == "::":
            workflow = stream[index + 2][0]
            if workflow in workflows:
                add(index, "ARCH002", f"crate-root workflow reference: {workflow}")
        if value == "super":
            cursor = index
            while cursor + 2 < len(stream) and stream[cursor + 1][0] == "::" and stream[cursor + 2][0] == "super":
                cursor += 2
            if cursor > index and cursor + 2 < len(stream) and stream[cursor + 1][0] == "::":
                workflow = stream[cursor + 2][0]
                if workflow in workflows:
                    add(index, "ARCH002", f"parent-escaped crate workflow reference: {workflow}")
    return sorted(violations)


def check_contract(policy_path: Path) -> list[Violation]:
    policy = json.loads(policy_path.read_text(encoding="utf-8"))
    if policy.get("schemaVersion") != 1:
        return [Violation(policy_path.as_posix(), 1, 1, "ARCH000", "unsupported architecture policy schema")]
    if policy.get("stageStatus") != "complete":
        return [Violation(policy_path.as_posix(), 1, 1, "STAGE4_INCOMPLETE", "Stage 4 architecture policy is not activated")]
    source_root = (policy_path.parent.parent / policy["sourceRoot"]).resolve()
    roles = policy.get("roles", {})
    role_files: dict[str, set[Path]] = {}
    violations: list[Violation] = []
    for role in (*PRODUCTION_ROLES, "testOnly"):
        patterns = roles.get(role)
        if not isinstance(patterns, list) or (role != "testOnly" and not patterns):
            violations.append(Violation(policy_path.as_posix(), 1, 1, "ARCH000", f"missing nonempty role: {role}"))
            role_files[role] = set()
            continue
        role_files[role], missing = expand_patterns(source_root, patterns)
        for pattern in missing:
            violations.append(Violation(policy_path.as_posix(), 1, 1, "ARCH000", f"role pattern matched no file: {role}/{pattern}"))
        if role != "testOnly" and not role_files[role]:
            violations.append(Violation(policy_path.as_posix(), 1, 1, "ARCH000", f"role matched no files: {role}"))
    if violations:
        return sorted(violations)

    files = source_files(source_root)
    for path in files:
        production = [role for role in PRODUCTION_ROLES if path.resolve() in role_files[role]]
        test_only = path.resolve() in role_files["testOnly"]
        if not production and not test_only:
            relative = path.relative_to(source_root).as_posix()
            violations.append(Violation(relative, 1, 1, "ARCH000", "Rust source is not assigned to a reviewed role"))
        if len(production) > 1:
            relative = path.relative_to(source_root).as_posix()
            violations.append(Violation(relative, 1, 1, "ARCH000", f"overlapping production roles: {', '.join(production)}"))

    external = INVENTORY.external_module_contexts(source_root, files)
    for path in sorted(role_files["testOnly"]):
        if path not in files:
            continue
        cfg, _modules = external.get(path.resolve(), ([], []))
        if not has_test_cfg(cfg):
            relative = path.relative_to(source_root).as_posix()
            violations.append(Violation(relative, 1, 1, "ARCH000", "test-only role lacks an inherited cfg(test) declaration"))
    workflows = set(policy.get("rootWorkflowSymbols", []))
    for path in files:
        production = [role for role in PRODUCTION_ROLES if path.resolve() in role_files[role]]
        if not production or path.resolve() in role_files["testOnly"]:
            continue
        role = production[0]
        if role not in RESTRICTED_ROLES:
            continue
        cfg, _modules = external.get(path.resolve(), ([], []))
        violations.extend(source_violations(source_root, path, role, cfg, workflows))
    return sorted(set(violations))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--policy",
        type=Path,
        default=Path("docs/backend-architecture-contract.json"),
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    violations = check_contract(args.policy.resolve())
    if args.json:
        print(json.dumps({"passed": not violations, "violations": [asdict(item) for item in violations]}, indent=2))
    else:
        for item in violations:
            print(f"{item.file}:{item.line}:{item.column}: {item.rule} {item.message}", file=sys.stderr)
    return 1 if violations else 0


if __name__ == "__main__":
    raise SystemExit(main())

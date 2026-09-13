#!/usr/bin/env python3
"""Fail-closed production source size and Rust control-flow guard."""

from __future__ import annotations

import argparse, importlib.util, json, re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONTRACT = ROOT / "docs" / "source-health-contract.json"
INVENTORY = Path(__file__).with_name("inventory-backend-methods.py")


def load_inventory():
    spec = importlib.util.spec_from_file_location("backend_inventory", INVENTORY)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def effective_lines(masked: str) -> int:
    return sum(bool(line.strip()) for line in masked.splitlines())


def callable_lines(masked: str) -> int:
    """Count physical source lines, with statement count preventing minification bypass."""
    return max(effective_lines(masked), masked.count(";"))


def max_flow_nesting(masked: str, inventory=None) -> int:
    inventory = inventory or load_inventory()
    stream = inventory.tokens(masked)
    braces = inventory.delimiter_pairs(stream, "{", "}")
    flow_bodies: set[int] = set()
    for index, (value, _, _) in enumerate(stream):
        if value not in {"if", "else", "match", "for", "while", "loop"}:
            continue
        if value == "else" and index + 1 < len(stream) and stream[index + 1][0] == "if":
            continue
        paren = bracket = 0
        position = index + 1
        while position < len(stream):
            token = stream[position][0]
            if token == "(": paren += 1
            elif token == ")": paren = max(0, paren - 1)
            elif token == "[": bracket += 1
            elif token == "]": bracket = max(0, bracket - 1)
            elif token == "{" and paren == 0 and bracket == 0 and position in braces:
                after = braces[position] + 1
                following = stream[after][0] if after < len(stream) else None
                # A struct literal/pattern before the actual control body.
                if following in {"{", "in"}:
                    position = after
                    continue
                flow_bodies.add(position)
                break
            elif token == ";" and paren == 0 and bracket == 0:
                break
            position += 1
    depth = maximum = 0
    stack: list[bool] = []
    for index, (token, _, _) in enumerate(stream):
        if token == "{":
            is_flow = index in flow_bodies
            stack.append(is_flow)
            if is_flow:
                depth += 1
                maximum = max(maximum, depth)
        elif token == "}" and stack:
            if stack.pop():
                depth -= 1
    return maximum


def production_file(path: Path, source_dir: Path) -> bool:
    relative = path.relative_to(source_dir)
    name = path.name
    return (
        "tests" not in relative.parts
        and name not in {"tests.rs", "test_support.rs", "performance_harness.rs", "soak_harness.rs", "backend_lifecycle_tests.rs", "public_boundary_tests.rs"}
        and not name.endswith("_tests.rs")
    )


def production_mask(text: str, inventory) -> str:
    """Mask comments/strings and complete cfg(test)-only source regions."""
    masked = inventory.mask_rust(text)
    chars = list(masked)
    stream = inventory.tokens(masked)
    pairs = inventory.delimiter_pairs(stream, "{", "}")
    ranges: list[tuple[int, int]] = []
    for index, (value, start, _) in enumerate(stream):
        if value not in {"mod", "fn", "impl"}:
            continue
        header, terminator = inventory.find_header_end(stream, index + 1)
        if header is None:
            continue
        cfg = inventory.cfgs_before(text, masked, start)
        attributes = inventory.attributes_before(masked, start)
        test_only = inventory.cfgs_require_test(cfg) or bool(
            re.search(r"#\s*\[\s*(?:tokio::)?test(?:\s*\(|\s*\])", attributes)
        )
        if not test_only:
            continue
        end = stream[pairs[header]][2] if terminator == "{" and header in pairs else stream[header][2]
        line_start = text.rfind("\n", 0, start) + 1
        # Include immediately preceding attribute lines in the excluded region.
        while line_start > 0:
            previous_start = text.rfind("\n", 0, line_start - 1) + 1
            if not text[previous_start:line_start].strip().startswith("#"):
                break
            line_start = previous_start
        ranges.append((line_start, end))
    for start, end in ranges:
        for offset in range(start, end):
            if chars[offset] != "\n":
                chars[offset] = " "
    return "".join(chars)


def collect_rust(root: Path, inventory) -> list[dict]:
    result = []
    scanned = inventory.scan(root)
    production = [e for e in scanned["entries"] if e["classification"] == "production"]
    for path in sorted((root / "src").rglob("*.rs")):
        if not production_file(path, root / "src"):
            continue
        relative = path.relative_to(root).as_posix()
        text = path.read_text(encoding="utf-8")
        masked = production_mask(text, inventory)
        result.append({"kind": "file", "id": relative, "metric": "loc", "value": effective_lines(masked)})
    for entry in production:
        if entry["kind"] in {"trait_impl", "function_declaration", "trait_method_declaration", "ffi_declaration"}:
            continue
        text = (root / entry["file"]).read_text(encoding="utf-8")
        masked = inventory.mask_rust(text)
        lines = masked.splitlines()[entry["line"] - 1:entry["endLine"]]
        span = "\n".join(lines)
        result.extend([
            {"kind": "callable", "id": entry["id"], "metric": "loc", "value": callable_lines(span)},
            {"kind": "callable", "id": entry["id"], "metric": "nesting", "value": max_flow_nesting(span)},
        ])
    return result


def validate_contract(contract: dict) -> list[str]:
    errors = []
    limits = contract.get("limits")
    expected_limits = {"productionFileLoc", "callableLoc", "flowNesting"}
    if not isinstance(limits, dict) or set(limits) != expected_limits:
        return ["contract limits must contain exactly productionFileLoc, callableLoc, and flowNesting"]
    for name, value in limits.items():
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            errors.append(f"limit {name} must be a positive integer")
    exceptions = contract.get("exceptions", {})
    if not isinstance(exceptions, dict):
        return errors + ["contract exceptions must be an object"]
    required = {"responsibility", "ceiling", "why", "alternatives", "tests"}
    for key, value in exceptions.items():
        parts = key.split(":", 2)
        if len(parts) != 3 or parts[0] not in {"file", "callable"} or parts[1] not in {"loc", "nesting"} or (parts[0] == "file" and parts[1] != "loc"):
            errors.append(f"exception {key} has an unknown measurement identity")
        if not isinstance(value, dict):
            errors.append(f"exception {key} must be an object")
            continue
        missing = sorted(required - value.keys())
        if missing or not all(value.get(field) for field in required):
            errors.append(f"exception {key} lacks reviewed fields: {', '.join(missing) or 'empty value'}")
        ceiling = value.get("ceiling")
        if isinstance(ceiling, bool) or not isinstance(ceiling, int) or ceiling <= 0:
            errors.append(f"exception {key} ceiling must be a positive integer")
        if not isinstance(value.get("tests"), list) or not all(isinstance(item, str) and item for item in value.get("tests", [])):
            errors.append(f"exception {key} tests must be a nonempty string array")
    return errors


def evaluate(measurements: list[dict], contract: dict) -> list[str]:
    limits = contract["limits"]
    exceptions = contract.get("exceptions", {})
    errors = validate_contract(contract)
    if errors:
        return errors
    seen = set()
    measured = set()
    for item in measurements:
        kind = item.get("kind")
        metric = item.get("metric")
        value = item.get("value")
        if kind not in {"file", "callable"} or metric not in {"loc", "nesting"} or (kind == "file" and metric != "loc"):
            errors.append(f"unknown measurement: {kind}:{metric}:{item.get('id')}")
            continue
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            errors.append(f"measurement {kind}:{metric}:{item.get('id')} must have a nonnegative integer value")
            continue
        measurement_key = (kind, metric, item.get("id"))
        if measurement_key in measured:
            errors.append(f"duplicate measurement: {kind}:{metric}:{item.get('id')}")
            continue
        measured.add(measurement_key)
        limit = limits["productionFileLoc"] if item["kind"] == "file" else limits["callableLoc"] if item["metric"] == "loc" else limits["flowNesting"]
        key = f'{item["kind"]}:{item["metric"]}:{item["id"]}'
        exception = exceptions.get(key)
        ceiling = exception.get("ceiling") if exception else limit
        if exception:
            seen.add(key)
            if ceiling <= limit:
                errors.append(f"exception {key} ceiling must exceed the standard limit {limit}")
        if item["value"] > ceiling:
            suffix = "; update requires human review" if exception else ""
            errors.append(f'{key} is {item["value"]}, ceiling {ceiling}{suffix}')
    for stale in sorted(set(exceptions) - seen):
        errors.append(f"stale exception: {stale}")
    return errors


def main(argv=None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", action="store_true")
    parser.add_argument("--source-root", type=Path, default=ROOT / "nuclear-app" / "src-tauri")
    args = parser.parse_args(argv)
    contract = json.loads(CONTRACT.read_text(encoding="utf-8"))
    inventory = load_inventory()
    measurements = collect_rust(args.source_root.resolve(), inventory)
    errors = evaluate(measurements, contract)
    if args.report:
        print(json.dumps({"measurements": measurements, "errors": errors}, indent=2))
    elif errors:
        print("Source health failed:\n" + "\n".join(f"- {e}" for e in errors), file=sys.stderr)
    else:
        print(f"Source health passed ({len(measurements)} Rust measurements).")
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())

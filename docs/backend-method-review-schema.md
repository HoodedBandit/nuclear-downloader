# Backend method review ledger

`backend-method-review.json` is the generated, source-bound inventory for the
backend maintainability review. Discovery creates a `pending` row; it does not
mean the callable was reviewed. Reviewers write non-overlapping sidecars under
`backend-method-reviews/`, and the inventory tool reconciles those records with
the current source.

## Inventory identity

Each entry has these generated fields:

- `id`, `kind`, `classification`, `file`, `line`, `endLine`, `symbol`,
  `qualifiedName`, `signature`, `sourceDigest`, and `cfg`;
- `ownerCall` for an async or blocking closure that is a direct argument to a
  task/thread ownership API;
- `movedFrom`, `previousDigest`, or `staleReason` when reconciliation detects a
  source move or change.

Kinds are `free_function`, `local_function`, `method`, `trait_method`,
`trait_method_declaration`, `function_declaration`, `ffi_declaration`,
`trait_impl`, `destructor`, `async_block`, `owned_async_closure`, and
`owned_work_closure`. Classification is `production`, `test`, or
`test_support`. The digest is lowercase SHA-256 of the exact item span after
normalizing CRLF or CR to LF, without trimming or adding a newline.

The tool is a conservative structural lexer, not a complete Rust parser. It
masks nested comments and string, raw-string, byte-string, and character
literals while retaining byte offsets. It inventories explicit `fn`
declarations, trait implementation blocks, every explicit async block, and
closures passed directly to the configured task/thread ownership calls. It
excludes function-pointer type syntax and does not infer callables generated
only by macro expansion. For conventional out-of-line modules, classification
inherits module and `cfg` context only from an actual `mod name;` declaration
whose source target exists in the scanned tree; filenames and `test_` prefixes
do not establish inherited module context. Conventional resolution treats
`lib.rs`, `main.rs`, `mod.rs`, and `build.rs` as crate/module roots and treats
other source files as parents of a same-stem module directory. Explicit
`#[path = "..."]` overrides, macro-generated modules, and nonconventional compiler
module resolution are not followed; those remain source-review obligations.
At the starting source, an independent masked-token
reconciliation found 961 `fn` tokens: one function-pointer alias and exactly
960 inventoried declarations. Its fixture tests cover comments/literals,
lifetimes, FFI declarations, local/method/trait/Drop items, cfg-test
classification, assigned and nested async blocks, direct spawn ownership, and
the process-spawn false-positive boundary. Discovery still requires source
review; these checks establish inventory coverage only.

Test-support classification evaluates `all`, `any`, and `not` cfg expressions
with the `test` predicate fixed to false. A row is excluded from production only
when its cfg cannot then be true. Other predicates remain unknown and malformed
or unsupported expressions conservatively remain production-visible. Thus
`cfg(not(test))` and `cfg(any(test, windows))` are production-capable, while
`cfg(all(test, windows))` requires the test build.

## Reviewer fields

Every reviewed row records:

- `purpose`, `callers`, `inputs`, and `output`;
- `sideEffects`, `errors`, `ownership`, `locks`, and `cancellation`;
- `invariants`, `tests`, `findingStatus`, and `evidence`;
- `disposition`, `destination`, `reviewer`, `reviewedAt`, `status`, and `notes`.

Every reviewed row, including test and test-support infrastructure, supplies a
nonempty array of nonempty strings for each array field. Use `["none"]` only
when source review establishes that the field has no applicable behavior.
Every scalar reviewer field is an explicit nonempty string.
Use canonical UTC for `reviewedAt`. `findingStatus` is `none`, `risk`,
`confirmed`, or `resolved`. A final passing ledger permits only `none` or
`resolved`. `disposition` is `keep`, `move`, `split`, `fix`, `remove`, or
`test_only`. Only `status: reviewed` is complete; `pending`, `needs_changes`,
and `deferred` fail the final check.

The sidecar shape is:

```json
{
  "schemaVersion": 1,
  "scope": ["src/example.rs"],
  "reviewer": "reviewer-name",
  "entries": []
}
```

Every sidecar entry must include every generated identity field and each value
must exactly match the current generated inventory. This includes an explicit
`ownerCall: null` when the row has no owning call. Missing identity fields are
rejected rather than populated during merge, so an old reviewed sidecar cannot
silently bless a changed signature, classification, kind, cfg list, owner call,
source location, or digest. Copy current identity fields only after reviewing
the current source span and its caller/ownership context.

A uniquely moved exact source body is identified, but remains pending because
callers, visibility, and ownership context may have changed. The final check
also compares every recorded generated identity with a fresh source scan.

## Commands

From the repository root:

```powershell
python scripts/inventory-backend-methods.py scan
python scripts/inventory-backend-methods.py scan --previous docs/backend-method-review.json
python scripts/inventory-backend-methods.py merge
python scripts/inventory-backend-methods.py check
python -m unittest scripts/test_inventory_backend_methods.py
```

`merge` writes the reconciled ledger but exits nonzero until every current row is
complete. `check` is read-only and fails for new, missing, duplicate, stale,
changed, deferred, unresolved, or unreviewed entries.

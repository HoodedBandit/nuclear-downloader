import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("inventory-backend-methods.py")
SPEC = importlib.util.spec_from_file_location("backend_inventory", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class BackendInventoryTests(unittest.TestCase):
    def scan_fixture(self, source: str):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            (root / "src" / "fixture.rs").write_text(source, encoding="utf-8")
            return MODULE.scan(root)["entries"]

    def scan_tree(self, files: dict[str, str]):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for relative, source in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source, encoding="utf-8")
            return MODULE.scan(root)["entries"]

    def test_literals_comments_pointer_alias_and_ffi_are_classified(self):
        entries = self.scan_fixture(
            r'''
// fn comment_fake() {}
const TEXT: &str = "fn string_fake() {}";
type Callback = fn(u32) -> u32;
extern "system" { fn NativeCall(value: u32) -> i32; }
fn real() {}
fn lifetime<'a>(value: &'a str) -> Option<&'a str> { Some(value) }
'''
        )
        symbols = {entry["symbol"]: entry for entry in entries}
        self.assertNotIn("comment_fake", symbols)
        self.assertNotIn("string_fake", symbols)
        self.assertNotIn("(", symbols)
        self.assertEqual(symbols["NativeCall"]["kind"], "ffi_declaration")
        self.assertEqual(symbols["real"]["kind"], "free_function")
        self.assertEqual(symbols["lifetime"]["kind"], "free_function")

    def test_methods_trait_impl_drop_local_and_cfg_test_are_inventoried(self):
        entries = self.scan_fixture(
            r'''
struct Item;
trait Work { fn work(&self); }
impl Item { fn inherent(&self) { fn local() {} local(); } }
impl Work for Item { fn work(&self) {} }
impl Drop for Item { fn drop(&mut self) {} }
#[cfg(test)] mod tests { #[test] fn case() {} fn helper() {} }
'''
        )
        kinds = {(entry["symbol"], entry["kind"]) for entry in entries}
        self.assertIn(("inherent", "method"), kinds)
        self.assertIn(("local", "local_function"), kinds)
        self.assertIn(("work", "trait_method"), kinds)
        self.assertIn(("drop", "destructor"), kinds)
        self.assertTrue(any(entry["kind"] == "trait_impl" and "Work for Item" in entry["symbol"] for entry in entries))
        case = next(entry for entry in entries if entry["symbol"] == "case")
        helper = next(entry for entry in entries if entry["symbol"] == "helper")
        self.assertEqual(case["classification"], "test")
        self.assertEqual(helper["classification"], "test_support")

    def test_all_async_blocks_are_recorded_and_only_direct_spawn_argument_is_owned(self):
        entries = self.scan_fixture(
            r'''
async fn command() {
    let task = async move { one().await; };
    manager.spawn_tracked(Kind::Worker, task);
    manager.spawn_tracked(Kind::Worker, async move {
        let nested = AssertUnwindSafe(async move { two().await; });
        nested.await;
    });
    std::process::Command::new("tool").spawn().map_err(|_| Error)?;
    tokio::task::spawn_blocking(move || { blocking(); });
}
'''
        )
        async_entries = [entry for entry in entries if entry["kind"] in ("async_block", "owned_async_closure")]
        self.assertEqual(len(async_entries), 3)
        self.assertEqual(sum(entry["kind"] == "owned_async_closure" for entry in async_entries), 1)
        work = [entry for entry in entries if entry["kind"] == "owned_work_closure"]
        self.assertEqual(len(work), 1)
        self.assertEqual(work[0]["ownerCall"], "spawn_blocking")
        self.assertFalse(any(entry.get("ownerCall") == "spawn" for entry in entries))

    def test_reconciliation_marks_changed_and_moved_rows_pending(self):
        prior = {
            "schemaVersion": 1,
            "entries": [{
                "id": "old.rs::same[cfg=all]", "sourceDigest": "digest",
                "status": "reviewed", "purpose": "reviewed",
            }],
        }
        current = {
            "entries": [{
                "id": "new.rs::same[cfg=all]", "sourceDigest": "digest",
                **MODULE.empty_review(),
            }],
        }
        MODULE.reconcile(current, prior)
        self.assertEqual(current["entries"][0]["status"], "pending")
        self.assertEqual(current["entries"][0]["movedFrom"], "old.rs::same[cfg=all]")
        self.assertEqual(current["entries"][0]["staleReason"], "moved_requires_context_review")

    def test_external_test_module_context_is_inherited_from_declaration(self):
        entries = self.scan_tree({
            "src/lib.rs": "#[cfg(test)] mod tests;\nmod production;\n",
            "src/tests.rs": "mod helpers;\nfn helper() {}\n#[test] fn case() {}\n",
            "src/tests/helpers.rs": "fn nested_helper() {}\n",
            "src/production.rs": "fn helper() {}\nfn test_name_is_not_an_attribute() {}\n",
            "build.rs": "mod build_config;\nfn main() {}\n",
            "build_config.rs": "fn shared_build_helper() {}\n#[cfg(test)] mod tests { fn build_test_helper() {} }\n",
        })
        by_file_symbol = {(entry["file"], entry["symbol"]): entry for entry in entries}
        self.assertEqual(by_file_symbol[("src/tests.rs", "helper")]["classification"], "test_support")
        self.assertEqual(by_file_symbol[("src/tests.rs", "case")]["classification"], "test")
        self.assertEqual(
            by_file_symbol[("src/tests/helpers.rs", "nested_helper")]["classification"],
            "test_support",
        )
        self.assertEqual(by_file_symbol[("src/production.rs", "helper")]["classification"], "production")
        self.assertEqual(
            by_file_symbol[("src/production.rs", "test_name_is_not_an_attribute")]["classification"],
            "production",
        )
        self.assertEqual(by_file_symbol[("build_config.rs", "shared_build_helper")]["classification"], "production")
        self.assertEqual(
            by_file_symbol[("build_config.rs", "shared_build_helper")]["qualifiedName"],
            "build_config::shared_build_helper",
        )
        self.assertEqual(
            by_file_symbol[("build_config.rs", "build_test_helper")]["classification"],
            "test_support",
        )

    def test_sidecar_cannot_override_any_generated_identity(self):
        current = {"entries": [{
            "id": "src/lib.rs::run[cfg=all]",
            "kind": "free_function", "classification": "production",
            "file": "src/lib.rs", "line": 1, "endLine": 1,
            "symbol": "run", "qualifiedName": "run", "signature": "fn run()",
            "sourceDigest": "digest", "cfg": [], **MODULE.empty_review(),
        }]}
        with tempfile.TemporaryDirectory() as temporary:
            reviews = Path(temporary)
            for field, bad in (
                ("signature", "fn run(value: u8)"),
                ("classification", "test"),
                ("ownerCall", "spawn"),
            ):
                sidecar = {
                    "schemaVersion": 1,
                    "entries": [{"id": "src/lib.rs::run[cfg=all]", field: bad}],
                }
                (reviews / "review.json").write_text(json.dumps(sidecar), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, f"stale {field}"):
                    MODULE.merge_sidecars(current, reviews)

    def test_final_validation_requires_explicit_review_evidence_for_test_rows(self):
        entry = {
            "id": "src/tests.rs::case[cfg=test]",
            "kind": "free_function", "classification": "test",
            "file": "src/tests.rs", "line": 1, "endLine": 1,
            "symbol": "case", "qualifiedName": "case", "signature": "fn case()",
            "sourceDigest": "digest", "cfg": ["test"],
            **MODULE.empty_review(),
        }
        entry.update({
            "purpose": "exercise behavior", "output": "unit",
            "cancellation": "synchronous", "findingStatus": "none",
            "disposition": "test_only", "destination": "src/tests.rs",
            "reviewer": "reviewer", "reviewedAt": "2026-09-08T00:00:00Z",
            "status": "reviewed", "notes": "none",
        })
        for field in MODULE.REVIEW_ARRAYS:
            entry[field] = ["none"]
        entry["evidence"] = []
        ledger = {"entries": [entry]}
        errors = MODULE.validate(ledger, ledger)
        self.assertIn(
            "evidence must record an explicit reviewed value: src/tests.rs::case[cfg=test]",
            errors,
        )

    def test_cfg_classification_requires_test_instead_of_matching_its_name(self):
        entries = self.scan_fixture(
            r'''
#[cfg(not(test))] fn not_test() {}
#[cfg(any(test, windows))] fn test_or_windows() {}
#[cfg(all(test, windows))] fn test_and_windows() {}
#[cfg(test)] mod inherited { fn helper() {} }
#[cfg(all(any(test, windows), all(test, not(unix))))] fn nested_requires_test() {}
#[cfg(any(all(test, windows), feature = "fixture"))] fn nested_can_be_production() {}
'''
        )
        classified = {entry["symbol"]: entry["classification"] for entry in entries}
        self.assertEqual(classified["not_test"], "production")
        self.assertEqual(classified["test_or_windows"], "production")
        self.assertEqual(classified["test_and_windows"], "test_support")
        self.assertEqual(classified["helper"], "test_support")
        self.assertEqual(classified["nested_requires_test"], "test_support")
        self.assertEqual(classified["nested_can_be_production"], "production")


if __name__ == "__main__":
    unittest.main()

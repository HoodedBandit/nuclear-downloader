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


if __name__ == "__main__":
    unittest.main()

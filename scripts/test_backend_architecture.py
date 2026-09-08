import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check-backend-architecture.py")
SPEC = importlib.util.spec_from_file_location("backend_architecture", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class BackendArchitectureTests(unittest.TestCase):
    def fixture(self, overrides=None, extra=None, stage="complete"):
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        files = {
            "crate/src/lib.rs": "mod desktop; mod bootstrap; mod services; mod engines;\n",
            "crate/src/desktop.rs": "use tauri::{AppHandle, Manager, State};\nfn boundary(_app: AppHandle, _state: State<'_, u8>) {}\n",
            "crate/src/bootstrap.rs": "use crate::services::Service;\npub struct Bootstrap;\n",
            "crate/src/services/mod.rs": "use crate::engines::Engine;\npub struct Service(Engine);\n",
            "crate/src/engines/mod.rs": "pub struct Engine;\n",
            "crate/build.rs": "mod build_config;\nfn main() { build_config::configure(); }\n",
            "crate/build_config.rs": "pub fn configure() {}\n#[cfg(test)] mod tests { use super::*; }\n",
        }
        files.update(overrides or {})
        files.update(extra or {})
        for relative, source in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source, encoding="utf-8")
        policy = {
            "schemaVersion": 1,
            "stageStatus": stage,
            "sourceRoot": "crate",
            "roles": {
                "applicationBoundary": ["src/lib.rs", "src/desktop.rs"],
                "bootstrap": ["src/bootstrap.rs"],
                "services": ["src/services/**/*.rs"],
                "engines": ["src/engines/**/*.rs"],
                "build": ["build.rs", "build_config.rs"],
                "testOnly": ["src/**/tests.rs", "src/**/tests/**/*.rs"],
            },
            "rootWorkflowSymbols": ["begin_inspection", "cancel_operation"],
        }
        policy_path = root / "docs" / "architecture.json"
        policy_path.parent.mkdir()
        policy_path.write_text(json.dumps(policy), encoding="utf-8")
        return temporary, policy_path

    def rules(self, policy):
        return [item.rule for item in MODULE.check_contract(policy)]

    def test_incomplete_policy_fails_closed(self):
        temporary, policy = self.fixture(stage="incomplete")
        self.addCleanup(temporary.cleanup)
        violations = MODULE.check_contract(policy)
        self.assertEqual([item.rule for item in violations], ["STAGE4_INCOMPLETE"])

    def test_clean_roles_allow_only_desktop_tauri_boundary_and_mask_literals(self):
        temporary, policy = self.fixture(overrides={
            "crate/src/services/mod.rs": r'''
use crate::engines::Engine;
// use tauri::{AppHandle, Manager, State};
const TEXT: &str = "crate::begin_inspection(); use crate::engines::*;";
pub struct Service(Engine);
''',
        })
        self.addCleanup(temporary.cleanup)
        self.assertEqual(MODULE.check_contract(policy), [])

    def test_grouped_imports_and_qualified_references_are_rejected(self):
        temporary, policy = self.fixture(overrides={
            "crate/src/services/mod.rs": """
use tauri::{AppHandle as Handle, Manager, State};
use crate::{begin_inspection as begin, engines::Engine};
use crate::engines::*;
fn run() { crate::cancel_operation(); tauri::Manager::state::<u8>(); }
""",
        })
        self.addCleanup(temporary.cleanup)
        violations = MODULE.check_contract(policy)
        self.assertEqual(sorted(set(item.rule for item in violations)), ["ARCH001", "ARCH002", "ARCH003"])
        self.assertTrue(all(item.file == "src/services/mod.rs" for item in violations))

    def test_cfg_test_wildcards_are_excluded_but_test_named_production_is_not(self):
        temporary, policy = self.fixture(overrides={
            "crate/src/services/mod.rs": """
use crate::engines::Engine;
#[cfg(test)] mod tests;
#[cfg(test)] mod inline { use crate::engines::*; }
fn test_named_helper() { let _ = Engine; }
""",
        }, extra={
            "crate/src/services/tests.rs": "use super::*; use tauri::AppHandle;\n",
        })
        self.addCleanup(temporary.cleanup)
        self.assertEqual(MODULE.check_contract(policy), [])

        (Path(temporary.name) / "crate/src/services/mod.rs").write_text(
            "use crate::engines::*;\nfn test_named_helper() {}\n",
            encoding="utf-8",
        )
        self.assertIn("ARCH003", self.rules(policy))

    def test_module_moves_and_line_changes_do_not_escape_role_checks(self):
        temporary, policy = self.fixture(extra={
            "crate/src/services/nested/task.rs": "\n\nuse crate::begin_inspection;\n",
        })
        self.addCleanup(temporary.cleanup)
        violations = MODULE.check_contract(policy)
        match = next(item for item in violations if item.rule == "ARCH002")
        self.assertEqual(match.file, "src/services/nested/task.rs")
        self.assertEqual(match.line, 3)

    def test_missing_roles_and_unmatched_sources_fail_configuration(self):
        temporary, policy = self.fixture(extra={"crate/src/orphan.rs": "fn orphan() {}\n"})
        self.addCleanup(temporary.cleanup)
        violations = MODULE.check_contract(policy)
        self.assertTrue(any(item.rule == "ARCH000" and "not assigned" in item.message for item in violations))

    def test_test_only_filename_without_cfg_declaration_fails_closed(self):
        temporary, policy = self.fixture(extra={
            "crate/src/services/tests.rs": "use super::*;\n",
        })
        self.addCleanup(temporary.cleanup)
        violations = MODULE.check_contract(policy)
        self.assertTrue(any(item.rule == "ARCH000" and "lacks an inherited cfg(test)" in item.message for item in violations))

    def test_cfg_boolean_logic_excludes_only_test_required_code(self):
        temporary, policy = self.fixture(overrides={
            "crate/src/services/mod.rs": r'''
#[cfg(not(test))] use crate::engines::*;
#[cfg(any(test, windows))] use crate::engines::*;
#[cfg(all(test, windows))] use crate::engines::*;
#[cfg(test)] mod inherited { use crate::engines::*; }
#[cfg(all(any(test, windows), all(test, not(unix))))]
mod nested_requires_test { use crate::engines::*; }
''',
        })
        self.addCleanup(temporary.cleanup)
        violations = [item for item in MODULE.check_contract(policy) if item.rule == "ARCH003"]
        self.assertEqual([item.line for item in violations], [2, 3])


if __name__ == "__main__":
    unittest.main()

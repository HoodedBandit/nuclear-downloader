import importlib.util, tempfile, unittest
from pathlib import Path

PATH = Path(__file__).with_name("source-health.py")
SPEC = importlib.util.spec_from_file_location("source_health", PATH)
health = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(health)

class SourceHealthTests(unittest.TestCase):
    def test_comments_blank_lines_and_compressed_braces(self):
        masked = health.load_inventory().mask_rust("fn x(){ if ok { work(); } }\n// hidden\n\n")
        self.assertEqual(health.effective_lines(masked), 1)
        self.assertEqual(health.max_flow_nesting(masked), 1)

    def test_compressed_statements_do_not_bypass_callable_budget(self):
        self.assertEqual(health.callable_lines("".join("work();" for _ in range(101))), 101)

    def test_nested_flow_and_lambda_body(self):
        source = "if a { for b in c { items.map(|x| { if x { x } }); } }"
        self.assertEqual(health.max_flow_nesting(source), 3)

    def test_struct_literals_closures_match_arms_and_else_if(self):
        source = "if let Some(x) = Foo { value: 1 } { call(|y| { y }); } else if b { match x { A => one(), B => { two() } } }"
        self.assertEqual(health.max_flow_nesting(source), 2)

    def test_braceless_match_arms_do_not_add_flow_depth(self):
        self.assertEqual(health.max_flow_nesting("match x { A => one(), B => two() }"), 1)

    def test_exception_is_a_growth_ceiling(self):
        contract = {"limits":{"productionFileLoc":500,"callableLoc":100,"flowNesting":5},"exceptions":{"file:loc:a.rs":{"responsibility":"x","ceiling":510,"why":"x","alternatives":"x","tests":["x"]}}}
        self.assertFalse(health.evaluate([{"kind":"file","metric":"loc","id":"a.rs","value":510}], contract))
        self.assertTrue(any("human review" in e for e in health.evaluate([{"kind":"file","metric":"loc","id":"a.rs","value":511}], contract)))

    def test_malformed_ceiling_fails_as_a_contract_error(self):
        contract = {"limits":{"productionFileLoc":500,"callableLoc":100,"flowNesting":5},"exceptions":{"file:loc:a.rs":{"responsibility":"x","ceiling":"510","why":"x","alternatives":"x","tests":["x"]}}}
        self.assertIn("positive integer", "\n".join(health.evaluate([], contract)))

    def test_unknown_metric_and_stale_exception_fail(self):
        review = {"responsibility":"x","ceiling":510,"why":"x","alternatives":"x","tests":["x"]}
        unknown = {"limits":{"productionFileLoc":500,"callableLoc":100,"flowNesting":5},"exceptions":{"file:branches:a.rs":review}}
        self.assertIn("unknown measurement", "\n".join(health.evaluate([], unknown)))
        stale = {"limits":{"productionFileLoc":500,"callableLoc":100,"flowNesting":5},"exceptions":{"file:loc:a.rs":review}}
        self.assertIn("stale exception", "\n".join(health.evaluate([], stale)))

    def test_unknown_and_duplicate_measurements_fail_closed(self):
        contract = {"limits":{"productionFileLoc":500,"callableLoc":100,"flowNesting":5},"exceptions":{}}
        self.assertIn("unknown measurement", "\n".join(health.evaluate([{"kind":"callable","metric":"branches","id":"x","value":1}], contract)))
        item = {"kind":"file","metric":"loc","id":"a.rs","value":1}
        self.assertIn("duplicate measurement", "\n".join(health.evaluate([item, item], contract)))

    def test_test_and_test_support_bodies_are_not_production(self):
        inventory = health.load_inventory()
        self.assertEqual(inventory.classify("src/a.rs", "case", [], "#[test]", []), "test")
        self.assertEqual(inventory.classify("src/a.rs", "helper", ["test"], "", []), "test_support")

    def test_inline_cfg_test_module_is_removed_from_file_loc(self):
        inventory = health.load_inventory()
        source = "fn live() {}\n#[cfg(test)]\nmod checks {\n fn helper() {}\n #[test]\n fn case() {}\n}\n"
        self.assertEqual(health.effective_lines(health.production_mask(source, inventory)), 1)

    def test_tests_directory_is_excluded_even_with_an_unusual_filename(self):
        with tempfile.TemporaryDirectory() as directory:
            source_dir = Path(directory)
            nested = source_dir / "tests" / "fixture.rs"
            nested.parent.mkdir()
            nested.write_text("fn helper() {}\n", encoding="utf-8")
            self.assertFalse(health.production_file(nested, source_dir))

    def test_cfg_attr_is_counted_when_inventory_cannot_prove_test_only(self):
        inventory = health.load_inventory()
        source = "#[cfg_attr(not(test), cfg(any()))]\nfn uncertain() {}\n"
        self.assertEqual(health.effective_lines(health.production_mask(source, inventory)), 2)


if __name__ == '__main__': unittest.main()

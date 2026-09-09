import assert from "node:assert/strict";
import test from "node:test";
import {
  buildExternalStylesheet,
  verifyPageStyleContract,
} from "./page-style-contract.mjs";

test("route-root generation preserves source bytes around selector edits", () => {
  const oracleSource = `<main data-mode="ready">
  <header><h1>Fixture</h1><button>Run</button></header>
  <section class="queue"><div class="item"><span>One</span></div></section>
</main>
<style>
  /* retained contract comment */
  :root { --tone: red; }
  :global(body) { margin: 0; }
  header h1,
  .item > span { color: var(--tone); }
  button:hover:not(:disabled) { color: white; }
  main[data-mode='ready'] { display: block; }
  .queue::-webkit-scrollbar-thumb:hover { background: black; }
</style>`;
  const expected = `
  /* retained contract comment */
  :root { --tone: red; }
  body { margin: 0; }
  :root header h1,
  :root .item > span { color: var(--tone); }
  :root button:hover:not(:disabled) { color: white; }
  :root main[data-mode='ready'] { display: block; }
  :root .queue::-webkit-scrollbar-thumb:hover { background: black; }
`;
  const candidateCss = buildExternalStylesheet({ source: oracleSource });
  assert.equal(candidateCss, expected);
  assert.deepEqual(verifyPageStyleContract({ oracleSource, candidateCss }), {
    ruleCount: 6,
    selectorCount: 7,
  });
});

test("contract rejects selector order and specificity drift", () => {
  const oracleSource = `<main><button>Run</button></main><style>
main { color: red }
button:hover { color: blue }
</style>`;
  const candidateCss = buildExternalStylesheet({ source: oracleSource });
  assert.doesNotThrow(() =>
    verifyPageStyleContract({ oracleSource, candidateCss }),
  );
  assert.throws(
    () =>
      verifyPageStyleContract({
        oracleSource,
        candidateCss: candidateCss.replace(":root main", "main"),
      }),
    /selector|specificity|root-prefix/,
  );
  const rules = candidateCss.trim().split("\n");
  assert.throws(
    () =>
      verifyPageStyleContract({
        oracleSource,
        candidateCss: `${rules.reverse().join("\n")}\n`,
      }),
    /declarations|selector/,
  );
});

test("contract preserves root variables and global body specificity", () => {
  const oracleSource =
    "<main>Fixture</main><style>:root { --color: red } :global(body) { margin: 0 } main { color: var(--color) }</style>";
  const candidateCss = buildExternalStylesheet({ source: oracleSource });
  assert.match(candidateCss, /^:root\s*\{/);
  assert.match(candidateCss, /body\s*\{/);
  assert.match(candidateCss, /:root main\s*\{/);
  assert.doesNotThrow(() =>
    verifyPageStyleContract({ oracleSource, candidateCss }),
  );
});

#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
export const repositoryRoot = path.resolve(scriptDirectory, "..");
const requireFromFrontend = createRequire(
  path.join(repositoryRoot, "nuclear-app", "package.json"),
);
const cssTree = requireFromFrontend("css-tree");
const { compile, parse: parseSvelte } = requireFromFrontend("svelte/compiler");
const SCOPE_CLASS = /^svelte-[a-z0-9]+$/;

function sourceStyle(source, filename) {
  const parsed = parseSvelte(source, { filename });
  assert.ok(parsed.css, `${filename} must contain one component style block.`);
  return parsed.css.content.styles;
}

function selectorStrings(prelude) {
  assert.equal(
    prelude.type,
    "SelectorList",
    "Style contract accepts selector rules only.",
  );
  return prelude.children
    .toArray()
    .map((selector) => cssTree.generate(selector));
}

function isSpecialSelector(selector) {
  return selector === ":root" || selector === ":global(body)";
}

function externalSelector(selector) {
  if (selector === ":root") return ":root";
  if (selector === ":global(body)") return "body";
  return `:root ${selector}`;
}

function rulesFromCss(css, context) {
  const ast = cssTree.parse(css, { context: "stylesheet", positions: false });
  const rules = [];
  ast.children.forEach((node) => {
    assert.equal(
      node.type,
      "Rule",
      `${context} contains unsupported ${node.type}.`,
    );
    const declarations = [];
    node.block.children.forEach((declaration) => {
      assert.equal(
        declaration.type,
        "Declaration",
        `${context} contains an unsupported declaration-list node.`,
      );
      declarations.push(cssTree.generate(declaration));
    });
    rules.push({ selectors: selectorStrings(node.prelude), declarations });
  });
  return rules;
}

function unwrapScopeSelector(selector, context) {
  const ast = cssTree.parse(selector, {
    context: "selector",
    positions: false,
  });
  ast.children.forEach((node, item, list) => {
    if (node.type === "ClassSelector" && SCOPE_CLASS.test(node.name)) {
      list.remove(item);
      return;
    }
    if (
      node.type === "PseudoClassSelector" &&
      node.name === "where" &&
      node.children
    ) {
      const selectorList = node.children.toArray()[0];
      const nested =
        selectorList?.type === "SelectorList"
          ? selectorList.children.toArray()
          : [];
      const only = nested.length === 1 ? nested[0].children.toArray() : [];
      if (
        only.length === 1 &&
        only[0].type === "ClassSelector" &&
        SCOPE_CLASS.test(only[0].name)
      ) {
        list.remove(item);
      }
    }
  });
  const normalized = cssTree.generate(ast).replace(/\s+/g, " ").trim();
  assert.ok(normalized, `${context} became empty after scope normalization.`);
  return normalized;
}

function addSpecificity(left, right) {
  return left.map((value, index) => value + right[index]);
}

function maxSpecificity(values) {
  return values.reduce(
    (maximum, value) =>
      value[0] > maximum[0] ||
      (value[0] === maximum[0] && value[1] > maximum[1]) ||
      (value[0] === maximum[0] &&
        value[1] === maximum[1] &&
        value[2] > maximum[2])
        ? value
        : maximum,
    [0, 0, 0],
  );
}

function specificityForAst(ast, context) {
  let specificity = [0, 0, 0];
  ast.children.forEach((node) => {
    if (node.type === "IdSelector")
      specificity = addSpecificity(specificity, [1, 0, 0]);
    else if (
      node.type === "ClassSelector" ||
      node.type === "AttributeSelector"
    ) {
      specificity = addSpecificity(specificity, [0, 1, 0]);
    } else if (node.type === "TypeSelector")
      specificity = addSpecificity(specificity, [0, 0, 1]);
    else if (node.type === "PseudoElementSelector") {
      specificity = addSpecificity(specificity, [0, 0, 1]);
    } else if (node.type === "PseudoClassSelector") {
      if (node.name === "where") return;
      assert.ok(
        !["is", "has", "nth-child", "nth-last-child"].includes(node.name),
        `${context} uses unsupported specificity-sensitive :${node.name}().`,
      );
      if (node.name === "not" && node.children) {
        const selectorList = node.children.toArray()[0];
        assert.equal(
          selectorList?.type,
          "SelectorList",
          `${context} has malformed :not().`,
        );
        const nested = selectorList.children
          .toArray()
          .map((selector) => specificityForAst(selector, context));
        specificity = addSpecificity(specificity, maxSpecificity(nested));
      } else {
        specificity = addSpecificity(specificity, [0, 1, 0]);
      }
    }
  });
  return specificity;
}

function specificity(selector, context) {
  return specificityForAst(
    cssTree.parse(selector, { context: "selector", positions: false }),
    context,
  );
}

export function buildExternalStylesheet({ source, filename = "+page.svelte" }) {
  const style = sourceStyle(source, filename);
  const ast = cssTree.parse(style, {
    context: "stylesheet",
    positions: true,
  });
  const edits = [];
  ast.children.forEach((node) => {
    assert.equal(
      node.type,
      "Rule",
      `${filename} source style contains unsupported ${node.type}.`,
    );
    assert.equal(
      node.prelude.type,
      "SelectorList",
      "Style contract accepts selector rules only.",
    );
    node.prelude.children.forEach((selector) => {
      const rendered = cssTree.generate(selector);
      assert.ok(selector.loc, `Selector ${rendered} has no source location.`);
      if (rendered === ":root") return;
      if (rendered === ":global(body)") {
        edits.push({
          start: selector.loc.start.offset,
          end: selector.loc.end.offset,
          text: "body",
        });
        return;
      }
      edits.push({
        start: selector.loc.start.offset,
        end: selector.loc.start.offset,
        text: ":root ",
      });
    });
  });
  return edits
    .sort((left, right) => right.start - left.start)
    .reduce(
      (candidate, edit) =>
        `${candidate.slice(0, edit.start)}${edit.text}${candidate.slice(edit.end)}`,
      style,
    );
}

export function verifyPageStyleContract({
  oracleSource,
  candidateCss,
  filename = "+page.svelte",
}) {
  const sourceRules = rulesFromCss(
    sourceStyle(oracleSource, filename),
    `${filename} source style`,
  );
  const compiled = compile(oracleSource, {
    filename,
    generate: "client",
    css: "external",
  });
  assert.ok(
    compiled.css,
    "Svelte compiler did not produce external CSS for the oracle.",
  );
  const compiledRules = rulesFromCss(
    compiled.css.code,
    `${filename} compiler CSS`,
  );
  const candidateRules = rulesFromCss(
    candidateCss,
    "candidate external stylesheet",
  );
  assert.equal(
    candidateRules.length,
    sourceRules.length,
    "Candidate CSS rule count changed.",
  );
  assert.equal(
    compiledRules.length,
    sourceRules.length,
    "Compiler CSS rule count changed.",
  );

  let selectorCount = 0;
  for (let ruleIndex = 0; ruleIndex < sourceRules.length; ruleIndex += 1) {
    const sourceRule = sourceRules[ruleIndex];
    const compiledRule = compiledRules[ruleIndex];
    const candidateRule = candidateRules[ruleIndex];
    assert.deepEqual(
      candidateRule.declarations,
      sourceRule.declarations,
      `Rule ${ruleIndex} declarations changed.`,
    );
    assert.deepEqual(
      compiledRule.declarations,
      sourceRule.declarations,
      `Rule ${ruleIndex} compiler declarations changed.`,
    );
    assert.equal(
      candidateRule.selectors.length,
      sourceRule.selectors.length,
      `Rule ${ruleIndex} selector count changed.`,
    );
    assert.equal(
      compiledRule.selectors.length,
      sourceRule.selectors.length,
      `Rule ${ruleIndex} compiled selector count changed.`,
    );
    for (
      let selectorIndex = 0;
      selectorIndex < sourceRule.selectors.length;
      selectorIndex += 1
    ) {
      selectorCount += 1;
      const sourceSelector = sourceRule.selectors[selectorIndex];
      const expectedCandidate = externalSelector(sourceSelector);
      const candidateSelector = candidateRule.selectors[selectorIndex];
      const compiledSelector = compiledRule.selectors[selectorIndex];
      assert.equal(
        candidateSelector,
        expectedCandidate,
        `Rule ${ruleIndex} selector ${selectorIndex} changed.`,
      );
      assert.equal(
        unwrapScopeSelector(
          compiledSelector,
          `Rule ${ruleIndex} selector ${selectorIndex}`,
        ),
        sourceSelector === ":global(body)" ? "body" : sourceSelector,
        `Rule ${ruleIndex} compiler selector no longer normalizes to source.`,
      );
      assert.deepEqual(
        specificity(candidateSelector, `candidate ${candidateSelector}`),
        specificity(compiledSelector, `compiled ${compiledSelector}`),
        `Rule ${ruleIndex} selector ${selectorIndex} specificity changed.`,
      );
      assert.equal(
        isSpecialSelector(sourceSelector),
        !candidateSelector.startsWith(":root "),
        `Rule ${ruleIndex} selector ${selectorIndex} has the wrong root-prefix policy.`,
      );
    }
  }
  return { ruleCount: sourceRules.length, selectorCount };
}

async function main() {
  const [oraclePath, candidatePath] = process.argv.slice(2);
  if (!oraclePath) {
    throw new Error(
      "Usage: node scripts/page-style-contract.mjs <oracle.svelte> [candidate.css]",
    );
  }
  const oracleSource = await readFile(path.resolve(oraclePath), "utf8");
  const candidateCss = candidatePath
    ? await readFile(path.resolve(candidatePath), "utf8")
    : buildExternalStylesheet({ source: oracleSource, filename: oraclePath });
  console.log(
    JSON.stringify(
      verifyPageStyleContract({
        oracleSource,
        candidateCss,
        filename: oraclePath,
      }),
    ),
  );
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  await main();
}

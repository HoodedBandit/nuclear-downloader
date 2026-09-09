#!/usr/bin/env node

/**
 * Compiler-backed inventory of production frontend callables.
 *
 * Discovery is structural evidence only. It never represents substantive source
 * review and deliberately contains no reviewed/passed status.
 */

import { createHash } from "node:crypto";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
export const REPOSITORY_ROOT = path.resolve(SCRIPT_DIR, "..");
export const FRONTEND_ROOT = path.join(REPOSITORY_ROOT, "nuclear-app");
const requireFromFrontend = createRequire(
  path.join(FRONTEND_ROOT, "package.json"),
);
const ts = requireFromFrontend("typescript");
const { parse: parseSvelte } = requireFromFrontend("svelte/compiler");

const OWNERSHIP = new Map([
  [
    "src/lib/frontend-types.ts",
    {
      responsibility:
        "Define shared renderer presentation types and existing format defaults.",
      workflows: ["queue", "inspection", "settings"],
    },
  ],
  [
    "src/lib/frontend-errors.ts",
    {
      responsibility:
        "Preserve existing user-facing error normalization and diagnostic detail.",
      workflows: ["inspection", "download", "diagnostics"],
    },
  ],
  [
    "src/lib/frontend-workflow-ports.ts",
    {
      responsibility:
        "Declare typed command, operation-wait, and lifetime dependencies.",
      workflows: ["ipc", "startup", "cancellation"],
    },
  ],
  [
    "src/lib/queue-presentation.ts",
    {
      responsibility:
        "Own queue projection, progress presentation, selection, and filename drafts.",
      workflows: ["queue", "download", "state-sync"],
    },
  ],
  [
    "src/lib/inspection-workflow.ts",
    {
      responsibility:
        "Own URL and playlist inspection, admission, cancellation, and their display state.",
      workflows: ["inspection", "queue", "cancellation"],
    },
  ],
  [
    "src/lib/queue-actions.ts",
    {
      responsibility:
        "Own queue command ordering, optimistic cancellation, retries, and settings changes.",
      workflows: ["queue", "download", "cancellation"],
    },
  ],
  [
    "src/lib/runtime-workflow.ts",
    {
      responsibility:
        "Own runtime checks, repair/update workflows, and runtime presentation state.",
      workflows: ["startup", "runtime-update"],
    },
  ],
  [
    "src/lib/app-update-workflow.ts",
    {
      responsibility:
        "Own application version, update checks, installation, and update dialog state.",
      workflows: ["startup", "app-update"],
    },
  ],
  [
    "src/lib/settings-diagnostics-workflow.ts",
    {
      responsibility:
        "Own output/cookie settings and diagnostic export, clear, and copy workflows.",
      workflows: ["startup", "settings", "diagnostics"],
    },
  ],
  [
    "src/routes/+page.svelte",
    {
      responsibility:
        "Compose the main window, user actions, backend workflows, and visible application state.",
      workflows: [
        "startup",
        "inspection",
        "queue",
        "download",
        "cancellation",
        "runtime-update",
        "app-update",
        "diagnostics",
      ],
    },
  ],
  [
    "src/routes/+layout.js",
    {
      responsibility:
        "Declare the renderer-only static application layout mode.",
      workflows: ["startup"],
    },
  ],
  [
    "src/lib/accessible-dialog.ts",
    {
      responsibility:
        "Provide keyboard focus, dismissal, and cleanup behavior for accessible dialogs.",
      workflows: ["dialogs", "accessibility"],
    },
  ],
  [
    "src/lib/app-state-controller.ts",
    {
      responsibility:
        "Own renderer snapshot/delta application and resynchronization sequencing.",
      workflows: ["startup", "state-sync"],
    },
  ],
  [
    "src/lib/backend-state.ts",
    {
      responsibility:
        "Derive stable operation and published-output facts from backend contracts.",
      workflows: ["state-sync", "queue", "download"],
    },
  ],
  [
    "src/lib/ipc-client.ts",
    {
      responsibility:
        "Provide the typed command and event boundary used by renderer workflows.",
      workflows: ["ipc", "state-sync"],
    },
  ],
  [
    "src/lib/operation-reducer.ts",
    {
      responsibility:
        "Order and reduce operation progress without regressing terminal state.",
      workflows: ["download", "cancellation", "state-sync"],
    },
  ],
  [
    "src/lib/operation-wait-registry.ts",
    {
      responsibility:
        "Own bounded renderer waiters for operation completion and teardown.",
      workflows: ["download", "cancellation", "runtime-update", "app-update"],
    },
  ],
  [
    "src/lib/page-lifetime.ts",
    {
      responsibility:
        "Own page resources and suppress callbacks after renderer disposal.",
      workflows: ["startup", "state-sync", "cancellation"],
    },
  ],
  [
    "src/lib/queue-logic.ts",
    {
      responsibility:
        "Validate and derive queue, format, selection, and redacted display behavior.",
      workflows: ["queue", "download", "diagnostics"],
    },
  ],
  [
    "src/lib/startup-state.ts",
    {
      responsibility: "Derive startup readiness and subsystem recovery state.",
      workflows: ["startup", "runtime-update"],
    },
  ],
  [
    "src/lib/state-reconciler.ts",
    {
      responsibility:
        "Coordinate ordered state-delta delivery, gap recovery, and listener disposal.",
      workflows: ["startup", "state-sync"],
    },
  ],
]);

export const EXCLUSIONS = [
  "src/**/*.test.ts and src/**/*.test.svelte",
  "src/lib/bindings/** (generated backend contracts)",
  "src/lib/AccessibleDialogHarness.svelte (declared test support)",
  "source extensions other than .js, .ts, and .svelte",
];

const TEST_SUPPORT_FILES = new Set(["src/lib/AccessibleDialogHarness.svelte"]);

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

function slash(value) {
  return value.split(path.sep).join("/");
}

function isProductionSource(relativePath) {
  return (
    (relativePath.endsWith(".js") ||
      relativePath.endsWith(".ts") ||
      relativePath.endsWith(".svelte")) &&
    !relativePath.endsWith(".test.ts") &&
    !relativePath.endsWith(".test.svelte") &&
    !relativePath.includes("/bindings/") &&
    !TEST_SUPPORT_FILES.has(relativePath)
  );
}

async function walk(directory) {
  const found = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) found.push(...(await walk(absolute)));
    else found.push(absolute);
  }
  return found;
}

function propertyName(node, sourceFile) {
  if (!node) return "<anonymous>";
  if (ts.isIdentifier(node) || ts.isPrivateIdentifier(node)) return node.text;
  if (ts.isStringLiteral(node) || ts.isNumericLiteral(node)) return node.text;
  return node.getText(sourceFile).replace(/\s+/g, " ");
}

function callOwner(node, sourceFile) {
  if (!node || !ts.isCallExpression(node)) return "callback";
  return node.expression.getText(sourceFile).replace(/\s+/g, " ");
}

function callableIdentity(node, sourceFile, owner, ordinal) {
  if (ts.isFunctionDeclaration(node)) {
    return {
      symbol: node.name?.text ?? `<anonymous function ${ordinal}>`,
      classification: "function",
    };
  }
  if (
    ts.isMethodDeclaration(node) ||
    ts.isGetAccessorDeclaration(node) ||
    ts.isSetAccessorDeclaration(node)
  ) {
    const kind = ts.isGetAccessorDeclaration(node)
      ? "getter"
      : ts.isSetAccessorDeclaration(node)
        ? "setter"
        : "method";
    return {
      symbol: propertyName(node.name, sourceFile),
      classification: kind,
    };
  }
  if (ts.isConstructorDeclaration(node)) {
    return { symbol: "constructor", classification: "constructor" };
  }
  const parent = node.parent;
  if (ts.isVariableDeclaration(parent)) {
    return {
      symbol: propertyName(parent.name, sourceFile),
      classification: "function-value",
    };
  }
  if (ts.isPropertyAssignment(parent) || ts.isPropertyDeclaration(parent)) {
    return {
      symbol: propertyName(parent.name, sourceFile),
      classification: "function-value",
    };
  }
  if (ts.isCallExpression(parent)) {
    const argument = parent.arguments.indexOf(node) + 1;
    return {
      symbol: `${callOwner(parent, sourceFile)} callback ${argument}`,
      classification: node.modifiers?.some(
        (modifier) => modifier.kind === ts.SyntaxKind.AsyncKeyword,
      )
        ? "async-callback"
        : "callback",
    };
  }
  return { symbol: `${owner} callback ${ordinal}`, classification: "callback" };
}

function isFunctionLike(node) {
  return (
    ts.isFunctionDeclaration(node) ||
    ts.isMethodDeclaration(node) ||
    ts.isGetAccessorDeclaration(node) ||
    ts.isSetAccessorDeclaration(node) ||
    ts.isConstructorDeclaration(node) ||
    ts.isArrowFunction(node) ||
    ts.isFunctionExpression(node)
  );
}

export function inventoryTypeScript({
  source,
  relativePath,
  sourceOffset = 0,
  fullSource = source,
  scriptContext = "module",
  scriptKind,
}) {
  const kind =
    scriptKind ??
    (relativePath.endsWith(".js") ? ts.ScriptKind.JS : ts.ScriptKind.TS);
  const sourceFile = ts.createSourceFile(
    relativePath,
    source,
    ts.ScriptTarget.Latest,
    true,
    kind,
  );
  if (sourceFile.parseDiagnostics.length > 0) {
    const diagnostic = sourceFile.parseDiagnostics[0];
    const position = sourceFile.getLineAndCharacterOfPosition(
      diagnostic.start ?? 0,
    );
    const message = ts.flattenDiagnosticMessageText(
      diagnostic.messageText,
      " ",
    );
    throw new Error(
      `${relativePath}:${position.line + 1}:${position.character + 1}: TypeScript parse failed: ${message}`,
    );
  }
  const ownership = OWNERSHIP.get(relativePath) ?? {
    responsibility: "Unmapped production frontend responsibility.",
    workflows: ["unmapped"],
  };
  const entries = [];
  let ordinal = 0;

  function visit(node, lexicalOwner) {
    let childOwner = lexicalOwner;
    if (isFunctionLike(node)) {
      ordinal += 1;
      const identity = callableIdentity(
        node,
        sourceFile,
        lexicalOwner,
        ordinal,
      );
      const start = node.getStart(sourceFile);
      const end = node.end;
      const absoluteStart = sourceOffset + start;
      const absoluteEnd = sourceOffset + end;
      const startPosition = sourceFile.getLineAndCharacterOfPosition(start);
      const endPosition = sourceFile.getLineAndCharacterOfPosition(end);
      const owner = lexicalOwner || scriptContext;
      const symbol = identity.symbol;
      entries.push({
        id: `${relativePath}::${owner}::${symbol}@${absoluteStart}-${absoluteEnd}`,
        path: relativePath,
        symbol,
        owner,
        line: startPosition.line + 1 + lineOffset(fullSource, sourceOffset),
        endLine: endPosition.line + 1 + lineOffset(fullSource, sourceOffset),
        startOffset: absoluteStart,
        endOffset: absoluteEnd,
        classification: identity.classification,
        async: Boolean(
          node.modifiers?.some(
            (modifier) => modifier.kind === ts.SyntaxKind.AsyncKeyword,
          ),
        ),
        scriptContext,
        sourceHash: sha256(fullSource),
        spanHash: sha256(fullSource.slice(absoluteStart, absoluteEnd)),
        responsibility: ownership.responsibility,
        workflows: ownership.workflows,
      });
      childOwner = lexicalOwner ? `${lexicalOwner}.${symbol}` : symbol;
    } else if (ts.isClassDeclaration(node) && node.name) {
      childOwner = lexicalOwner
        ? `${lexicalOwner}.${node.name.text}`
        : node.name.text;
    }
    ts.forEachChild(node, (child) => visit(child, childOwner));
  }

  visit(sourceFile, scriptContext);
  return entries;
}

function templateEntry({
  node,
  relativePath,
  source,
  owner,
  symbol,
  classification,
}) {
  const start = node.start;
  const end = node.end;
  const line = lineOffset(source, start) + 1;
  const endLine = lineOffset(source, end) + 1;
  const ownership = OWNERSHIP.get(relativePath);
  return {
    id: `${relativePath}::${owner}::${symbol}@${start}-${end}`,
    path: relativePath,
    symbol,
    owner,
    line,
    endLine,
    startOffset: start,
    endOffset: end,
    classification,
    async: Boolean(node.async),
    scriptContext: "template",
    sourceHash: sha256(source),
    spanHash: sha256(source.slice(start, end)),
    responsibility: ownership.responsibility,
    workflows: ownership.workflows,
  };
}

function expressionLabel(node, source, ancestors, ordinal) {
  for (let index = ancestors.length - 1; index >= 0; index -= 1) {
    const ancestor = ancestors[index];
    if (ancestor.type === "Attribute") return `${ancestor.name} callback`;
    if (ancestor.type === "CallExpression") {
      const argument = ancestor.arguments?.indexOf(node) ?? -1;
      if (argument >= 0)
        return `${source.slice(ancestor.callee.start, ancestor.callee.end)} callback ${argument + 1}`;
    }
  }
  return `template callback ${ordinal}`;
}

function inventorySvelteTemplate({ fragment, source, relativePath }) {
  const entries = [];
  const visited = new WeakSet();
  let ordinal = 0;

  function visit(node, owner, ancestors) {
    if (!node || typeof node !== "object" || visited.has(node)) return;
    visited.add(node);
    let childOwner = owner;
    if (node.type === "SnippetBlock") {
      const symbol = node.expression?.name ?? `snippet ${ordinal + 1}`;
      entries.push(
        templateEntry({
          node,
          relativePath,
          source,
          owner,
          symbol,
          classification: "snippet",
        }),
      );
      childOwner = `${owner}.${symbol}`;
    } else if (
      node.type === "ArrowFunctionExpression" ||
      node.type === "FunctionExpression"
    ) {
      ordinal += 1;
      const symbol = expressionLabel(node, source, ancestors, ordinal);
      entries.push(
        templateEntry({
          node,
          relativePath,
          source,
          owner,
          symbol,
          classification: node.async
            ? "markup-async-callback"
            : "markup-callback",
        }),
      );
      childOwner = `${owner}.${symbol}`;
    }
    for (const [key, value] of Object.entries(node)) {
      if (key === "metadata" || key === "parent") continue;
      if (Array.isArray(value)) {
        for (const child of value)
          visit(child, childOwner, [...ancestors, node]);
      } else {
        visit(value, childOwner, [...ancestors, node]);
      }
    }
  }

  visit(fragment, "template", []);
  return entries;
}

function lineOffset(source, offset) {
  let lines = 0;
  for (let index = 0; index < offset; index += 1) {
    if (source.charCodeAt(index) === 10) lines += 1;
  }
  return lines;
}

export function inventorySvelte({ source, relativePath }) {
  const ast = parseSvelte(source, { filename: relativePath, modern: true });
  const scripts = [
    ["module", ast.module],
    ["instance", ast.instance],
  ];
  const entries = [];
  for (const [context, script] of scripts) {
    if (!script?.content) continue;
    const start = script.content.start;
    const end = script.content.end;
    entries.push(
      ...inventoryTypeScript({
        source: source.slice(start, end),
        relativePath,
        sourceOffset: start,
        fullSource: source,
        scriptContext: context,
      }),
    );
  }
  entries.push(
    ...inventorySvelteTemplate({
      fragment: ast.fragment,
      source,
      relativePath,
    }),
  );
  return entries;
}

export async function buildInventory(frontendRoot = FRONTEND_ROOT) {
  const sourceRoot = path.join(frontendRoot, "src");
  const paths = (await walk(sourceRoot))
    .map((absolute) => ({
      absolute,
      relative: slash(path.relative(frontendRoot, absolute)),
    }))
    .filter(({ relative }) => isProductionSource(relative))
    .sort((left, right) => left.relative.localeCompare(right.relative));
  const files = [];
  const entries = [];
  for (const item of paths) {
    const source = await readFile(item.absolute, "utf8");
    const ownership = OWNERSHIP.get(item.relative);
    if (!ownership)
      throw new Error(
        `Production source lacks an ownership mapping: ${item.relative}`,
      );
    const fileEntries = item.relative.endsWith(".svelte")
      ? inventorySvelte({ source, relativePath: item.relative })
      : inventoryTypeScript({
          source,
          relativePath: item.relative,
          fullSource: source,
          scriptKind: item.relative.endsWith(".js")
            ? ts.ScriptKind.JS
            : ts.ScriptKind.TS,
        });
    files.push({
      path: item.relative,
      sourceHash: sha256(source),
      responsibility: ownership.responsibility,
      workflows: ownership.workflows,
      entries: fileEntries.length,
    });
    entries.push(...fileEntries);
  }
  const ids = new Set();
  for (const entry of entries) {
    if (ids.has(entry.id))
      throw new Error(`Duplicate inventory identity: ${entry.id}`);
    ids.add(entry.id);
  }
  return {
    schemaVersion: 1,
    sourceRoot: "nuclear-app/src",
    generator: "scripts/frontend-source-inventory.mjs",
    parser: {
      typescript: ts.version,
      svelte: requireFromFrontend("svelte/package.json").version,
    },
    reviewSemantics:
      "Automated compiler-backed discovery only. No entry is substantively reviewed by generation.",
    exclusions: EXCLUSIONS,
    summary: {
      files: files.length,
      entries: entries.length,
      asyncCallbacks: entries.filter((entry) =>
        entry.classification.includes("async-callback"),
      ).length,
    },
    files,
    entries,
  };
}

export async function checkInventory(output, frontendRoot = FRONTEND_ROOT) {
  const expected = `${JSON.stringify(await buildInventory(frontendRoot), null, 2)}\n`;
  let actual;
  try {
    actual = await readFile(output, "utf8");
  } catch (error) {
    throw new Error(
      `Frontend inventory is missing or unreadable: ${error.message}`,
    );
  }
  if (actual !== expected) {
    throw new Error(
      "Frontend inventory is stale; regenerate it from the current production source.",
    );
  }
}

function parseArguments(argv) {
  let frontendRoot = FRONTEND_ROOT;
  let output = path.join(
    REPOSITORY_ROOT,
    "docs",
    "frontend-source-inventory.json",
  );
  let check = false;
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === "--root") frontendRoot = path.resolve(argv[++index]);
    else if (argv[index] === "--output") output = path.resolve(argv[++index]);
    else if (argv[index] === "--check") check = true;
    else throw new Error(`Unknown argument: ${argv[index]}`);
  }
  return { frontendRoot, output, check };
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const { frontendRoot, output, check } = parseArguments(process.argv.slice(2));
  if (check) {
    await checkInventory(output, frontendRoot);
    console.log("Frontend source inventory matches current production source.");
  } else {
    const inventory = await buildInventory(frontendRoot);
    await writeFile(output, `${JSON.stringify(inventory, null, 2)}\n`, "utf8");
    console.log(
      `Discovered ${inventory.summary.entries} frontend callables across ${inventory.summary.files} production files.`,
    );
  }
}

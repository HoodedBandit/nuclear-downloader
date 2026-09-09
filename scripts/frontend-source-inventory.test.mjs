import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {
  buildInventory,
  checkInventory,
  inventorySvelte,
  inventoryTypeScript,
  FRONTEND_ROOT
} from './frontend-source-inventory.mjs';

async function removeFixture(root) {
  const resolved = path.resolve(root);
  assert.equal(path.dirname(resolved), path.resolve(os.tmpdir()));
  assert.match(path.basename(resolved), /^frontend-inventory-(?:change-)?[A-Za-z0-9]+$/);
  await rm(resolved, { recursive: true, force: true });
}

test('discovers nested async callbacks with stable lexical owners', () => {
  const entries = inventoryTypeScript({
    relativePath: 'src/lib/ipc-client.ts',
    source:
      'export async function start() { await run(async () => { await stop(async function finish() {}); }); }'
  });
  assert.deepEqual(
    entries.map(({ classification, async, owner }) => ({
      classification,
      async,
      owner
    })),
    [
      { classification: 'function', async: true, owner: 'module' },
      { classification: 'async-callback', async: true, owner: 'module.start' },
      {
        classification: 'async-callback',
        async: true,
        owner: 'module.start.run callback 1'
      }
    ]
  );
});

test('uses the Svelte compiler to discover instance and module script functions', () => {
  const source =
    '<script context="module" lang="ts">export function shared() {}</script>\n<script lang="ts">function local() {}</script>';
  const entries = inventorySvelte({
    source,
    relativePath: 'src/routes/+page.svelte'
  });
  assert.deepEqual(
    entries.map(({ symbol, scriptContext }) => [symbol, scriptContext]),
    [
      ['shared', 'module'],
      ['local', 'instance']
    ]
  );
  assert.equal(entries[1].line, 2);
});

test('discovers markup callbacks, nested async callbacks, and snippets without recounting scripts', () => {
  const source = `<script lang="ts">function scripted() {}</script>
{#snippet row(value)}
  <button onclick={() => run(async () => value)}>Run</button>
{/snippet}`;
  const entries = inventorySvelte({
    source,
    relativePath: 'src/routes/+page.svelte'
  });
  assert.deepEqual(
    entries.map(({ symbol, owner, classification, async }) => ({
      symbol,
      owner,
      classification,
      async
    })),
    [
      {
        symbol: 'scripted',
        owner: 'instance',
        classification: 'function',
        async: false
      },
      {
        symbol: 'row',
        owner: 'template',
        classification: 'snippet',
        async: false
      },
      {
        symbol: 'onclick callback',
        owner: 'template.row',
        classification: 'markup-callback',
        async: false
      },
      {
        symbol: 'run callback 1',
        owner: 'template.row.onclick callback',
        classification: 'markup-async-callback',
        async: true
      }
    ]
  );
  assert.equal(entries[1].line, 2);
  assert.equal(entries[2].line, 3);
});

test('rejects TypeScript recovery parses with syntax diagnostics', () => {
  assert.throws(
    () =>
      inventoryTypeScript({
        relativePath: 'src/lib/ipc-client.ts',
        source: 'function broken('
      }),
    /TypeScript parse failed/
  );
});

test('excludes tests, generated bindings, harnesses, and non-target languages', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'frontend-inventory-'));
  try {
    await mkdir(path.join(root, 'src', 'lib', 'bindings'), { recursive: true });
    await mkdir(path.join(root, 'src', 'routes'), { recursive: true });
    await writeFile(
      path.join(root, 'src', 'lib', 'ipc-client.ts'),
      'export function included() {}'
    );
    await writeFile(
      path.join(root, 'src', 'lib', 'ipc-client.test.ts'),
      'function excludedTest() {}'
    );
    await writeFile(
      path.join(root, 'src', 'lib', 'bindings', 'Generated.ts'),
      'export function excludedBinding() {}'
    );
    await writeFile(
      path.join(root, 'src', 'lib', 'AccessibleDialogHarness.svelte'),
      '<script>function excludedHarness() {}</script>'
    );
    await writeFile(
      path.join(root, 'src', 'routes', '+layout.js'),
      'export function includedJavaScript() {}'
    );
    const inventory = await buildInventory(root);
    assert.deepEqual(
      inventory.files.map((file) => file.path),
      ['src/lib/ipc-client.ts', 'src/routes/+layout.js']
    );
    assert.deepEqual(
      inventory.entries.map((entry) => entry.symbol),
      ['included', 'includedJavaScript']
    );
  } finally {
    await removeFixture(root);
  }
});

test('source and span hashes change when source changes', () => {
  const first = inventoryTypeScript({
    relativePath: 'src/lib/ipc-client.ts',
    source: 'function value() { return 1; }'
  })[0];
  const second = inventoryTypeScript({
    relativePath: 'src/lib/ipc-client.ts',
    source: 'function value() { return 2; }'
  })[0];
  assert.notEqual(first.sourceHash, second.sourceHash);
  assert.notEqual(first.spanHash, second.spanHash);
});

test('check mode detects a source change after inventory generation', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'frontend-inventory-change-'));
  const output = path.join(root, 'inventory.json');
  try {
    await mkdir(path.join(root, 'src', 'lib'), { recursive: true });
    const sourcePath = path.join(root, 'src', 'lib', 'ipc-client.ts');
    await writeFile(sourcePath, 'export function current() { return 1; }');
    const inventory = await buildInventory(root);
    await writeFile(output, `${JSON.stringify(inventory, null, 2)}\n`);
    await checkInventory(output, root);
    await writeFile(sourcePath, 'export function current() { return 2; }');
    await assert.rejects(checkInventory(output, root), /inventory is stale/i);
  } finally {
    await removeFixture(root);
  }
});

test('generated inventory records discovery semantics without review status', async () => {
  const inventory = await buildInventory();
  assert.match(inventory.reviewSemantics, /discovery only/i);
  assert.ok(inventory.entries.length > 0);
  const serialized = JSON.stringify(inventory);
  assert.doesNotMatch(serialized, /"status"\s*:/);
  assert.doesNotMatch(serialized, /"reviewed"\s*:/);
  const packageJson = JSON.parse(await readFile(path.join(FRONTEND_ROOT, 'package.json'), 'utf8'));
  assert.equal(
    packageJson.devDependencies.typescript.startsWith(
      `~${inventory.parser.typescript.split('.').slice(0, 2).join('.')}`
    ),
    true
  );
});

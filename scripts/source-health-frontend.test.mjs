import test from 'node:test';
import assert from 'node:assert/strict';
import { callableLines, dependencyErrors, effectiveLines, validateFrontendContract } from './source-health-frontend.mjs';

test('queue presentation rejects IPC despite comments and braces', () => {
  assert.deepEqual(dependencyErrors('lib/queue-presentation.ts', `// invoke('ignored')\nimport { invoke } from './ipc-client';`), ['queue presentation must not import or call IPC']);
});

test('queue presentation helper modules cannot bypass the IPC boundary', () => {
  assert.deepEqual(dependencyErrors('lib/queue-presentation-helpers.ts', `import { invoke } from './ipc-client';`), ['queue presentation must not import or call IPC']);
});

test('IPC words in comments and strings are not calls', () => {
  assert.deepEqual(dependencyErrors('lib/queue-presentation-helpers.ts', `// invoke('ignored')\nconst text = 'invoke()';`), []);
});

test('workflows reject Svelte component dependencies', () => {
  assert.deepEqual(dependencyErrors('lib/a-workflow.ts', `import View from './components/View.svelte';`), ['workflow modules must not import Svelte components']);
});

test('ordinary modules permit presentation imports', () => {
  assert.deepEqual(dependencyErrors('lib/view.ts', `import View from './components/View.svelte';`), []);
});

test('blank, JavaScript, CSS, and Svelte comments do not consume LOC', () => {
  assert.equal(effectiveLines(`// one\n/* two */\n<!-- three -->\n\nconst value = 1;`), 1);
});

test('compressed statements cannot bypass the callable budget', () => {
  assert.equal(callableLines('work();'.repeat(101)), 101);
});

test('malformed ceilings and unknown exception identities fail closed', () => {
  const contract = { limits: { productionFileLoc: 500, callableLoc: 100, flowNesting: 5 }, frontendDependencies: { exceptions: [{ id: 'mystery', responsibility: 'x', ceiling: '1', why: 'x', alternatives: 'x', tests: ['x'] }] }, frontendExceptions: {} };
  assert.match(validateFrontendContract(contract).join('\n'), /unknown identity/);
  assert.match(validateFrontendContract(contract).join('\n'), /positive integer/);
});

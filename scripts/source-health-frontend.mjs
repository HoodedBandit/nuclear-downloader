#!/usr/bin/env node
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { inventorySvelte, inventoryTypeScript } from './frontend-source-inventory.mjs';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SRC = path.join(ROOT, 'nuclear-app', 'src');
const requireFromFrontend = createRequire(path.join(ROOT, 'nuclear-app', 'package.json'));
const ts = requireFromFrontend('typescript');

export function dependencyErrors(relative, source, exceptions = []) {
  const errors = [];
  const queueOwner = /^lib\/queue-presentation(?:-[a-z0-9-]+)?\.ts$/.test(relative);
  const workflow = /lib\/.*workflow\.ts$/.test(relative);
  if (!queueOwner && !workflow) return errors;
  const tree = ts.createSourceFile(relative, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const imports = [];
  let invokes = 0;
  function visit(node) {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) imports.push(node.moduleSpecifier.text);
    if (ts.isCallExpression(node) && (ts.isIdentifier(node.expression) && node.expression.text === 'invoke' || ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === 'invoke')) invokes += 1;
    ts.forEachChild(node, visit);
  }
  visit(tree);
  if (
    queueOwner &&
    (imports.some((x) => x.includes('ipc-client')) || invokes > 0) &&
    !exceptions.includes('queue-presentation-ipc')
  )
    errors.push('queue presentation must not import or call IPC');
  if (workflow && imports.some((x) => x.endsWith('.svelte') || x.includes('/components/')))
    errors.push('workflow modules must not import Svelte components');
  return errors;
}

function pageDomainBodies(entries) {
  return entries.filter((e) => e.scriptContext !== 'template' && !['callback', 'async-callback'].includes(e.classification) && e.endLine - e.line + 1 > 8);
}

export function effectiveLines(source) {
  source = source.replace(/<!--[\s\S]*?-->/g, (comment) => comment.replace(/[^\n]/g, ' '));
  let block = false;
  return source.split(/\r?\n/).filter((line) => {
    let value = line;
    if (block) { const end = value.indexOf('*/'); if (end < 0) return false; value = value.slice(end + 2); block = false; }
    while (value.includes('/*')) { const start = value.indexOf('/*'); const end = value.indexOf('*/', start + 2); if (end < 0) { value = value.slice(0, start); block = true; break; } value = value.slice(0, start) + value.slice(end + 2); }
    value = value.replace(/\/\/.*$/, '');
    return Boolean(value.trim());
  }).length;
}

export function callableLines(source) {
  return Math.max(effectiveLines(source), (source.match(/;/g) ?? []).length);
}

export function validateFrontendContract(contract) {
  const errors = [];
  const limits = contract?.limits;
  if (!limits || !Number.isInteger(limits.productionFileLoc) || !Number.isInteger(limits.callableLoc) || !Number.isInteger(limits.flowNesting) || Object.values(limits).some((value) => value <= 0))
    errors.push('frontend limits must be positive integers');
  const dependencies = contract?.frontendDependencies?.exceptions;
  const sizes = contract?.frontendExceptions;
  if (!Array.isArray(dependencies)) errors.push('frontend dependency exceptions must be an array');
  if (!sizes || typeof sizes !== 'object' || Array.isArray(sizes)) errors.push('frontend size exceptions must be an object');
  const allowedDependencyIds = new Set(['queue-presentation-ipc']);
  for (const entry of dependencies ?? []) if (!allowedDependencyIds.has(entry?.id) && !entry?.id?.startsWith('page-domain:')) errors.push(`frontend exception ${entry?.id ?? '<missing>'} has an unknown identity`);
  for (const id of Object.keys(sizes ?? {})) if (!/^frontend-(?:file|callable):/.test(id)) errors.push(`frontend exception ${id} has an unknown identity`);
  for (const [id, value] of [...(dependencies ?? []).map((x) => [x.id, x]), ...Object.entries(sizes ?? {})]) {
    for (const field of ['responsibility', 'ceiling', 'why', 'alternatives', 'tests']) if (!value?.[field] || (Array.isArray(value[field]) && value[field].length === 0)) errors.push(`frontend exception ${id} lacks ${field}`);
    if (!Number.isInteger(value?.ceiling) || value.ceiling <= 0) errors.push(`frontend exception ${id} ceiling must be a positive integer`);
    if (!Array.isArray(value?.tests) || !value.tests.every((item) => typeof item === 'string' && item)) errors.push(`frontend exception ${id} tests must be a nonempty string array`);
  }
  return errors;
}

export async function checkFrontend(root = SRC) {
  const contract = JSON.parse(await readFile(path.join(ROOT, 'docs', 'source-health-contract.json'), 'utf8'));
  const contractErrors = validateFrontendContract(contract);
  if (contractErrors.length) return contractErrors;
  const exceptions = contract.frontendDependencies.exceptions;
  const files = [];
  async function walk(dir) {
    for (const entry of await (await import('node:fs/promises')).readdir(dir, { withFileTypes: true })) {
      const absolute = path.join(dir, entry.name);
      if (entry.isDirectory()) await walk(absolute);
      else if (/\.(?:ts|js|svelte)$/.test(entry.name) && !/\.test\.(?:ts|svelte)$/.test(entry.name) && !absolute.includes(`${path.sep}bindings${path.sep}`)) files.push(absolute);
    }
  }
  await walk(root);
  const errors = [];
  const seenExceptions = new Set();
  for (const absolute of files) {
    const relative = path.relative(root, absolute).split(path.sep).join('/');
    const source = await readFile(absolute, 'utf8');
    errors.push(...dependencyErrors(relative, source, exceptions.map((x) => x.id)).map((message) => `${relative}: ${message}`));
    if (/^lib\/queue-presentation(?:-[a-z0-9-]+)?\.ts$/.test(relative)) {
      const reviewed = exceptions.find((x) => x.id === 'queue-presentation-ipc');
      const count = (source.match(/\b(?:dependencies|options)\.invoke\s*\(/g) ?? []).length;
      if (reviewed && count > 0) seenExceptions.add(reviewed.id);
      if (reviewed && count > reviewed.ceiling) errors.push(`queue-presentation-ipc grew to ${count}, ceiling ${reviewed.ceiling}; update requires human review`);
    }
    let entries;
    if (absolute.endsWith('.svelte') && relative === 'routes/+page.svelte') entries = inventorySvelte({ source, relativePath: `src/${relative}` });
    else if (absolute.endsWith('.svelte')) {
      entries = [];
      for (const match of source.matchAll(/<script(?:\s[^>]*)?>([\s\S]*?)<\/script>/g)) entries.push(...inventoryTypeScript({ source: match[1], relativePath: `src/${relative}`, sourceOffset: match.index + match[0].indexOf(match[1]), fullSource: source, scriptContext: 'instance' }));
    } else entries = inventoryTypeScript({ source, relativePath: `src/${relative}` });
    for (const entry of entries) {
      const value = callableLines(source.slice(entry.startOffset, entry.endOffset));
      const id = `frontend-callable:${relative}:${entry.scriptContext}:${entry.symbol}`;
      const exception = contract.frontendExceptions?.[id];
      const ceiling = exception?.ceiling ?? contract.limits.callableLoc;
      if (exception && value > contract.limits.callableLoc) seenExceptions.add(id);
      if (value > ceiling) errors.push(`${id} is ${value}, ceiling ${ceiling}${exception ? '; update requires human review' : ''}`);
    }
    const regions = absolute.endsWith('.svelte') ? [...source.matchAll(/<(script|style)(?:\s[^>]*)?>([\s\S]*?)<\/\1>/g)].map((m) => [m[1], m[2]]) : [['module', source]];
    if (absolute.endsWith('.svelte')) regions.push(['template', source.replace(/<(script|style)(?:\s[^>]*)?>[\s\S]*?<\/\1>/g, '')]);
    for (const [region, content] of regions) {
      const value = effectiveLines(content); const id = `frontend-file:${relative}:${region}`;
      const exception = contract.frontendExceptions?.[id]; const ceiling = exception?.ceiling ?? contract.limits.productionFileLoc;
      if (exception && value > contract.limits.productionFileLoc) seenExceptions.add(id);
      if (value > ceiling) errors.push(`${id} is ${value}, ceiling ${ceiling}${exception ? '; update requires human review' : ''}`);
    }
    if (relative === 'routes/+page.svelte') {
      for (const entry of pageDomainBodies(entries)) {
        const reviewed = exceptions.find((x) => x.id === `page-domain:${entry.symbol}`);
        const value = entry.endLine - entry.line + 1;
        if (!reviewed) errors.push(`${relative}:${entry.line}: page composition contains domain callable ${entry.symbol}`);
        else { seenExceptions.add(reviewed.id); if (value > reviewed.ceiling) errors.push(`page-domain:${entry.symbol} grew to ${value}, ceiling ${reviewed.ceiling}; update requires human review`); }
      }
    }
  }
  for (const id of [...exceptions.map((x) => x.id), ...Object.keys(contract.frontendExceptions ?? {})]) if (!seenExceptions.has(id)) errors.push(`stale frontend exception: ${id}`);
  return errors;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const errors = await checkFrontend();
  if (errors.length) { console.error(`Frontend source health failed:\n${errors.join('\n')}`); process.exitCode = 1; }
  else
  console.log('Frontend dependency health passed.');
}

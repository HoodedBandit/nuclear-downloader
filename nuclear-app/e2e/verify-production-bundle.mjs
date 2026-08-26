import { readdir, readFile, stat } from 'node:fs/promises';
import path from 'node:path';

const forbiddenTokens = [
  '__NUCLEAR_WEBDRIVER_RELEASE_STARTUP__',
  '__NUCLEAR_E2E_SNAPSHOT__',
  '__wdio_mocks__'
];
const buildRoot = path.resolve('build');

async function filesUnder(directory) {
  const result = [];
  for (const entry of await readdir(directory)) {
    const candidate = path.join(directory, entry);
    if ((await stat(candidate)).isDirectory()) result.push(...(await filesUnder(candidate)));
    else result.push(candidate);
  }
  return result;
}

for (const file of await filesUnder(buildRoot)) {
  const contents = await readFile(file);
  const text = contents.toString('utf8');
  for (const token of forbiddenTokens) {
    if (text.includes(token)) {
      throw new Error(`Production bundle leaked WebDriver-only token ${token} in ${file}.`);
    }
  }
}

console.log('Production bundle contains no WebDriver gate or mock-registry tokens.');

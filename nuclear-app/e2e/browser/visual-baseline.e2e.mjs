import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { pngDimensions } from './support/png-contract.mjs';
import {
  IDS,
  applyDelta,
  registerRenderer,
  waitForMockCalls
} from './support/renderer-fixture.mjs';
import { playlistInspection, visualFixedTimeMs, visualStates } from './support/visual-fixtures.mjs';

const outputDirectory = process.env.NUCLEAR_VISUAL_OUTPUT_DIRECTORY;
const captureDescribe = outputDirectory ? describe : describe.skip;
const viewports = [
  { width: 800, height: 500 },
  { width: 1000, height: 700 },
  { width: 1440, height: 1000 }
];
const stateIds = Object.keys(visualStates);
const scale = Number(process.env.NUCLEAR_E2E_SCALE);
const targetPercent = scale === 1 ? 100 : scale === 1.5 ? 150 : null;
let previousMocks = [];

function requiredEnvironment(name, pattern) {
  const value = process.env[name];
  assert.ok(value && pattern.test(value), `${name} is missing or malformed.`);
  return value;
}

async function setCssViewport(viewport) {
  // Set the content viewport directly. Resizing the outer Windows frame can
  // round fractional device pixels and miss a CSS dimension at 150% scale.
  await browser.setViewport({ ...viewport, devicePixelRatio: scale });
  const measured = await browser.execute(() => ({ width: innerWidth, height: innerHeight }));
  assert.deepEqual(measured, viewport, 'Could not establish the requested CSS viewport.');
}

async function prepareRenderer(stateId) {
  await browser.refresh();
  const snapshot = visualStates[stateId]();
  const mocks = await registerRenderer(snapshot, previousMocks, async () => {
    await browser.execute((fixedTime) => {
      const NativeDate = Date;
      class FixedDate extends NativeDate {
        constructor(...args) {
          super(...(args.length === 0 ? [fixedTime] : args));
        }
        static now() {
          return fixedTime;
        }
      }
      FixedDate.parse = NativeDate.parse;
      FixedDate.UTC = NativeDate.UTC;
      window.Date = FixedDate;
      const style = document.createElement('style');
      style.dataset.nuclearVisualFreeze = 'true';
      style.textContent = `
      *, *::before, *::after {
        animation: none !important;
        transition: none !important;
        caret-color: transparent !important;
        scroll-behavior: auto !important;
      }
    `;
      document.head.append(style);
    }, visualFixedTimeMs);
  });
  previousMocks = Object.values(mocks);

  await browser.waitUntil(
    async () => {
      const ready = await browser.execute(() => ({
        runtime: document.querySelector('[data-testid="runtime-status"]')?.textContent ?? '',
        version: document.querySelector('.badge.neutral')?.textContent?.trim() ?? '',
        update: [...document.querySelectorAll('button')].some((button) =>
          button.textContent?.includes('Update v0.6.1')
        ),
        output: document.querySelector('#outdir')?.value ?? ''
      }));
      return (
        ready.runtime.includes('Runtime ready') &&
        ready.version.length > 1 &&
        ready.update &&
        ready.output === 'C:\\fixture-output'
      );
    },
    { timeout: 10_000, timeoutMsg: 'Renderer startup fields did not reach their fixture values.' }
  );

  if (stateId === 'playlist-modal') {
    await mocks.begin_inspection.mockResolvedValueOnce({ operationId: IDS.playlistInspection });
    await $('#video-url').setValue('https://fixture.test/playlist');
    await $('button=Add').click();
    await waitForMockCalls(mocks.begin_inspection, 1);
    const delta = {
      schemaVersion: 1,
      sequence: snapshot.latestSequence + 1,
      emittedAtMs: visualFixedTimeMs,
      kind: 'operation_upserted',
      value: playlistInspection
    };
    const next = applyDelta(snapshot, delta);
    await browser.execute((value) => {
      window.__NUCLEAR_E2E_SNAPSHOT__ = value;
    }, next);
    await browser.tauri.emitEvent('app-state-changed', delta);
    await $('[role="dialog"][aria-labelledby="playlist-modal-title"]').waitForDisplayed();
  } else if (stateId === 'update-modal') {
    await $('button=Update v0.6.1').click();
    await $('[role="dialog"][aria-labelledby="update-modal-title"]').waitForDisplayed();
  }

  await browser.executeAsync((done) => {
    Promise.resolve(document.fonts?.ready)
      .then(
        () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))
      )
      .then(() => done());
  });
  await browser.waitUntil(
    async () => {
      const first = await browser.execute(() => document.body.getBoundingClientRect().toJSON());
      await browser.pause(40);
      const second = await browser.execute(() => document.body.getBoundingClientRect().toJSON());
      return JSON.stringify(first) === JSON.stringify(second);
    },
    { timeout: 5_000, timeoutMsg: `Renderer did not settle for ${stateId}.` }
  );
}

async function captureTabOrder(stateId) {
  const modalState = stateId.endsWith('-modal');
  const initial = await browser.execute((useModalRoots) => {
    const main = document.querySelector('main');
    if (!main) throw new Error('Renderer app root <main> is missing.');
    const modalRoots = [...document.querySelectorAll('.modal-layer')];
    const rootKey = (root) => {
      if (root === main) return 'main';
      if (root.querySelector('[aria-labelledby="playlist-modal-title"]')) return 'modal:playlist';
      if (root.querySelector('[aria-labelledby="update-modal-title"]')) return 'modal:update';
      throw new Error('Modal layer has no stable visual-contract identity.');
    };
    const allRoots = [main, ...modalRoots];
    // The backdrop is a sibling of the dialog and is deliberately outside its
    // keyboard trap. Count only controls in the dialog's actual focus scope.
    const traversalRoots = useModalRoots
      ? modalRoots.map((root) => root.querySelector('[role="dialog"]')).filter(Boolean)
      : [main];
    if (useModalRoots && traversalRoots.length !== 1) {
      throw new Error(`Expected one active modal root, received ${traversalRoots.length}.`);
    }
    const containingRoot = (element) => allRoots.find((root) => root.contains(element));
    const keyFor = (element) => {
      const root = containingRoot(element);
      if (!root) return '';
      const stableRootKey = rootKey(root);
      if (element === root) return stableRootKey;
      const parts = [];
      let current = element;
      while (current && current !== root) {
        const tag = current.tagName.toLowerCase();
        const siblings = current.parentElement
          ? [...current.parentElement.children].filter((item) => item.tagName === current.tagName)
          : [];
        parts.unshift(`${tag}:nth-of-type(${siblings.indexOf(current) + 1})`);
        current = current.parentElement;
      }
      return `${stableRootKey}>${parts.join('>')}`;
    };
    const allElements = allRoots.flatMap((root) => [root, ...root.querySelectorAll('*')]);
    const scrollPositions = allElements.map((element) => ({
      key: keyFor(element),
      left: element.scrollLeft,
      top: element.scrollTop
    }));
    const focusableCount = traversalRoots
      .flatMap((root) => [root, ...root.querySelectorAll('*')])
      .filter((element) => {
        const style = getComputedStyle(element);
        return (
          element.tabIndex >= 0 &&
          !element.disabled &&
          style.display !== 'none' &&
          style.visibility !== 'hidden' &&
          Number(style.opacity) !== 0 &&
          element.getClientRects().length > 0
        );
      }).length;
    const activeKey = keyFor(document.activeElement);
    if (useModalRoots && !modalRoots.some((root) => root.contains(document.activeElement))) {
      throw new Error('Modal traversal did not begin inside the active dialog layer.');
    }
    if (!useModalRoots) document.activeElement?.blur?.();
    return {
      activeKey,
      focusableCount,
      windowScroll: { left: window.scrollX, top: window.scrollY },
      scrollPositions
    };
  }, modalState);

  assert.ok(initial.focusableCount > 0, `${stateId} has no keyboard-focusable controls.`);
  const strictBound = Math.min(initial.focusableCount + 1, 2001);
  const tabOrder = [];
  let traversalComplete = false;
  for (let index = 0; index < strictBound; index += 1) {
    await browser.keys(['Tab']);
    const focused = await browser.execute((useModalRoots) => {
      const main = document.querySelector('main');
      const modalRoots = [...document.querySelectorAll('.modal-layer')];
      const active = document.activeElement;
      if (!main || !(active instanceof Element)) return { key: '', inside: false };
      const roots = useModalRoots ? modalRoots : [main];
      const root = roots.find((candidate) => candidate.contains(active));
      if (!root) return { key: '', inside: false };
      if (useModalRoots && !root.querySelector('[role="dialog"]')?.contains(active)) {
        return { key: '', inside: false };
      }
      const rootKey =
        root === main
          ? 'main'
          : root.querySelector('[aria-labelledby="playlist-modal-title"]')
            ? 'modal:playlist'
            : root.querySelector('[aria-labelledby="update-modal-title"]')
              ? 'modal:update'
              : '';
      if (!rootKey) throw new Error('Focused modal layer has no stable identity.');
      const parts = [];
      let current = active;
      while (current && current !== root) {
        const tag = current.tagName.toLowerCase();
        const siblings = current.parentElement
          ? [...current.parentElement.children].filter((item) => item.tagName === current.tagName)
          : [];
        parts.unshift(`${tag}:nth-of-type(${siblings.indexOf(current) + 1})`);
        current = current.parentElement;
      }
      const style = getComputedStyle(active);
      return {
        key: current === root ? `${rootKey}>${parts.join('>')}` : '',
        inside: current === root,
        visible:
          style.display !== 'none' &&
          style.visibility !== 'hidden' &&
          Number(style.opacity) !== 0 &&
          active.getClientRects().length > 0,
        enabled: !('disabled' in active) || !active.disabled
      };
    }, modalState);
    if (!focused.inside) {
      assert.equal(modalState, false, `${stateId} Tab traversal escaped the dialog focus trap.`);
      assert.equal(
        tabOrder.length,
        initial.focusableCount,
        `${stateId} left the app before visiting every Tab stop.`
      );
      traversalComplete = true;
      break;
    }
    assert.ok(
      focused.visible && focused.enabled,
      `${stateId} focused a hidden or disabled control.`
    );
    const focusedKey = focused.key;
    const firstIndex = tabOrder.indexOf(focusedKey);
    if (firstIndex !== -1) {
      assert.equal(firstIndex, 0, `${stateId} Tab traversal repeated a non-initial key.`);
      if (modalState) tabOrder.push(focusedKey);
      traversalComplete = true;
      break;
    }
    tabOrder.push(focusedKey);
  }
  assert.ok(
    traversalComplete,
    `${stateId} did not complete Tab traversal within its strict bound.`
  );
  assert.ok(tabOrder.length > 0, `${stateId} produced an empty Tab order.`);
  if (modalState) {
    assert.ok(tabOrder.length > 1, `${stateId} did not demonstrate modal focus wrapping.`);
    assert.equal(tabOrder.at(-1), tabOrder[0], `${stateId} did not wrap to its first Tab stop.`);
    assert.equal(
      tabOrder.length,
      initial.focusableCount + 1,
      `${stateId} did not visit every dialog Tab stop before wrapping.`
    );
  } else {
    assert.equal(new Set(tabOrder).size, tabOrder.length, `${stateId} Tab order is not unique.`);
    assert.equal(
      tabOrder.length,
      initial.focusableCount,
      `${stateId} did not visit every app Tab stop.`
    );
  }

  await browser.execute((saved) => {
    const main = document.querySelector('main');
    if (!main) throw new Error('Renderer app root <main> is missing during focus restoration.');
    const roots = new Map([['main', main]]);
    for (const layer of document.querySelectorAll('.modal-layer')) {
      if (layer.querySelector('[aria-labelledby="playlist-modal-title"]'))
        roots.set('modal:playlist', layer);
      if (layer.querySelector('[aria-labelledby="update-modal-title"]'))
        roots.set('modal:update', layer);
    }
    const findByKey = (key) => {
      const separator = key.indexOf('>');
      const rootKey = separator === -1 ? key : key.slice(0, separator);
      const root = roots.get(rootKey);
      if (!root || separator === -1) return root;
      return root.querySelector(`:scope>${key.slice(separator + 1)}`);
    };
    document.activeElement?.blur?.();
    if (saved.activeKey) findByKey(saved.activeKey)?.focus({ preventScroll: true });
    for (const position of saved.scrollPositions) {
      findByKey(position.key)?.scrollTo(position.left, position.top);
    }
    window.scrollTo(saved.windowScroll.left, saved.windowScroll.top);
  }, initial);
  await browser.executeAsync((done) =>
    requestAnimationFrame(() => requestAnimationFrame(() => done()))
  );
  return tabOrder;
}

async function captureSemantic(tabOrder) {
  return browser.execute((observedTabOrder) => {
    const main = document.querySelector('main');
    if (!main) throw new Error('Renderer app root <main> is missing.');
    const roots = [main, ...document.querySelectorAll('.modal-layer')];
    function rootKey(root) {
      if (root === main) return 'main';
      if (root.querySelector('[aria-labelledby="playlist-modal-title"]')) return 'modal:playlist';
      if (root.querySelector('[aria-labelledby="update-modal-title"]')) return 'modal:update';
      throw new Error('Modal layer has no stable visual-contract identity.');
    }
    function keyFor(element) {
      const root = roots.find((candidate) => candidate.contains(element));
      if (!root) return '';
      const stableRootKey = rootKey(root);
      if (element === root) return stableRootKey;
      const parts = [];
      let current = element;
      while (current && current !== root) {
        const tag = current.tagName.toLowerCase();
        const siblings = current.parentElement
          ? [...current.parentElement.children].filter((item) => item.tagName === current.tagName)
          : [];
        parts.unshift(`${tag}:nth-of-type(${siblings.indexOf(current) + 1})`);
        current = current.parentElement;
      }
      return `${stableRootKey}>${parts.join('>')}`;
    }
    function controlFor(element) {
      const tag = element.tagName.toLowerCase();
      const kind =
        tag === 'input'
          ? element.type || 'text'
          : tag === 'select' || tag === 'textarea' || tag === 'button'
            ? tag
            : null;
      const value = 'value' in element ? String(element.value) : null;
      return {
        kind,
        value,
        checked: 'checked' in element ? Boolean(element.checked) : null,
        expanded: element.hasAttribute('aria-expanded')
          ? element.getAttribute('aria-expanded') === 'true'
          : null,
        pressed: element.hasAttribute('aria-pressed')
          ? element.getAttribute('aria-pressed') === 'true'
          : null,
        selected: 'selected' in element ? Boolean(element.selected) : null
      };
    }
    const elements = roots.flatMap((root) => [root, ...root.querySelectorAll('*')]);
    if (elements.length > 2000) {
      throw new Error(`Semantic capture has ${elements.length} elements; maximum is 2000.`);
    }
    return {
      activeElementKey:
        document.activeElement instanceof Element &&
        roots.some((root) => root.contains(document.activeElement))
          ? keyFor(document.activeElement)
          : '',
      tabOrder: observedTabOrder,
      elements: elements.map((element) => {
        const rect = element.getBoundingClientRect();
        const style = getComputedStyle(element);
        const visible =
          style.display !== 'none' &&
          style.visibility !== 'hidden' &&
          Number(style.opacity) !== 0 &&
          rect.width > 0 &&
          rect.height > 0;
        const directText = [...element.childNodes]
          .filter((node) => node.nodeType === Node.TEXT_NODE)
          .map((node) => node.textContent)
          .join(' ')
          .replace(/\s+/g, ' ')
          .trim();
        return {
          key: keyFor(element),
          tag: element.tagName.toLowerCase(),
          text: directText,
          rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
          visible,
          enabled: !('disabled' in element) || !element.disabled,
          focused: element === document.activeElement,
          control: controlFor(element)
        };
      })
    };
  }, tabOrder);
}

captureDescribe('Stage 1 renderer visual contract capture', () => {
  it('captures deterministic screenshots and semantic structure', async function () {
    this.timeout(15 * 60_000);
    assert.ok(
      path.isAbsolute(outputDirectory),
      'NUCLEAR_VISUAL_OUTPUT_DIRECTORY must be absolute.'
    );
    assert.ok(targetPercent, 'NUCLEAR_E2E_SCALE must be exactly 1 or 1.5.');
    const sourceCommit = requiredEnvironment('NUCLEAR_VISUAL_SOURCE_COMMIT', /^[0-9a-f]{40}$/);
    const productionHash = requiredEnvironment('NUCLEAR_VISUAL_PRODUCTION_HASH', /^[0-9a-f]{64}$/);
    const osVersion = requiredEnvironment('NUCLEAR_VISUAL_OS_VERSION', /^.+$/);
    const actualWindowsScalePercent = Number(
      requiredEnvironment('NUCLEAR_VISUAL_WINDOWS_SCALE_PERCENT', /^\d+(?:\.\d+)?$/)
    );
    const artifactPath = path.join(outputDirectory, `visual-${targetPercent}.json`);
    assert.equal(existsSync(artifactPath), false, `Refusing to overwrite ${artifactPath}.`);
    const screenshotDirectory = path.join(outputDirectory, 'screenshots');
    mkdirSync(screenshotDirectory, { recursive: true });

    const capabilities = browser.capabilities;
    const browserVersion = capabilities.browserVersion ?? capabilities.version;
    assert.ok(browserVersion, 'The browser did not report its version.');
    const scenarios = [];
    for (const state of stateIds) {
      for (const viewport of viewports) {
        const repeats = [];
        for (let sequence = 1; sequence <= 2; sequence += 1) {
          await setCssViewport(viewport);
          await prepareRenderer(state);
          const actualDeviceScaleFactor = await browser.execute(() => window.devicePixelRatio);
          assert.equal(
            actualDeviceScaleFactor,
            scale,
            `Expected devicePixelRatio ${scale}, received ${actualDeviceScaleFactor}.`
          );
          const file = `${state}-${viewport.width}x${viewport.height}-${targetPercent}-r${sequence}.png`;
          const relativePath = `screenshots/${file}`;
          const screenshotPath = path.join(screenshotDirectory, file);
          assert.equal(
            existsSync(screenshotPath),
            false,
            `Refusing to overwrite ${screenshotPath}.`
          );
          await browser.saveScreenshot(screenshotPath);
          const bytes = readFileSync(screenshotPath);
          const dimensions = pngDimensions(bytes);
          assert.deepEqual(
            dimensions,
            { width: viewport.width * scale, height: viewport.height * scale },
            'Screenshot dimensions do not equal the CSS viewport times devicePixelRatio.'
          );
          assert.ok(
            dimensions.width <= 4096 && dimensions.height <= 4096,
            'Screenshot exceeds 4096px.'
          );
          repeats.push({
            sequence,
            screenshot: {
              path: relativePath,
              sha256: createHash('sha256').update(bytes).digest('hex'),
              ...dimensions
            },
            semantic: await captureSemantic(await captureTabOrder(state))
          });
        }
        scenarios.push({
          id: `${state}@${viewport.width}x${viewport.height}@${targetPercent}`,
          state,
          viewport,
          scaling: {
            targetPercent,
            deviceScaleFactor: scale,
            actualWindowsScalePercent,
            mode: 'emulated'
          },
          repeats
        });
      }
    }

    assert.equal(scenarios.length, 30);
    const artifact = {
      schemaVersion: 'renderer-visual-contract/v1',
      source: {
        commit: sourceCommit,
        productionHash,
        description: 'Frozen production renderer Stage 1 browser capture'
      },
      fixture: { id: 'frontend-stage1', version: '1' },
      environment: {
        os: { name: 'Windows 11', version: osVersion, architecture: 'x64' },
        browser: { name: 'Chrome', version: String(browserVersion), engine: 'Blink' },
        renderer: { name: 'Chrome', version: String(browserVersion) }
      },
      scenarios
    };
    const serialized = `${JSON.stringify(artifact, null, 2)}\n`;
    assert.ok(Buffer.byteLength(serialized) <= 16 * 1024 * 1024, 'Visual contract exceeds 16 MiB.');
    writeFileSync(artifactPath, serialized, { encoding: 'utf8', flag: 'wx' });
  });
});

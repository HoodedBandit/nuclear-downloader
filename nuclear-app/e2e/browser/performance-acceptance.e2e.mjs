import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import path from 'node:path';

const ROW_COUNT = Number(process.env.NUCLEAR_E2E_QUEUE_SIZE ?? '1000');
if (![1, 100, 1000].includes(ROW_COUNT)) {
  throw new Error('NUCLEAR_E2E_QUEUE_SIZE must be 1, 100, or 1000.');
}
const ACTIVE_COUNT = Math.min(5, ROW_COUNT);
const PROGRESS_EVENTS_PER_SECOND = 25;
const TEST_SECONDS = 60;
const EXPECTED_EVENTS = PROGRESS_EVENTS_PER_SECOND * TEST_SECONDS;

function percentile(values, percentileValue) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * percentileValue))];
}

function distribution(values) {
  return {
    samples: values.length,
    p50Ms: percentile(values, 0.5),
    p95Ms: percentile(values, 0.95),
    p99Ms: percentile(values, 0.99)
  };
}

function queueItem(index) {
  const active = index < ACTIVE_COUNT;
  return {
    schemaVersion: 1,
    id: `20000000-0000-4000-8000-${String(index).padStart(12, '0')}`,
    sourceUrl: `https://fixture.test/performance/${index}`,
    title: `Performance row ${String(index).padStart(4, '0')}`,
    availableQualities: ['best', '1080p'],
    hasAudio: true,
    cookieConfig: null,
    format: 'mp4',
    quality: 'best',
    outputDir: 'C:\\fixture-output',
    filenameOverride: null,
    compatConfigPath: null,
    state: active ? 'running' : 'inert',
    latestOperationId: active ? `30000000-0000-4000-8000-${String(index).padStart(12, '0')}` : null,
    createdAtMs: index + 1,
    updatedAtMs: index + 1
  };
}

function activeOperation(index) {
  return {
    schemaVersion: 1,
    id: `30000000-0000-4000-8000-${String(index).padStart(12, '0')}`,
    queueItemId: `20000000-0000-4000-8000-${String(index).padStart(12, '0')}`,
    kind: 'download',
    state: 'running',
    progress: 0,
    phase: 'download',
    sequence: index + 1,
    createdAtMs: index + 1,
    updatedAtMs: index + 1,
    finishedAtMs: null,
    error: null,
    inspectionResult: null,
    publishedOutput: null,
    intendedTerminalOutcome: null,
    correlationId: `performance-${index}`
  };
}

async function preparePerformanceRenderer() {
  const snapshot = {
    schemaVersion: 1,
    queue: Array.from({ length: ROW_COUNT }, (_, index) => queueItem(index)),
    operations: Array.from({ length: ACTIVE_COUNT }, (_, index) => activeOperation(index)),
    runtimeReadiness: 'ready',
    maintenanceActive: false,
    draining: false,
    persistenceHealth: { degraded: false, error: null },
    latestSequence: 1
  };

  await browser.execute((value) => {
    window.__NUCLEAR_E2E_SNAPSHOT__ = value;
  }, snapshot);

  const getSnapshot = await browser.tauri.mock('get_app_snapshot');
  await getSnapshot.mockImplementation(() => window.__NUCLEAR_E2E_SNAPSHOT__);
  const runtime = await browser.tauri.mock('check_downloader_runtime');
  await runtime.mockResolvedValue({
    state: 'ready',
    runtimeVersion: '2026.7.4',
    source: 'fixture',
    updateAvailable: false,
    latestRuntimeVersion: null,
    runtimeDir: 'C:\\fixture-runtime',
    pluginDir: 'C:\\fixture-plugins',
    message: null,
    tools: []
  });
  const runtimeUpdate = await browser.tauri.mock('check_runtime_update');
  await runtimeUpdate.mockResolvedValue({
    updateAvailable: false,
    latestRuntimeVersion: null,
    message: null
  });
  const output = await browser.tauri.mock('default_download_dir');
  await output.mockResolvedValue('C:\\fixture-output');
  const validate = await browser.tauri.mock('validate_output_directory');
  await validate.mockResolvedValue('C:\\fixture-output');
  const update = await browser.tauri.mock('check_app_update');
  await update.mockResolvedValue({
    currentVersion: '0.6.0',
    hasUpdate: false,
    latestVersion: null,
    notes: null,
    publishedAt: null,
    installerName: null
  });

  await browser.waitUntil(
    () => browser.execute(() => typeof window.__NUCLEAR_WEBDRIVER_RELEASE_STARTUP__ === 'function'),
    { timeout: 10_000, timeoutMsg: 'WebDriver performance startup gate was not installed.' }
  );
  await browser.execute(() => window.__NUCLEAR_WEBDRIVER_RELEASE_STARTUP__());
  await browser.waitUntil(
    async () =>
      (await $('.queue').getAttribute('data-queue-count')) === String(ROW_COUNT) &&
      (await $$('tr.queue-item')).length > 0,
    {
      timeout: 30_000,
      timeoutMsg: `Renderer did not project the ${ROW_COUNT}-row virtual queue.`
    }
  );
  assert.ok(
    (await $$('tr.queue-item')).length < 100,
    'The 1,000-row queue was not virtualized to a bounded DOM window.'
  );
}

describe('renderer performance acceptance', () => {
  it(`meets the ${ROW_COUNT}-row sustained progress thresholds`, async () => {
    // Measure the same renderer clock while startup is still gated. This is
    // diagnostic context for frame-budget failures, never a replacement budget.
    const idleFrameDurations = await browser.executeAsync((done) => {
      const durations = [];
      let previous;
      const sample = (timestamp) => {
        if (previous !== undefined) durations.push(timestamp - previous);
        previous = timestamp;
        if (durations.length < 300) requestAnimationFrame(sample);
        else done(durations);
      };
      requestAnimationFrame(sample);
    });
    await preparePerformanceRenderer();

    const metrics = await browser.executeAsync(
      async (expectedEvents, activeCount, seconds, done) => {
        const reducerDurations = [];
        const dispatchDurations = [];
        const stateDispatchDurations = [];
        const frameDurations = [];
        const longTasks = [];
        let observingFrames = true;
        let previousFrame = performance.now();

        const frame = (now) => {
          frameDurations.push(now - previousFrame);
          previousFrame = now;
          if (observingFrames) requestAnimationFrame(frame);
        };
        requestAnimationFrame(frame);

        const longTaskObserver =
          typeof PerformanceObserver !== 'undefined' &&
          PerformanceObserver.supportedEntryTypes.includes('longtask')
            ? new PerformanceObserver((list) => {
                for (const entry of list.getEntries()) longTasks.push(entry.duration);
              })
            : null;
        longTaskObserver?.observe({ entryTypes: ['longtask'] });

        const reducerModule = await import('/src/lib/operation-reducer.ts');
        let reduced = {
          status: 'downloading',
          progress: 0,
          downloadProgress: 0,
          conversionProgress: null,
          phase: 'download',
          speed: '',
          eta: '',
          error: null
        };
        for (let index = 0; index < expectedEvents; index += 1) {
          const started = performance.now();
          reduced = reducerModule.reduceOperationProgress(reduced, {
            status: 'downloading',
            progress: index % 100,
            download_progress: index % 100,
            conversion_progress: null,
            phase: 'download',
            speed: '1 MiB/s',
            eta: '1s',
            error: null
          });
          reducerDurations.push(performance.now() - started);
        }

        const startedAt = performance.now();
        let stateSequence = 1;
        const ticks = expectedEvents / activeCount;
        const tickIntervalMs = (seconds * 1000) / ticks;
        for (let tick = 0; tick < ticks; tick += 1) {
          const target = startedAt + tick * tickIntervalMs;
          const delay = target - performance.now();
          if (delay > 0) await new Promise((resolve) => setTimeout(resolve, delay));
          for (let active = 0; active < activeCount; active += 1) {
            const operationId = `30000000-0000-4000-8000-${String(active).padStart(12, '0')}`;
            const queueItemId = `20000000-0000-4000-8000-${String(active).padStart(12, '0')}`;
            const progress = (tick + active) % 100;
            const operationStarted = performance.now();
            await window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {
              event: 'app-state-changed',
              payload: {
                schemaVersion: 1,
                sequence: ++stateSequence,
                emittedAtMs: Date.now(),
                kind: 'operation_upserted',
                value: {
                  schemaVersion: 1,
                  id: operationId,
                  queueItemId,
                  kind: 'download',
                  state: 'running',
                  progress,
                  phase: 'download',
                  sequence: stateSequence,
                  createdAtMs: active + 1,
                  updatedAtMs: Date.now(),
                  finishedAtMs: null,
                  error: null,
                  inspectionResult: null,
                  publishedOutput: null,
                  intendedTerminalOutcome: null,
                  correlationId: `performance-${active}`
                }
              }
            });
            stateDispatchDurations.push(performance.now() - operationStarted);
            const queueStarted = performance.now();
            await window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {
              event: 'app-state-changed',
              payload: {
                schemaVersion: 1,
                sequence: ++stateSequence,
                emittedAtMs: Date.now(),
                kind: 'queue_item_upserted',
                value: {
                  schemaVersion: 1,
                  id: queueItemId,
                  sourceUrl: `https://fixture.test/performance/${active}`,
                  title: `Performance row ${String(active).padStart(4, '0')}`,
                  availableQualities: ['best', '1080p'],
                  hasAudio: true,
                  cookieConfig: null,
                  format: 'mp4',
                  quality: 'best',
                  outputDir: 'C:\\fixture-output',
                  filenameOverride: null,
                  compatConfigPath: null,
                  state: 'running',
                  latestOperationId: operationId,
                  createdAtMs: active + 1,
                  updatedAtMs: Date.now()
                }
              }
            });
            stateDispatchDurations.push(performance.now() - queueStarted);
            const beforeDispatch = performance.now();
            await window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {
              event: 'download-progress',
              payload: {
                download_id: operationId,
                status: 'downloading',
                progress,
                phase: 'download',
                download_progress: progress,
                conversion_progress: null,
                speed: '1 MiB/s',
                eta: '1s',
                error: null,
                error_code: null,
                error_detail: null,
                filename: null
              }
            });
            dispatchDurations.push(performance.now() - beforeDispatch);
          }
        }

        const input = document.querySelector('#video-url');
        const inputStarted = performance.now();
        input.value = 'https://fixture.test/input-latency';
        input.dispatchEvent(new Event('input', { bubbles: true }));
        await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        const inputToPaint = performance.now() - inputStarted;

        observingFrames = false;
        longTaskObserver?.disconnect();
        done({
          reducerDurations,
          dispatchDurations,
          stateDispatchDurations,
          frameDurations: frameDurations.slice(2),
          longTasks,
          javascriptHeap: performance.memory
            ? {
                usedBytes: performance.memory.usedJSHeapSize,
                totalBytes: performance.memory.totalJSHeapSize,
                limitBytes: performance.memory.jsHeapSizeLimit,
                scope: 'Chromium-reported JavaScript heap; excludes native and GPU memory'
              }
            : null,
          inputToPaint,
          dispatched: dispatchDurations.length,
          elapsed: performance.now() - startedAt
        });
      },
      EXPECTED_EVENTS,
      ACTIVE_COUNT,
      TEST_SECONDS
    );

    const measured = {
      queueSize: ROW_COUNT,
      activeOperations: ACTIVE_COUNT,
      dispatched: metrics.dispatched,
      elapsedMs: metrics.elapsed,
      reducerP95Ms: percentile(metrics.reducerDurations, 0.95),
      progressDispatchP95Ms: percentile(metrics.dispatchDurations, 0.95),
      stateDeltaDispatchP95Ms: percentile(metrics.stateDispatchDurations, 0.95),
      frameP95Ms: percentile(metrics.frameDurations, 0.95),
      inputToPaintMs: metrics.inputToPaint,
      longestTaskMs: metrics.longTasks.length === 0 ? 0 : Math.max(...metrics.longTasks),
      distributions: {
        idleFrame: distribution(idleFrameDurations),
        workloadFrame: distribution(metrics.frameDurations),
        reducer: distribution(metrics.reducerDurations),
        progressDispatch: distribution(metrics.dispatchDurations),
        stateDeltaDispatch: distribution(metrics.stateDispatchDurations)
      },
      javascriptHeap: metrics.javascriptHeap
    };
    console.log(`PERFORMANCE_ACCEPTANCE ${JSON.stringify(measured)}`);
    if (process.env.NUCLEAR_RENDERER_OUTPUT_DIRECTORY) {
      writeFileSync(
        path.join(process.env.NUCLEAR_RENDERER_OUTPUT_DIRECTORY, 'performance.json'),
        `${JSON.stringify({ schemaVersion: 'renderer-performance/v1', measured, raw: { ...metrics, idleFrameDurations } }, null, 2)}\n`,
        { flag: 'wx' }
      );
    }

    assert.equal(metrics.dispatched, EXPECTED_EVENTS, 'Sustained progress event count drifted.');
    assert.ok(
      metrics.elapsed <= 61_000,
      `The ${EXPECTED_EVENTS} progress events did not sustain 25/s for 60s (${metrics.elapsed.toFixed(0)} ms).`
    );
    assert.ok(
      percentile(metrics.reducerDurations, 0.95) < 5,
      `p95 reducer time was ${percentile(metrics.reducerDurations, 0.95).toFixed(2)} ms.`
    );
    assert.ok(
      percentile(metrics.frameDurations, 0.95) < 16.7,
      `p95 frame time was ${percentile(metrics.frameDurations, 0.95).toFixed(2)} ms.`
    );
    assert.ok(
      metrics.inputToPaint < 100,
      `Input-to-paint was ${metrics.inputToPaint.toFixed(2)} ms.`
    );
    assert.ok(
      metrics.longTasks.every((duration) => duration <= 100),
      `Observed a task longer than 100 ms: ${Math.max(...metrics.longTasks).toFixed(2)} ms.`
    );
    assert.ok(
      percentile(metrics.dispatchDurations, 0.95) < 5,
      `p95 progress dispatch/reducer time was ${percentile(metrics.dispatchDurations, 0.95).toFixed(2)} ms.`
    );
    assert.ok(
      percentile(metrics.stateDispatchDurations, 0.95) < 5,
      `p95 authoritative state-delta projection time was ${percentile(metrics.stateDispatchDurations, 0.95).toFixed(2)} ms.`
    );
  });
});

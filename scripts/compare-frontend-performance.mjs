#!/usr/bin/env node

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { lstat, readFile, realpath } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const QUEUES = [1, 100, 1000];
const EXPECTED_EVENTS = 1500;
const METRICS = {
  idleFrame: "idleFrameDurations",
  workloadFrame: "frameDurations",
  reducer: "reducerDurations",
  progressDispatch: "dispatchDurations",
  stateDeltaDispatch: "stateDispatchDurations",
};
const THRESHOLDS = [
  ["elapsed", "<=", 61_000],
  ["reducer.p95", "<", 5],
  ["workloadFrame.p95", "<", 16.7],
  ["inputToPaint", "<", 100],
  ["longestTask", "<=", 100],
  ["progressDispatch.p95", "<", 5],
  ["stateDeltaDispatch.p95", "<", 5],
];

function percentile(values, fraction) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[
    Math.min(sorted.length - 1, Math.floor(sorted.length * fraction))
  ];
}

function distribution(values, label) {
  assert.ok(
    Array.isArray(values) && values.length > 0,
    `${label} must be non-empty.`,
  );
  values.forEach((value) =>
    assert.ok(
      Number.isFinite(value) && value >= 0,
      `${label} has an invalid sample.`,
    ),
  );
  return {
    samples: values.length,
    p50: percentile(values, 0.5),
    p95: percentile(values, 0.95),
    p99: percentile(values, 0.99),
  };
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

async function jsonFile(filename) {
  return JSON.parse(await readFile(filename, "utf8"));
}

function within(root, candidate) {
  const relative = path.relative(root, candidate);
  return (
    relative !== "" &&
    !relative.startsWith(`..${path.sep}`) &&
    relative !== ".." &&
    !path.isAbsolute(relative)
  );
}

async function validateArchivedManifest(
  directory,
  manifestName,
  recorded,
  expectedHash,
) {
  assert.equal(recorded.path, manifestName);
  const manifestPath = path.join(directory, manifestName);
  const manifestBytes = await readFile(manifestPath);
  assert.equal(sha256(manifestBytes), recorded.sha256);
  const manifest = JSON.parse(manifestBytes);
  const kind =
    manifestName === "production-manifest.json" ? "production" : "harness";
  assert.equal(manifest.schemaVersion, "renderer-input-manifest/v1");
  assert.equal(manifest.kind, kind);
  assert.ok(Array.isArray(manifest.files) && manifest.files.length > 0);
  assert.equal(
    sha256(Buffer.from(JSON.stringify(manifest.files))),
    manifest.aggregateHash,
  );
  assert.equal(manifest.aggregateHash, expectedHash);
  const resolvedDirectory = path.resolve(directory);
  const realDirectory = await realpath(directory);
  const seenPaths = new Set();
  for (const entry of manifest.files) {
    assert.ok(typeof entry.path === "string" && entry.path.length > 0);
    assert.equal(entry.archivePath, `inputs/${kind}/${entry.path}`);
    assert.ok(
      !seenPaths.has(entry.path),
      `Duplicate archived input path: ${entry.path}`,
    );
    seenPaths.add(entry.path);
    const archivePath = path.resolve(
      directory,
      ...entry.archivePath.split("/"),
    );
    assert.ok(
      within(resolvedDirectory, archivePath),
      `Archived input escapes run directory: ${entry.archivePath}`,
    );
    let current = directory;
    for (const segment of entry.archivePath.split("/")) {
      current = path.join(current, segment);
      assert.equal(
        (await lstat(current)).isSymbolicLink(),
        false,
        `Archived input uses a reparse link: ${entry.archivePath}`,
      );
    }
    const realArchivePath = await realpath(archivePath);
    assert.ok(
      within(realDirectory, realArchivePath),
      `Archived input escapes run directory: ${entry.archivePath}`,
    );
    const file = await lstat(realArchivePath);
    assert.equal(
      file.isFile(),
      true,
      `Archived input is not a regular file: ${entry.archivePath}`,
    );
    assert.equal(file.size, entry.size);
    assert.equal(sha256(await readFile(realArchivePath)), entry.sha256);
  }
}

function assertHash(value, label) {
  assert.match(
    value,
    /^[0-9a-f]{64}$/,
    `${label} must be a lowercase SHA-256.`,
  );
}

function valueAt(run, metric) {
  return metric.split(".").reduce((value, key) => value[key], run);
}

function thresholdResults(run) {
  return THRESHOLDS.map(([metric, operator, limit]) => {
    const actual = valueAt(run, metric);
    const passed = operator === "<" ? actual < limit : actual <= limit;
    return { metric, operator, limit, actual, passed };
  });
}

function aggregate(values) {
  assert.equal(
    values.length,
    3,
    "Each aggregate requires exactly three repeats.",
  );
  return {
    median: percentile(values, 0.5),
    min: Math.min(...values),
    max: Math.max(...values),
  };
}

export async function validateFrontendPerformanceRun({
  directory,
  side,
  expected,
}) {
  const receiptPath = path.join(directory, "run-receipt.json");
  const metricsPath = path.join(directory, "performance.json");
  const [receiptBytes, metrics] = await Promise.all([
    readFile(receiptPath),
    jsonFile(metricsPath),
  ]);
  const receipt = JSON.parse(receiptBytes);
  assert.ok(typeof receipt.runId === "string" && receipt.runId.length > 0);
  assert.equal(receipt.schemaVersion, "renderer-check-receipt/v1");
  assert.equal(receipt.suite, "performance");
  assert.equal(
    receipt.selectedSpec,
    "e2e/browser/performance-acceptance.e2e.mjs",
  );
  assert.equal(receipt.inputsVerifiedAfter, true);
  assert.equal(receipt.failure, null);
  assert.ok(
    [0, 1].includes(receipt.exitCode),
    "Performance exitCode must be 0 or an assertion failure (1).",
  );
  assert.equal(receipt.scaleFactor, 1);
  assert.equal(receipt.scalingMode, "emulated");
  assert.equal(receipt.environment.windowsBuild, expected.windowsBuild);
  assert.equal(
    receipt.environment.actualWindowsScalePercent,
    expected.actualWindowsScalePercent,
  );
  assert.equal(receipt.source.commit, expected[`${side}Commit`]);
  assert.equal(
    receipt.source.productionHash,
    expected[`${side}ProductionHash`],
  );
  assert.equal(receipt.harness.hash, expected.harnessHash);
  for (const executable of ["node", "browser", "driver"]) {
    assert.equal(
      receipt.executables[executable].sha256,
      expected.executables[executable],
    );
  }
  await validateArchivedManifest(
    directory,
    "production-manifest.json",
    receipt.source.manifest,
    receipt.source.productionHash,
  );
  await validateArchivedManifest(
    directory,
    "harness-manifest.json",
    receipt.harness.manifest,
    receipt.harness.hash,
  );
  assert.equal(metrics.schemaVersion, "renderer-performance/v1");
  const raw = metrics.raw;
  assert.ok(
    raw && typeof raw === "object",
    "performance.json must retain raw metrics.",
  );
  assert.ok(QUEUES.includes(receipt.queueSize));
  assert.equal(metrics.measured.queueSize, receipt.queueSize);
  assert.equal(
    metrics.measured.activeOperations,
    Math.min(5, receipt.queueSize),
  );
  assert.equal(raw.dispatched, EXPECTED_EVENTS);
  assert.equal(metrics.measured.dispatched, EXPECTED_EVENTS);
  assert.ok(Number.isFinite(raw.elapsed) && raw.elapsed > 0);
  assert.equal(raw.reducerDurations.length, EXPECTED_EVENTS);
  assert.equal(raw.dispatchDurations.length, EXPECTED_EVENTS);
  assert.equal(raw.stateDispatchDurations.length, EXPECTED_EVENTS * 2);
  assert.equal(raw.idleFrameDurations.length, 300);
  assert.ok(Number.isFinite(raw.inputToPaint) && raw.inputToPaint >= 0);
  assert.ok(Array.isArray(raw.longTasks));
  raw.longTasks.forEach((duration) =>
    assert.ok(
      Number.isFinite(duration) && duration >= 0,
      "longTasks has an invalid sample.",
    ),
  );
  assert.ok(raw.javascriptHeap && typeof raw.javascriptHeap === "object");
  for (const field of ["usedBytes", "totalBytes", "limitBytes"])
    assert.ok(
      Number.isFinite(raw.javascriptHeap[field]) &&
        raw.javascriptHeap[field] >= 0,
    );
  assert.ok(raw.javascriptHeap.usedBytes <= raw.javascriptHeap.totalBytes);
  assert.ok(raw.javascriptHeap.totalBytes <= raw.javascriptHeap.limitBytes);
  const distributions = Object.fromEntries(
    Object.entries(METRICS).map(([name, rawName]) => [
      name,
      distribution(raw[rawName], rawName),
    ]),
  );
  const longestTask =
    raw.longTasks.length === 0 ? 0 : Math.max(...raw.longTasks);
  assert.equal(metrics.measured.elapsedMs, raw.elapsed);
  assert.equal(metrics.measured.inputToPaintMs, raw.inputToPaint);
  assert.equal(metrics.measured.longestTaskMs, longestTask);
  assert.deepEqual(metrics.measured.javascriptHeap, raw.javascriptHeap);
  for (const [name, computed] of Object.entries(distributions)) {
    assert.deepEqual(metrics.measured.distributions[name], {
      samples: computed.samples,
      p50Ms: computed.p50,
      p95Ms: computed.p95,
      p99Ms: computed.p99,
    });
  }
  assert.equal(metrics.measured.reducerP95Ms, distributions.reducer.p95);
  assert.equal(
    metrics.measured.progressDispatchP95Ms,
    distributions.progressDispatch.p95,
  );
  assert.equal(
    metrics.measured.stateDeltaDispatchP95Ms,
    distributions.stateDeltaDispatch.p95,
  );
  assert.equal(metrics.measured.frameP95Ms, distributions.workloadFrame.p95);
  const result = {
    directory,
    receiptSha256: sha256(receiptBytes),
    runId: receipt.runId,
    queueSize: receipt.queueSize,
    exitCode: receipt.exitCode,
    elapsed: raw.elapsed,
    inputToPaint: raw.inputToPaint,
    longestTask,
    javascriptHeap: raw.javascriptHeap,
    ...distributions,
  };
  result.thresholds = thresholdResults(result);
  result.thresholdsPassed = result.thresholds.every(
    (threshold) => threshold.passed,
  );
  assert.equal(
    receipt.exitCode === 0,
    result.thresholdsPassed,
    "Receipt exit code disagrees with frozen thresholds.",
  );
  return result;
}

function aggregates(runs) {
  const fields = ["elapsed", "inputToPaint", "longestTask"];
  for (const metric of Object.keys(METRICS)) {
    for (const percentileName of ["p50", "p95", "p99"])
      fields.push(`${metric}.${percentileName}`);
  }
  return Object.fromEntries(
    fields.map((field) => [
      field,
      aggregate(runs.map((run) => valueAt(run, field))),
    ]),
  );
}

export async function compareFrontendPerformance(
  manifest,
  manifestDirectory = process.cwd(),
) {
  assert.equal(
    manifest.schemaVersion,
    "frontend-performance-comparison-input/v1",
  );
  const expected = manifest.expected;
  assert.ok(expected && typeof expected === "object");
  for (const field of ["baselineCommit", "candidateCommit"])
    assert.match(expected[field], /^[0-9a-f]{40}$/);
  for (const field of [
    "baselineProductionHash",
    "candidateProductionHash",
    "harnessHash",
  ])
    assertHash(expected[field], field);
  for (const executable of ["node", "browser", "driver"])
    assertHash(expected.executables[executable], executable);
  assert.match(expected.windowsBuild, /^\d+\.\d+$/);
  assert.ok(
    Number.isInteger(expected.actualWindowsScalePercent) &&
      expected.actualWindowsScalePercent > 0,
  );

  const allDirectories = [...manifest.baseline, ...manifest.candidate];
  const realDirectories = await Promise.all(
    allDirectories.map((directory) =>
      realpath(path.resolve(manifestDirectory, directory)),
    ),
  );
  assert.equal(
    new Set(realDirectories.map((directory) => directory.toLowerCase())).size,
    realDirectories.length,
    "Run directories must be unique by real path.",
  );

  const output = {
    schemaVersion: "frontend-performance-comparison/v1",
    expected,
    sides: {},
  };
  for (const side of ["baseline", "candidate"]) {
    assert.equal(
      manifest[side].length,
      9,
      `${side} must contain nine run directories.`,
    );
    const runs = await Promise.all(
      manifest[side].map((directory) =>
        validateFrontendPerformanceRun({
          directory: path.resolve(manifestDirectory, directory),
          side,
          expected,
        }),
      ),
    );
    for (const queueSize of QUEUES) {
      const repeats = runs.filter((run) => run.queueSize === queueSize);
      assert.equal(
        repeats.length,
        3,
        `${side} queue ${queueSize} must have three repeats.`,
      );
      output.sides[side] ??= {};
      output.sides[side][queueSize] = {
        runs: repeats,
        aggregates: aggregates(repeats),
        thresholdFailures: repeats.flatMap((run, repeat) =>
          run.thresholds
            .filter((result) => !result.passed)
            .map((result) => ({ repeat: repeat + 1, ...result })),
        ),
      };
    }
  }
  const allRuns = Object.values(output.sides).flatMap((side) =>
    Object.values(side).flatMap((queue) => queue.runs),
  );
  assert.equal(
    new Set(allRuns.map((run) => run.runId)).size,
    allRuns.length,
    "Run IDs must be unique.",
  );
  assert.equal(
    new Set(allRuns.map((run) => run.receiptSha256)).size,
    allRuns.length,
    "Receipt hashes must be unique.",
  );
  output.thresholdsPassed = allRuns.every((run) => run.thresholdsPassed);
  output.qualified = output.thresholdsPassed;
  output.comparisons = Object.fromEntries(
    QUEUES.map((queueSize) => {
      const baseline = output.sides.baseline[queueSize].aggregates;
      const candidate = output.sides.candidate[queueSize].aggregates;
      return [
        queueSize,
        Object.fromEntries(
          Object.keys(baseline).map((metric) => {
            const baselineMedian = baseline[metric].median;
            const candidateMedian = candidate[metric].median;
            return [
              metric,
              {
                baselineMedian,
                candidateMedian,
                delta: candidateMedian - baselineMedian,
                ratio:
                  baselineMedian === 0
                    ? null
                    : candidateMedian / baselineMedian,
              },
            ];
          }),
        ),
      ];
    }),
  );
  return output;
}

async function main() {
  const inputPath = process.argv[2];
  assert.ok(
    inputPath,
    "Usage: node scripts/compare-frontend-performance.mjs <runs.json>",
  );
  const absolute = path.resolve(inputPath);
  const report = await compareFrontendPerformance(
    await jsonFile(absolute),
    path.dirname(absolute),
  );
  console.log(JSON.stringify(report, null, 2));
  if (!report.qualified) process.exitCode = 1;
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  await main();

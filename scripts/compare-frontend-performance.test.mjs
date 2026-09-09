import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { promisify } from "node:util";
import { compareFrontendPerformance } from "./compare-frontend-performance.mjs";

const execFileAsync = promisify(execFile);

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function inputDefinition(kind, content) {
  const relative = `fixture/${kind}.txt`;
  const files = [
    {
      path: relative,
      archivePath: `inputs/${kind}/${relative}`,
      size: Buffer.byteLength(content),
      sha256: sha256(content),
    },
  ];
  return {
    content,
    manifest: {
      schemaVersion: "renderer-input-manifest/v1",
      kind,
      aggregateHash: sha256(JSON.stringify(files)),
      files,
    },
  };
}

const inputs = {
  baselineProduction: inputDefinition("production", "baseline"),
  candidateProduction: inputDefinition("production", "candidate"),
  harness: inputDefinition("harness", "frozen harness"),
};
const expected = {
  baselineCommit: "a".repeat(40),
  candidateCommit: "b".repeat(40),
  baselineProductionHash: inputs.baselineProduction.manifest.aggregateHash,
  candidateProductionHash: inputs.candidateProduction.manifest.aggregateHash,
  harnessHash: inputs.harness.manifest.aggregateHash,
  executables: {
    node: "4".repeat(64),
    browser: "5".repeat(64),
    driver: "6".repeat(64),
  },
  windowsBuild: "26200.9445",
  actualWindowsScalePercent: 100,
};

function distribution(values) {
  const sorted = [...values].sort((left, right) => left - right);
  const at = (fraction) =>
    sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * fraction))];
  return {
    samples: values.length,
    p50Ms: at(0.5),
    p95Ms: at(0.95),
    p99Ms: at(0.99),
  };
}

async function writeArchivedInput(directory, definition) {
  const entry = definition.manifest.files[0];
  const filename = path.join(directory, ...entry.archivePath.split("/"));
  await mkdir(path.dirname(filename), { recursive: true });
  await writeFile(filename, definition.content);
}

async function fixtureRun(root, side, queueSize, repeat, options = {}) {
  const directory = path.join(root, `${side}-${queueSize}-${repeat}`);
  await mkdir(directory);
  const production =
    side === "baseline"
      ? inputs.baselineProduction
      : inputs.candidateProduction;
  await writeArchivedInput(directory, production);
  await writeArchivedInput(directory, inputs.harness);
  const productionManifest = JSON.stringify(production.manifest);
  const harnessManifest = JSON.stringify(inputs.harness.manifest);
  await writeFile(
    path.join(directory, "production-manifest.json"),
    productionManifest,
  );
  await writeFile(
    path.join(directory, "harness-manifest.json"),
    harnessManifest,
  );
  const frame = side === "baseline" ? 20 + repeat : 12 + repeat;
  const raw = {
    reducerDurations: Array(1500).fill(1),
    dispatchDurations: Array(1500).fill(2),
    stateDispatchDurations: Array(3000).fill(3),
    frameDurations: Array(300).fill(frame),
    idleFrameDurations: Array(300).fill(18),
    longTasks: options.invalidLongTask ? [Number.NaN] : [],
    javascriptHeap: {
      usedBytes: 1000 + repeat,
      totalBytes: 2000,
      limitBytes: 3000,
    },
    inputToPaint: 25,
    dispatched: options.dispatched ?? 1500,
    elapsed: 60_000,
  };
  const distributions = {
    idleFrame: distribution(raw.idleFrameDurations),
    workloadFrame: distribution(raw.frameDurations),
    reducer: distribution(raw.reducerDurations),
    progressDispatch: distribution(raw.dispatchDurations),
    stateDeltaDispatch: distribution(raw.stateDispatchDurations),
  };
  const measured = {
    queueSize,
    activeOperations: Math.min(5, queueSize),
    dispatched: raw.dispatched,
    elapsedMs: raw.elapsed,
    reducerP95Ms: distributions.reducer.p95Ms,
    progressDispatchP95Ms: distributions.progressDispatch.p95Ms,
    stateDeltaDispatchP95Ms: distributions.stateDeltaDispatch.p95Ms,
    frameP95Ms: distributions.workloadFrame.p95Ms,
    inputToPaintMs: raw.inputToPaint,
    longestTaskMs: 0,
    distributions,
    javascriptHeap: raw.javascriptHeap,
  };
  await writeFile(
    path.join(directory, "performance.json"),
    JSON.stringify({ schemaVersion: "renderer-performance/v1", measured, raw }),
  );
  const receipt = {
    schemaVersion: "renderer-check-receipt/v1",
    runId: options.runId ?? `${side}-${queueSize}-${repeat}`,
    suite: "performance",
    selectedSpec: "e2e/browser/performance-acceptance.e2e.mjs",
    inputsVerifiedAfter: true,
    failure: null,
    exitCode: side === "candidate" ? 0 : 1,
    scaleFactor: 1,
    scalingMode: "emulated",
    queueSize,
    environment: {
      windowsBuild: options.windowsBuild ?? expected.windowsBuild,
      actualWindowsScalePercent: expected.actualWindowsScalePercent,
    },
    source: {
      commit: expected[`${side}Commit`],
      productionHash: options.sourceHash ?? expected[`${side}ProductionHash`],
      manifest: {
        path: "production-manifest.json",
        sha256: sha256(productionManifest),
      },
    },
    harness: {
      hash: expected.harnessHash,
      manifest: {
        path: "harness-manifest.json",
        sha256: sha256(harnessManifest),
      },
    },
    executables: Object.fromEntries(
      Object.entries(expected.executables).map(([name, hash]) => [
        name,
        { sha256: hash },
      ]),
    ),
  };
  await writeFile(
    path.join(directory, "run-receipt.json"),
    JSON.stringify(receipt),
  );
  if (options.tamperArchive)
    await writeFile(
      path.join(
        directory,
        ...production.manifest.files[0].archivePath.split("/"),
      ),
      "tampered",
    );
  return directory;
}

async function fixtureManifest(optionsForRun) {
  const root = await mkdtemp(path.join(os.tmpdir(), "frontend-performance-"));
  const manifest = {
    schemaVersion: "frontend-performance-comparison-input/v1",
    expected,
    baseline: [],
    candidate: [],
  };
  for (const side of ["baseline", "candidate"])
    for (const queueSize of [1, 100, 1000])
      for (let repeat = 1; repeat <= 3; repeat += 1)
        manifest[side].push(
          await fixtureRun(
            root,
            side,
            queueSize,
            repeat,
            optionsForRun?.({ side, queueSize, repeat }) ?? {},
          ),
        );
  return manifest;
}

test("reports distributions, ranges, failures, and side comparison", async () => {
  const result = await compareFrontendPerformance(await fixtureManifest());
  assert.equal(result.sides.baseline[100].runs[0].workloadFrame.p95, 21);
  assert.deepEqual(result.sides.baseline[100].aggregates["workloadFrame.p95"], {
    median: 22,
    min: 21,
    max: 23,
  });
  assert.equal(result.sides.baseline[100].thresholdFailures.length, 3);
  assert.equal(result.sides.candidate[1000].thresholdFailures.length, 0);
  assert.equal(result.comparisons[100]["workloadFrame.p95"].delta, -8);
  assert.equal(result.thresholdsPassed, false);
  assert.equal(result.qualified, false);
});

test("CLI prints the complete failed report and exits nonzero", async () => {
  const manifest = await fixtureManifest();
  const inputPath = path.join(
    path.dirname(manifest.baseline[0]),
    "comparison-input.json",
  );
  await writeFile(inputPath, JSON.stringify(manifest));
  const script = path.resolve("scripts/compare-frontend-performance.mjs");
  const error = await execFileAsync(process.execPath, [
    script,
    inputPath,
  ]).catch((failure) => failure);
  assert.equal(error.code, 1);
  const report = JSON.parse(error.stdout);
  assert.equal(report.qualified, false);
  assert.equal(
    report.sides.baseline[1].thresholdFailures[0].metric,
    "workloadFrame.p95",
  );
});

test("rejects duplicate repeat directories", async () => {
  const manifest = await fixtureManifest();
  manifest.baseline[1] = manifest.baseline[0];
  await assert.rejects(
    () => compareFrontendPerformance(manifest),
    /unique by real path/,
  );
});

test("rejects archived input tampering", async () => {
  const manifest = await fixtureManifest(({ side, queueSize, repeat }) => ({
    tamperArchive: side === "candidate" && queueSize === 100 && repeat === 2,
  }));
  await assert.rejects(
    () => compareFrontendPerformance(manifest),
    /Expected values to be strictly equal/,
  );
});

test("rejects environment drift", async () => {
  const manifest = await fixtureManifest(({ side, queueSize, repeat }) => ({
    windowsBuild:
      side === "baseline" && queueSize === 1 && repeat === 1
        ? "99999.1"
        : undefined,
  }));
  await assert.rejects(
    () => compareFrontendPerformance(manifest),
    /Expected values to be strictly equal/,
  );
});

test("rejects an invalid long-task sample", async () => {
  const manifest = await fixtureManifest(({ side, queueSize, repeat }) => ({
    invalidLongTask: side === "candidate" && queueSize === 1000 && repeat === 3,
  }));
  await assert.rejects(
    () => compareFrontendPerformance(manifest),
    /longTasks has an invalid sample/,
  );
});

test("rejects duplicate run IDs even when receipt bytes differ", async () => {
  const manifest = await fixtureManifest(({ repeat }) => ({
    runId: repeat <= 2 ? "duplicate" : undefined,
  }));
  await assert.rejects(
    () => compareFrontendPerformance(manifest),
    /Run IDs must be unique/,
  );
});

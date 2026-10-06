#!/usr/bin/env node

import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  rmSync,
  statfsSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { isAbsolute, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const laneNames = new Set(["worker", "integration", "terminal-heavy", "performance"]);
const minimumTempBytes = 1024 * 1024 * 1024;
const maximumUnixSocketBytes = 107;
const defaultLock = "/tmp/puppet-master-e2e-heavy.lock";
const managedTempAlias = "/tmp/puppet-master-e2e-runtime";

export function laneContract(lane, specs = [], targetDir = "target") {
  if (!laneNames.has(lane)) throw new Error(`unknown E2E lane: ${lane}`);
  if (lane === "worker" && specs.length === 0) {
    throw new Error("the worker lane requires at least one focused E2E_SPECS path");
  }
  if (lane !== "worker" && specs.length !== 0) {
    throw new Error(`${lane} always selects its complete suite and does not accept spec paths`);
  }
  for (const spec of specs) {
    if (!/^e2e\/[A-Za-z0-9._/-]+\.spec\.ts$/.test(spec)) {
      throw new Error(`focused spec must be an e2e/*.spec.ts path: ${spec}`);
    }
    if (spec === "e2e/terminal-performance.spec.ts" || spec === "e2e/terminal-latency.spec.ts") {
      throw new Error(`${spec.slice("e2e/".length)} is reserved for the performance lane`);
    }
  }
  const profile = lane === "performance" ? "release" : "e2e";
  const project = lane === "performance" ? "performance" : lane === "terminal-heavy" ? "terminal-heavy" : "functional";
  const absoluteTarget = isAbsolute(targetDir) ? targetDir : resolve(root, targetDir);
  return {
    lane,
    profile,
    project,
    specs,
    targetDir: absoluteTarget,
    pmBinary: join(absoluteTarget, profile, "pm"),
    testAgentBinary: join(absoluteTarget, profile, "pm-testagent"),
  };
}

function command(label, program, args, env) {
  const started = performance.now();
  const result = spawnSync(program, args, { cwd: root, env, stdio: "inherit" });
  process.stdout.write(`[e2e-timing] ${label}: ${((performance.now() - started) / 1_000).toFixed(2)}s\n`);
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function processExists(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

export function acquireLock(path) {
  try {
    const fd = openSync(path, "wx");
    writeFileSync(fd, `${process.pid}\n`);
    closeSync(fd);
    return;
  } catch (error) {
    if (error?.code !== "EEXIST") throw error;
  }
  const owner = Number.parseInt(readFileSync(path, "utf8"), 10);
  if (Number.isInteger(owner) && !processExists(owner)) {
    rmSync(path);
    return acquireLock(path);
  }
  throw new Error(`another browser or heavy build lane holds ${path}${owner ? ` (pid ${owner})` : ""}`);
}

export function validateTempDirectoryPath(path) {
  const longestSocket = join(resolve(path), "pm-browser-test-XXXXXX", "pm.sock");
  if (Buffer.byteLength(longestSocket) > maximumUnixSocketBytes) {
    throw new Error(
      `E2E temp directory is too long for Unix sockets: ${path}. `
      + "Set TMPDIR to a shorter path on the same filesystem.",
    );
  }
}

export function assertTempSpace(path, minimumBytes = minimumTempBytes) {
  const stats = statfsSync(path);
  const available = Number(stats.bavail) * Number(stats.bsize);
  if (available < minimumBytes) {
    const availableMiB = Math.floor(available / (1024 * 1024));
    const requiredMiB = Math.ceil(minimumBytes / (1024 * 1024));
    throw new Error(
      `E2E temp directory ${path} has ${availableMiB} MiB free; at least ${requiredMiB} MiB is required. `
      + "Set TMPDIR to a short path on a filesystem with more free space.",
    );
  }
}

export function runtimeTempContract(contract, base = process.env) {
  if (base.TMPDIR) {
    return { path: resolve(base.TMPDIR), storage: null };
  }
  return {
    path: managedTempAlias,
    storage: join(contract.targetDir, ".pm-e2e-runtime"),
  };
}

function prepareRuntimeTemp(contract) {
  const runtime = runtimeTempContract(contract);
  if (runtime.storage === null) {
    validateTempDirectoryPath(runtime.path);
    assertTempSpace(runtime.path);
    return { path: runtime.path, cleanup: () => {} };
  }

  try {
    rmSync(runtime.storage, { recursive: true, force: true });
    mkdirSync(runtime.storage, { recursive: true });
    assertTempSpace(runtime.storage);
    rmSync(runtime.path, { recursive: true, force: true });
    symlinkSync(runtime.storage, runtime.path, "dir");
    validateTempDirectoryPath(runtime.path);
  } catch (error) {
    rmSync(runtime.path, { recursive: true, force: true });
    rmSync(runtime.storage, { recursive: true, force: true });
    throw error;
  }
  return {
    path: runtime.path,
    cleanup: () => {
      rmSync(runtime.path, { recursive: true, force: true });
      rmSync(runtime.storage, { recursive: true, force: true });
    },
  };
}

export function laneEnvironment(contract, base = process.env, rootDirectory = root) {
  return {
    ...base,
    PM_E2E_LANE: contract.lane,
    PM_E2E_PM_BIN: contract.pmBinary,
    PM_E2E_TESTAGENT_BIN: contract.testAgentBinary,
    PM_WEB_ASSETS_DIR: join(rootDirectory, "web", "dist"),
  };
}

function run(contract) {
  const started = performance.now();
  // The heavy-lane lock must remain repository-global even when a caller moves
  // browser profiles away from a constrained system temp filesystem.
  const lock = process.env.PM_E2E_LOCK ?? defaultLock;
  acquireLock(lock);
  let cleanupRuntime = () => {};
  const release = () => {
    cleanupRuntime();
    cleanupRuntime = () => {};
    if (existsSync(lock) && readFileSync(lock, "utf8").trim() === String(process.pid)) rmSync(lock);
  };
  process.once("exit", release);
  process.once("SIGINT", () => process.exit(130));
  process.once("SIGTERM", () => process.exit(143));

  const runtime = prepareRuntimeTemp(contract);
  cleanupRuntime = runtime.cleanup;
  const env = laneEnvironment(contract, { ...process.env, TMPDIR: runtime.path });
  const pnpm = process.env.PNPM ?? "pnpm";
  const cargo = process.env.CARGO ?? "cargo";
  command("web build", pnpm, ["-F", "puppet-master-web", "run", "build"], env);
  command("Rust build", cargo, ["build", "--profile", contract.profile, "--features", "pm-daemon/testagent", "--bin", "pm", "--bin", "pm-testagent"], env);
  command("Chromium", pnpm, ["-F", "puppet-master-web", "run", `test:e2e:${contract.project}`, ...contract.specs], env);
  process.stdout.write(`[e2e-timing] total: ${((performance.now() - started) / 1_000).toFixed(2)}s\n`);
  release();
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [, , lane, ...specs] = process.argv;
    const contract = laneContract(lane, specs, process.env.CARGO_TARGET_DIR ?? "target");
    if (process.env.PM_E2E_PRINT_CONTRACT === "1") {
      process.stdout.write(`${JSON.stringify(contract)}\n`);
    } else {
      run(contract);
    }
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(2);
  }
}

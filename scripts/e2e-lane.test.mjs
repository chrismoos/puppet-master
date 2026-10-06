import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  acquireLock,
  assertTempSpace,
  laneContract,
  laneEnvironment,
  runtimeTempContract,
  validateTempDirectoryPath,
} from "./e2e-lane.mjs";

test("worker selects focused functional specs and the non-LTO profile", () => {
  const contract = laneContract("worker", ["e2e/terminal-scrollback.spec.ts"], "/cache/item-68");
  assert.equal(contract.profile, "e2e");
  assert.equal(contract.project, "functional");
  assert.equal(contract.pmBinary, "/cache/item-68/e2e/pm");
  assert.deepEqual(contract.specs, ["e2e/terminal-scrollback.spec.ts"]);
});

test("worker requires an explicit functional spec", () => {
  assert.throws(() => laneContract("worker"), /requires at least one focused/);
  assert.throws(
    () => laneContract("worker", ["e2e/terminal-performance.spec.ts"]),
    /reserved for the performance lane/,
  );
  assert.throws(() => laneContract("worker", ["terminal-scrollback"]), /e2e\/\*\.spec\.ts/);
});

test("integration selects every functional spec with the e2e profile", () => {
  const contract = laneContract("integration", [], "/cache/item-68");
  assert.equal(contract.profile, "e2e");
  assert.equal(contract.project, "functional");
  assert.deepEqual(contract.specs, []);
  assert.equal(contract.targetDir, "/cache/item-68");
});

test("terminal-heavy selects only expensive terminal scenarios with the e2e profile", () => {
  const contract = laneContract("terminal-heavy", [], "/cache/item-68");
  assert.equal(contract.profile, "e2e");
  assert.equal(contract.project, "terminal-heavy");
  assert.deepEqual(contract.specs, []);
});

test("performance selects only the performance project and release binary", () => {
  const contract = laneContract("performance", [], "/cache/item-68");
  assert.equal(contract.profile, "release");
  assert.equal(contract.project, "performance");
  assert.equal(contract.testAgentBinary, "/cache/item-68/release/pm-testagent");
});

test("performance scenarios run with one browser worker", () => {
  const manifest = JSON.parse(readFileSync(new URL("../web/package.json", import.meta.url), "utf8"));
  assert.match(manifest.scripts["test:e2e:performance"], /(?:^|\s)--workers=1(?:\s|$)/);
});

test("lane environment wires the selected binaries and external functional assets", () => {
  const contract = laneContract("integration", [], "/cache/item-68");
  assert.deepEqual(laneEnvironment(contract, { KEEP: "yes" }, "/worktree"), {
    KEEP: "yes",
    PM_E2E_LANE: "integration",
    PM_E2E_PM_BIN: "/cache/item-68/e2e/pm",
    PM_E2E_TESTAGENT_BIN: "/cache/item-68/e2e/pm-testagent",
    PM_WEB_ASSETS_DIR: "/worktree/web/dist",
  });
});

test("complete lanes reject ad hoc spec selection", () => {
  assert.throws(
    () => laneContract("integration", ["e2e/session-tabs.spec.ts"]),
    /does not accept spec paths/,
  );
  assert.throws(
    () => laneContract("performance", ["e2e/terminal-performance.spec.ts"]),
    /does not accept spec paths/,
  );
  assert.throws(
    () => laneContract("terminal-heavy", ["e2e/terminal-agent-replay.spec.ts"]),
    /does not accept spec paths/,
  );
});

test("the shared lane lock rejects a live owner and replaces a stale owner", () => {
  const directory = mkdtempSync(join(tmpdir(), "pm-e2e-lane-test-"));
  const lock = join(directory, "lane.lock");
  try {
    writeFileSync(lock, `${process.pid}\n`);
    assert.throws(() => acquireLock(lock), /another browser or heavy build lane/);
    writeFileSync(lock, "2147483647\n");
    acquireLock(lock);
    assert.equal(readFileSync(lock, "utf8"), `${process.pid}\n`);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("temp preflight rejects paths that cannot fit daemon sockets", () => {
  assert.doesNotThrow(() => validateTempDirectoryPath("/tmp"));
  assert.throws(
    () => validateTempDirectoryPath(`/${"nested/".repeat(20)}`),
    /too long for Unix sockets/,
  );
});

test("temp preflight reports insufficient free space before Chromium starts", () => {
  const directory = mkdtempSync(join(tmpdir(), "pm-e2e-lane-test-"));
  try {
    assert.doesNotThrow(() => assertTempSpace(directory, 1));
    assert.throws(
      () => assertTempSpace(directory, Number.MAX_SAFE_INTEGER),
      /at least .* MiB is required/,
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("runtime temp defaults to task storage behind a short socket-safe alias", () => {
  const contract = laneContract("integration", [], "/cache/item-68");
  assert.deepEqual(runtimeTempContract(contract, {}), {
    path: "/tmp/puppet-master-e2e-runtime",
    storage: "/cache/item-68/.pm-e2e-runtime",
  });
  assert.deepEqual(runtimeTempContract(contract, { TMPDIR: "/short/custom" }), {
    path: "/short/custom",
    storage: null,
  });
});

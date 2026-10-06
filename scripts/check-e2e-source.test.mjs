import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { auditSources } from "./check-e2e-source.mjs";

test("rejects ordered names and undocumented sleeps", () => {
  const directory = mkdtempSync(join(tmpdir(), "pm-e2e-source-"));
  try {
    writeFileSync(join(directory, "zz-late.spec.ts"), "await page.waitForTimeout(50);\n");
    assert.deepEqual(auditSources(directory), [
      "zz-late.spec.ts: ordered zz- spec names are forbidden",
      "zz-late.spec.ts:1: replace waitForTimeout with an observable condition, or document why real time is the contract with // e2e-real-time-wait: <reason>",
    ]);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("accepts an explicitly documented real-time contract", () => {
  const directory = mkdtempSync(join(tmpdir(), "pm-e2e-source-"));
  try {
    writeFileSync(join(directory, "animation.spec.ts"), "// e2e-real-time-wait: verify the product's 100ms dismissal delay\nawait page.waitForTimeout(100);\n");
    assert.deepEqual(auditSources(directory), []);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

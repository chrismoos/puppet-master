#!/usr/bin/env node

import { readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));

export function auditSources(directory) {
  const errors = [];
  for (const name of readdirSync(directory).filter((entry) => entry.endsWith(".spec.ts")).sort()) {
    if (name.startsWith("zz-")) errors.push(`${name}: ordered zz- spec names are forbidden`);
    const lines = readFileSync(resolve(directory, name), "utf8").split("\n");
    lines.forEach((line, index) => {
      if (!line.includes("waitForTimeout(")) return;
      const previous = lines[index - 1] ?? "";
      if (!line.includes("e2e-real-time-wait:") && !previous.includes("e2e-real-time-wait:")) {
        errors.push(`${name}:${index + 1}: replace waitForTimeout with an observable condition, or document why real time is the contract with // e2e-real-time-wait: <reason>`);
      }
    });
  }
  return errors;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const errors = auditSources(resolve(root, "web", "e2e"));
  if (errors.length > 0) {
    process.stderr.write(`${errors.join("\n")}\n`);
    process.exitCode = 1;
  } else {
    process.stdout.write("E2E source policy passed\n");
  }
}

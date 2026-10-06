import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const INSTALLER = readFileSync(join(ROOT, "scripts/install.sh"), "utf8");

// Exercises the real PATH-writing code rather than a copy of it, by
// lifting it out of the installer between two fixed landmarks.
function pathFunctions() {
  const start = INSTALLER.indexOf("MARKER=");
  const end = INSTALLER.indexOf("tmp=$(mktemp");
  assert.ok(start > 0 && end > start, "install.sh no longer has the PATH block");
  return INSTALLER.slice(start, end);
}

function runAddToPath(shell, { times = 1 } = {}) {
  const home = mkdtempSync(join(tmpdir(), "pm-install-test-"));
  const script = join(home, "run.sh");
  writeFileSync(
    script,
    `set -eu\nINSTALL_DIR="$HOME/.local/bin"\n${pathFunctions()}\n${"add_to_path\n".repeat(times)}`,
  );
  const stderr = execFileSync("sh", [script], {
    env: { HOME: home, SHELL: shell, PATH: process.env.PATH },
    encoding: "utf8",
    stdio: ["ignore", "ignore", "pipe"],
  });
  return { home, stderr };
}

function read(home, relative) {
  try {
    return readFileSync(join(home, relative), "utf8");
  } catch {
    return null;
  }
}

test("a zsh user gets the directory on PATH in their rc file", () => {
  const { home } = runAddToPath("/bin/zsh");
  const rc = read(home, ".zshrc");
  assert.ok(rc?.includes("added by pm install.sh"));
  assert.ok(rc.includes(".local/bin"));
});

test("a bash user gets it in bashrc", () => {
  const { home } = runAddToPath("/bin/bash");
  assert.ok(read(home, ".bashrc")?.includes(".local/bin"));
});

test("an unrecognised shell falls back to .profile", () => {
  const { home } = runAddToPath("/usr/bin/ksh");
  assert.ok(read(home, ".profile")?.includes(".local/bin"));
});

test("fish gets its own conf.d file and fish syntax", () => {
  const { home } = runAddToPath("/usr/bin/fish");
  const conf = read(home, ".config/fish/conf.d/pm.fish");
  assert.ok(conf?.includes("fish_add_path"), "fish cannot read a POSIX export line");
  assert.ok(!conf.includes("export PATH="));
});

// Re-running the installer must not stack copies in a startup file.
test("running twice appends only once", () => {
  const { home } = runAddToPath("/bin/zsh", { times: 3 });
  const rc = read(home, ".zshrc") ?? "";
  assert.equal(rc.split("added by pm install.sh").length - 1, 1);
});

// The line lands in a file that may already be on PATH, and gets sourced
// again by nested shells, so it has to be inert the second time.
test("the written line does not duplicate an entry already on PATH", () => {
  const { home } = runAddToPath("/bin/zsh");
  const rc = read(home, ".zshrc") ?? "";
  const line = rc.split("\n").find((l) => l.includes("export PATH="));
  assert.ok(line, "expected a PATH line");
  const probe = join(home, "probe.sh");
  writeFileSync(probe, `INSTALL_DIR="$HOME/.local/bin"\n${line}\n${line}\necho "$PATH"`);
  const out = execFileSync("sh", [probe], {
    env: { HOME: home, PATH: "/usr/bin:/bin" },
    encoding: "utf8",
  });
  const count = out.trim().split(":").filter((p) => p.endsWith("/.local/bin")).length;
  assert.equal(count, 1, `PATH gained duplicates: ${out.trim()}`);
});

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const setupScript = join(import.meta.dirname, "ci-rust.sh");

function fixture(t, { installed = true, dependencies = true, uid = "0", downloadStatus = "0", toolchainStatus = "0", certificates = true } = {}) {
  const root = mkdtempSync(join(tmpdir(), "pm-ci-rust-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, "bin");
  const cargoHome = join(root, "cargo home");
  const runnerTemp = join(root, "runner-temp");
  for (const dir of [bin, join(cargoHome, "bin"), runnerTemp]) mkdirSync(dir, { recursive: true });
  const certificateBundle = join(root, "ca-certificates.crt");
  if (certificates) writeFileSync(certificateBundle, "fixture trust bundle", { mode: 0o444 });
  const log = join(root, "commands");
  const githubPath = join(root, "github-path");
  const githubEnv = join(root, "github-env");
  for (const path of [log, githubPath, githubEnv]) writeFileSync(path, "");
  for (const [name, path] of [["sh", "/bin/sh"], ["env", "/usr/bin/env"], ["mktemp", "/usr/bin/mktemp"], ["rm", "/bin/rm"], ["cp", "/bin/cp"]]) {
    symlinkSync(path, join(bin, name));
  }
  function command(name, body) {
    writeFileSync(join(bin, name), `#!/bin/sh\nset -eu\n${body}\n`, { mode: 0o755 });
  }
  for (const name of ["cc", "make", "pkg-config"]) command(name, "exit 0");
  if (dependencies) command("ld.lld", "exit 0");
  command("id", "printf '%s\\n' \"$PM_TEST_UID\"");
  command("apt-get", "printf 'apt-get %s\\n' \"$*\" >> \"$PM_COMMAND_LOG\"");
  command("sudo", `printf 'sudo %s\\n' "$*" >> "$PM_COMMAND_LOG"
[ "$1" = "-n" ]
shift
exec "$@"`);
  const rustupSource = join(root, "rustup-source");
  writeFileSync(rustupSource, `#!/bin/sh
printf 'rustup %s\\n' "$*" >> "$PM_COMMAND_LOG"
if [ "$1" = toolchain ]; then exit "$PM_TOOLCHAIN_STATUS"; fi
`, { mode: 0o755 });
  if (installed) symlinkSync(rustupSource, join(cargoHome, "bin", "rustup"));
  command("curl", `printf 'curl %s\\n' "$*" >> "$PM_COMMAND_LOG"
if [ "$PM_DOWNLOAD_STATUS" != 0 ]; then exit "$PM_DOWNLOAD_STATUS"; fi
while [ "$1" != --output ]; do shift; done
shift
printf '%s\\n' '#!/bin/sh' 'printf "installer %s\\n" "$*" >> "$PM_COMMAND_LOG"' 'cp "$PM_RUSTUP_SOURCE" "$CARGO_HOME/bin/rustup"' > "$1"`);
  const result = spawnSync("/bin/sh", [setupScript], {
    encoding: "utf8",
    env: {
      PATH: bin,
      HOME: root,
      CARGO_HOME: cargoHome,
      RUNNER_TEMP: runnerTemp,
      SSL_CERT_FILE: certificateBundle,
      GITHUB_PATH: githubPath,
      GITHUB_ENV: githubEnv,
      PM_COMMAND_LOG: log,
      PM_TEST_UID: uid,
      PM_DOWNLOAD_STATUS: downloadStatus,
      PM_TOOLCHAIN_STATUS: toolchainStatus,
      PM_RUSTUP_SOURCE: rustupSource,
    },
  });
  return {
    result,
    commands: readFileSync(log, "utf8"),
    path: readFileSync(githubPath, "utf8"),
    environment: readFileSync(githubEnv, "utf8"),
    cargoHome,
    temporaryFiles: readdirSync(runnerTemp),
  };
}

test("reuses rustup and exports stable Rust for later CI steps", (t) => {
  const run = fixture(t);
  assert.equal(run.result.status, 0, run.result.stderr);
  assert.equal(run.commands, "rustup toolchain install stable --profile minimal --component rustfmt --component clippy\nrustup default stable\n");
  assert.equal(run.path, `${run.cargoHome}/bin\n`);
  assert.equal(run.environment, "RUSTUP_TOOLCHAIN=stable\n");
});

test("bootstraps missing rustup with the official installer and cleans the download", (t) => {
  const run = fixture(t, { installed: false });
  assert.equal(run.result.status, 0, run.result.stderr);
  assert.match(run.commands, /curl --proto =https --tlsv1\.2 --fail --silent --show-error https:\/\/sh\.rustup\.rs --output /);
  assert.match(run.commands, /installer -y --no-modify-path --profile minimal --default-toolchain none/);
  assert.match(run.commands, /rustup default stable/);
  assert.deepEqual(run.temporaryFiles, []);
});

for (const uid of ["0", "1000"]) {
  test(`installs missing compiler dependencies with apt for uid ${uid}`, (t) => {
    const run = fixture(t, { dependencies: false, uid });
    assert.equal(run.result.status, 0, run.result.stderr);
    assert.match(run.commands, /apt-get update/);
    assert.match(run.commands, /apt-get install -y --no-upgrade --no-install-recommends build-essential lld pkg-config curl/);
    if (uid === "0") assert.doesNotMatch(run.commands, /sudo/);
    else assert.match(run.commands, /sudo -n apt-get update/);
  });
}

test("download failure stops before installation and cleans the temporary file", (t) => {
  const run = fixture(t, { installed: false, downloadStatus: "22" });
  assert.equal(run.result.status, 22);
  assert.doesNotMatch(run.commands, /installer|rustup toolchain/);
  assert.equal(run.path, "");
  assert.equal(run.environment, "");
  assert.deepEqual(run.temporaryFiles, []);
});

test("toolchain failure does not select a default or export successful setup", (t) => {
  const run = fixture(t, { toolchainStatus: "1" });
  assert.equal(run.result.status, 1);
  assert.doesNotMatch(run.commands, /rustup default/);
  assert.equal(run.path, "");
  assert.equal(run.environment, "");
});

test("preserves an existing certificate bundle while installing missing build tools", (t) => {
  const run = fixture(t, { dependencies: false });
  assert.equal(run.result.status, 0, run.result.stderr);
  assert.doesNotMatch(run.commands, /apt-get install[^\n]*ca-certificates/);
  assert.match(run.commands, /apt-get install[^\n]*--no-upgrade/);
});

test("requests certificate installation when no readable bundle is available", (t) => {
  const run = fixture(t, { dependencies: false, certificates: false });
  assert.equal(run.result.status, 0, run.result.stderr);
  assert.match(run.commands, /apt-get install[^\n]*curl ca-certificates/);
});

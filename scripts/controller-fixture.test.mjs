import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_SPEC,
  ENROLL_TOKEN_MIN_REMAINING_MS,
  LOG_TAIL_BYTES,
  MAX_SOCKET_PATH_BYTES,
  UnrecoverableError,
  assertIsolatedLayout,
  assertLoopbackHost,
  enrollTokenHash,
  enrollTokenRecord,
  failureReport,
  fixtureEnv,
  fixtureLayout,
  guardStdout,
  normalizeSpec,
  parseArgs,
  routeWarningsToStderr,
  renderEnv,
  reproCommand,
  reservedPaths,
  resolveBinaries,
  tailBytes,
  waitForCondition,
} from "./controller-fixture.mjs";

const home = "/home/fixture-user";
const env = {
  XDG_RUNTIME_DIR: "/run/user/1000",
  XDG_DATA_HOME: `${home}/.local/share`,
  XDG_CONFIG_HOME: `${home}/.config`,
};

function temporaryRoot() {
  return mkdtempSync(join(resolve(tmpdir()), "pm-fixture-test-"));
}

test("a fixture layout keeps everything it will delete under one root", () => {
  const root = temporaryRoot();
  try {
    const layout = fixtureLayout(root);
    for (const path of [
      layout.socket,
      layout.db,
      layout.scrollbackDir,
      layout.logFile,
      layout.failureLog,
      layout.statePath,
      layout.binDir,
      layout.agentHome,
      layout.projectDir,
    ]) {
      assert.ok(path.startsWith(`${layout.root}/`), `${path} escapes ${layout.root}`);
    }
    assert.doesNotThrow(() => assertIsolatedLayout(layout, { env, home }));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the default socket, database and transcript directory are refused by name", () => {
  const root = temporaryRoot();
  try {
    const base = fixtureLayout(root);
    const reserved = reservedPaths(env, home);
    assert.throws(
      () => assertIsolatedLayout({ ...base, socket: reserved.sockets[0] }, { env, home }),
      /is the default socket/,
    );
    assert.throws(
      () => assertIsolatedLayout({ ...base, db: reserved.databases[0] }, { env, home }),
      /is the default database/,
    );
    assert.throws(
      () => assertIsolatedLayout({ ...base, scrollbackDir: reserved.scrollbacks[0] }, { env, home }),
      /is the default transcript directory/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a root that overlaps the user's puppet-master directories is refused", () => {
  const reserved = reservedPaths(env, home);
  for (const directory of reserved.directories) {
    assert.throws(
      () => assertIsolatedLayout(fixtureLayout(join(directory, "run")), { env, home, allowAnyRoot: true }),
      /overlaps the puppet-master directory/,
      `${directory} was accepted as a fixture root`,
    );
    assert.throws(
      () => assertIsolatedLayout(fixtureLayout(resolve(directory, "..")), { env, home, allowAnyRoot: true }),
      /overlaps the puppet-master directory/,
      `the parent of ${directory} was accepted as a fixture root`,
    );
  }
});

test("broad and shared roots are refused", () => {
  assert.throws(() => assertIsolatedLayout(fixtureLayout("/"), { env, home }), /filesystem root/);
  assert.throws(() => assertIsolatedLayout(fixtureLayout(home), { env, home }), /home directory/);
  assert.throws(() => assertIsolatedLayout(fixtureLayout("/etc/pm"), { env, home }), /pass an explicit --root/);
  assert.throws(
    () => assertIsolatedLayout({ ...fixtureLayout("/tmp/pm-fixture-x"), root: "relative/root" }, { env, home }),
    /absolute path/,
  );
});

test("a managed path outside the root is refused", () => {
  const root = temporaryRoot();
  try {
    const layout = { ...fixtureLayout(root), db: "/var/lib/elsewhere/pm.db" };
    assert.throws(() => assertIsolatedLayout(layout, { env, home }), /is not inside the fixture root/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a root too long for a Unix socket is refused with the limit named", () => {
  const long = join(resolve(tmpdir()), "x".repeat(MAX_SOCKET_PATH_BYTES));
  assert.throws(
    () => assertIsolatedLayout(fixtureLayout(long), { env, home }),
    new RegExp(`over the ${MAX_SOCKET_PATH_BYTES}-byte Unix socket limit`),
  );
});

test("only loopback bind addresses are accepted", () => {
  assert.equal(assertLoopbackHost("127.0.0.1"), "127.0.0.1");
  assert.equal(assertLoopbackHost("127.9.9.9"), "127.9.9.9");
  assert.equal(assertLoopbackHost("::1"), "::1");
  for (const host of ["0.0.0.0", "192.168.1.4", "::", "localhost", ""]) {
    assert.throws(() => assertLoopbackHost(host), /loopback|non-empty/);
  }
});

test("readiness polls the condition instead of waiting a fixed time", async () => {
  let clock = 0;
  const slept = [];
  let attempts = 0;
  const value = await waitForCondition("a condition that becomes true", () => {
    attempts += 1;
    return attempts === 3 ? "ready" : false;
  }, {
    timeoutMs: 1_000,
    intervalMs: 10,
    now: () => clock,
    sleep: (ms) => { slept.push(ms); clock += ms; return Promise.resolve(); },
  });
  assert.equal(value, "ready");
  assert.equal(attempts, 3);
  assert.deepEqual(slept, [10, 10]);
});

test("readiness reports the deadline, the probe count and the last error", async () => {
  let clock = 0;
  await assert.rejects(
    () => waitForCondition("the controller", () => { throw new Error("connection refused"); }, {
      timeoutMs: 100,
      intervalMs: 25,
      now: () => clock,
      sleep: (ms) => { clock += ms; return Promise.resolve(); },
    }),
    /timed out after 100ms and 5 probes waiting for the controller: connection refused/,
  );
});

test("readiness gives up at once on a condition that can never become true", async () => {
  let attempts = 0;
  await assert.rejects(
    () => waitForCondition("the controller", () => {
      attempts += 1;
      throw new UnrecoverableError("the controller exited during startup with code 1");
    }, { timeoutMs: 60_000, intervalMs: 1 }),
    /exited during startup/,
  );
  assert.equal(attempts, 1);
});

test("the default fixture seeds sessions, items and both decision shapes", () => {
  const spec = normalizeSpec();
  assert.ok(spec.sessions.length >= 1);
  assert.ok(spec.items.length >= 1);
  const single = spec.plans.filter((plan) => plan.decision);
  const batched = spec.plans.filter((plan) => plan.batch);
  assert.equal(single.length, 1);
  assert.equal(batched.length, 1);
  assert.ok(batched[0].batch.decisions.length >= 2);
  for (const plan of spec.plans) {
    assert.ok(spec.sessions.some((session) => session.key === plan.session));
  }
});

test("configuration overrides the default fixture without losing unset fields", () => {
  const spec = normalizeSpec({
    bucket: "custom",
    sessions: [{ key: "only", title: "custom-session" }],
    plans: [{
      key: "p",
      session: "only",
      name: "Custom plan",
      markdownPath: "plan.md",
      decision: { key: "d", title: "Decide", mode: "single", options: [{ key: "a", label: "A" }] },
    }],
    items: [],
  });
  assert.equal(spec.bucket, "custom");
  assert.equal(spec.project.name, DEFAULT_SPEC.project.name);
  assert.equal(spec.mobile.accessTtlMinutes, DEFAULT_SPEC.mobile.accessTtlMinutes);
  // An unset prompt still gives the test agent something to print.
  assert.equal(spec.sessions[0].prompt, "custom-session");
  assert.equal(spec.plans[0].decision.allowCustom, false);
});

test("a fixture shape a consumer could not match on is refused", () => {
  const plan = {
    key: "p",
    session: "a",
    name: "Plan",
    markdownPath: "plan.md",
    decision: { key: "d", title: "Decide", mode: "single", options: [{ key: "x", label: "X" }] },
  };
  const sessions = [{ key: "a", title: "a" }];
  const cases = [
    [{ sessions: [{ key: "a", title: "t" }, { key: "a", title: "u" }] }, /session keys must be unique/],
    [{ sessions: [{ key: "a", title: "t" }, { key: "b", title: "t" }] }, /session titles must be unique/],
    [{ sessions, plans: [{ ...plan, session: "missing" }] }, /which the fixture does not seed/],
    [{ sessions, plans: [{ ...plan, decision: undefined }] }, /carries no decision/],
    [{ sessions, plans: [{ ...plan, batch: { batchKey: "b", decisions: [] } }] }, /both a decision and a batch/],
    [{ sessions, plans: [{ ...plan, markdownPath: "/etc/plan.md" }] }, /must be relative/],
    [{ sessions, plans: [{ ...plan, markdownPath: "../plan.md" }] }, /must not escape/],
    [{ sessions, plans: [{ ...plan, decision: { ...plan.decision, mode: "yes-no" } }] }, /mode must be one of/],
    [{ sessions, plans: [{ ...plan, decision: { ...plan.decision, options: [] } }] }, /needs at least one option/],
    [{ items: [{ key: "i", title: "t", status: "wondering", priority: "high" }] }, /not a board status/],
    [{ items: [{ key: "i", title: "t", status: "inbox", priority: "later" }] }, /priority .* is unknown/],
    [{ project: { directory: "/absolute" } }, /must be relative/],
    [{ mobile: { accessTtlMinutes: 0 } }, /positive integer/],
  ];
  for (const [overrides, expected] of cases) {
    assert.throws(() => normalizeSpec(overrides), expected, `${JSON.stringify(overrides)} was accepted`);
  }
});

test("a batch is held to the daemon's own two-to-eight bound", () => {
  const decision = (key) => ({ key, title: key, mode: "single", options: [{ key: "a", label: "A" }] });
  const withDecisions = (count) => ({
    sessions: [{ key: "a", title: "a" }],
    plans: [{
      key: "p",
      session: "a",
      name: "Plan",
      markdownPath: "plan.md",
      batch: { batchKey: "b", decisions: Array.from({ length: count }, (_, index) => decision(`d${index}`)) },
    }],
  });
  assert.throws(() => normalizeSpec(withDecisions(1)), /between 2 and 8/);
  assert.throws(() => normalizeSpec(withDecisions(9)), /between 2 and 8/);
  assert.doesNotThrow(() => normalizeSpec(withDecisions(2)));
  assert.doesNotThrow(() => normalizeSpec(withDecisions(8)));
  const dialogue = withDecisions(2);
  dialogue.plans[0].batch.decisions[0].mode = "dialogue";
  assert.throws(() => normalizeSpec(dialogue), /mode must be one of single, multiple/);
});

test("a missing binary names the build command rather than skipping", () => {
  const root = temporaryRoot();
  try {
    assert.throws(
      () => resolveBinaries({}, { CARGO_TARGET_DIR: root }, root),
      /cargo build --profile e2e --features pm-daemon\/testagent --bin pm --bin pm-testagent/,
    );
    assert.throws(
      () => resolveBinaries({ pmBin: join(root, "absent") }, {}, root),
      /does not exist/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a failure report names the stage, the preserved root and the repro command", () => {
  const report = failureReport({
    stage: "seeding",
    error: "plan Fixture architecture has no active decision after seeding",
    root: "/tmp/pm-fixture-abc",
    repro: reproCommand(["start"], "/tmp/pm-fixture-abc"),
    logTail: "daemon line one\ndaemon line two\n",
  });
  assert.match(report, /failed during seeding: plan Fixture architecture has no active decision/);
  assert.match(report, /preserved fixture root: \/tmp\/pm-fixture-abc/);
  assert.match(report, /reproduce with: \.\/scripts\/controller-fixture\.mjs start --root \/tmp\/pm-fixture-abc/);
  assert.match(report, /daemon line two/);
});

test("the repro command points at the preserved root, not the one that was asked for", () => {
  assert.equal(
    reproCommand(["start", "--root", "/tmp/asked", "--format", "env"], "/tmp/kept"),
    "./scripts/controller-fixture.mjs start --format env --root /tmp/kept",
  );
  assert.equal(reproCommand([], "/tmp/kept"), "./scripts/controller-fixture.mjs start --root /tmp/kept");
});

test("a failure report says so when the daemon wrote nothing", () => {
  const report = failureReport({ stage: "startup", error: "boom", root: "/tmp/x", repro: "cmd", logTail: "" });
  assert.match(report, /the daemon produced no output/);
});

test("preserved logs are bounded and say what was dropped", () => {
  const long = "y".repeat(LOG_TAIL_BYTES * 2);
  const tail = tailBytes(long);
  assert.ok(Buffer.byteLength(tail) < Buffer.byteLength(long));
  assert.match(tail, /^… \d+ earlier bytes omitted …\n/);
  assert.ok(tail.endsWith("y"));
  assert.equal(tailBytes("short"), "short");
});

test("the emitted environment carries the connection, credentials and fixture ids", () => {
  const state = {
    root: "/tmp/pm-fixture-abc",
    baseUrl: "http://127.0.0.1:5000",
    socket: "/tmp/pm-fixture-abc/pm.sock",
    auth: {
      installationId: "install",
      username: "fixture-1",
      password: "secret",
      sessionCookie: "cookie",
      mobileEnrollToken: "enroll",
      mobileEnrollTokenExpiresAtUnixMs: 1788644415603,
      mobileAccessToken: "access",
      mobileRefreshToken: "refresh",
      mobileDeviceId: "1",
    },
    fixtures: {
      bucketId: 1,
      projectId: 2,
      projectDir: "/tmp/pm-fixture-abc/project",
      sessions: [{ key: "planning", title: "fixture-planning", id: 7 }],
      items: [{ key: "inbox", title: "Fixture inbox item", id: 3 }],
      plans: [{ key: "release", id: 4, decisionKeys: ["region", "notifications"] }],
    },
  };
  const emitted = fixtureEnv(state);
  assert.equal(emitted.PM_FIXTURE_BASE_URL, "http://127.0.0.1:5000");
  assert.equal(emitted.PM_FIXTURE_SOCKET, "/tmp/pm-fixture-abc/pm.sock");
  assert.equal(emitted.PM_FIXTURE_SESSION_IDS, "planning=7");
  assert.equal(emitted.PM_FIXTURE_ITEM_IDS, "inbox=3");
  assert.equal(emitted.PM_FIXTURE_PLAN_IDS, "release=4");
  assert.equal(emitted.PM_FIXTURE_DECISION_KEYS, "release=region|notifications");
  assert.equal(emitted.PM_FIXTURE_MOBILE_ENROLL_TOKEN, "enroll");
  assert.equal(emitted.PM_FIXTURE_MOBILE_ENROLL_TOKEN_EXPIRES_AT, "1788644415603");
  for (const value of Object.values(emitted)) assert.equal(typeof value, "string");
  assert.match(renderEnv(emitted), /^PM_FIXTURE_ROOT="\/tmp\/pm-fixture-abc"$/m);
});

test("the enrollment token is identified the way the controller stores it", () => {
  // sha256("abc"), so the fixture and the daemon agree on the at-rest form.
  assert.equal(enrollTokenHash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
});

test("an enrollment token is only advertised while the controller says it is spendable", () => {
  const now = 1_000_000;
  const rows = [
    { token_hash: enrollTokenHash("spent"), expires_at_unix_ms: now + 600_000, used_at_unix_ms: now - 10 },
    { token_hash: enrollTokenHash("fresh"), expires_at_unix_ms: now + 600_000, used_at_unix_ms: null },
    { token_hash: enrollTokenHash("expiring"), expires_at_unix_ms: now + 1_000, used_at_unix_ms: null },
  ];
  assert.deepEqual(enrollTokenRecord(rows, "absent", now), { known: false, used: false, remainingMs: 0 });
  assert.deepEqual(enrollTokenRecord(rows, "spent", now), { known: true, used: true, remainingMs: 600_000 });
  assert.deepEqual(enrollTokenRecord(rows, "fresh", now), { known: true, used: false, remainingMs: 600_000 });
  assert.ok(enrollTokenRecord(rows, "expiring", now).remainingMs < ENROLL_TOKEN_MIN_REMAINING_MS);
  assert.ok(enrollTokenRecord(rows, "fresh", now).remainingMs > ENROLL_TOKEN_MIN_REMAINING_MS);
});

test("the command surface is small and refuses anything else", () => {
  assert.deepEqual(parseArgs(["start"]).command, "start");
  assert.equal(parseArgs(["enroll-token", "--root", "/tmp/x"]).command, "enroll-token");
  assert.equal(parseArgs(["start", "--format", "env"]).options.format, "env");
  assert.equal(parseArgs(["stop", "--root", "/tmp/x", "--keep"]).options.keep, true);
  assert.throws(() => parseArgs([]), /usage: controller-fixture\.mjs/);
  assert.throws(() => parseArgs(["restart"]), /unknown command restart/);
  assert.throws(() => parseArgs(["start", "--wat"]), /unknown option --wat/);
  assert.throws(() => parseArgs(["start", "--root"]), /--root needs a value/);
  assert.throws(() => parseArgs(["start", "--format", "yaml"]), /--format must be json or env/);
  assert.throws(() => parseArgs(["start", "--timeout-ms", "0"]), /positive number of milliseconds/);
  assert.throws(() => parseArgs(["start", "--http-host", "0.0.0.0"]), /loopback/);
  for (const command of ["seed", "status", "stop", "enroll-token"]) {
    assert.throws(() => parseArgs([command]), new RegExp(`${command} needs --root`));
  }
});

const fixtureScript = fileURLToPath(new URL("./controller-fixture.mjs", import.meta.url));

function collectingStream() {
  const chunks = [];
  return { chunks, text: () => chunks.join(""), write: (chunk) => { chunks.push(String(chunk)); return true; } };
}

/// Runs the real command-line the way a consumer does, with the noise a
/// consumer actually hit injected around it: a preloaded module that
/// writes to stdout and warns from inside an ordinary async call, and
/// Node's own warning redirection aimed at stdout.
function runNoisily(args, root) {
  const noise = join(root, "noise.mjs");
  writeFileSync(noise, [
    "const shout = () => {",
    "  process.emitWarning('noisy fixture warning');",
    "  process.stdout.write('STRAY STDOUT FROM A DEPENDENCY\\n');",
    "  console.log('STRAY CONSOLE LOG');",
    "};",
    "const inner = globalThis.fetch;",
    "globalThis.fetch = (...args) => { shout(); return inner(...args); };",
    "const timer = setInterval(shout, 1);",
    "timer.unref();",
    "",
  ].join("\n"));
  return spawnSync(process.execPath, [fixtureScript, ...args], {
    encoding: "utf8",
    env: {
      ...process.env,
      NODE_OPTIONS: `--import ${JSON.stringify(noise)} --redirect-warnings=/dev/stdout`,
    },
  });
}

test("only the guard's writer reaches stdout", () => {
  const stdout = collectingStream();
  const stderr = collectingStream();
  const writeDocument = guardStdout(stdout, stderr);
  stdout.write("a stray log line\n");
  writeDocument('{"ok":true}\n');
  stdout.write("another stray line\n");
  assert.equal(stdout.text(), '{"ok":true}\n');
  assert.equal(stderr.text(), "a stray log line\nanother stray line\n");
});

test("warnings go to stderr even when Node was told to redirect them", () => {
  const emitter = new EventEmitter();
  const original = () => {};
  emitter.on("warning", original);
  const stderr = collectingStream();
  routeWarningsToStderr(emitter, stderr);
  assert.equal(emitter.listenerCount("warning"), 1);
  emitter.emit("warning", { name: "ExperimentalWarning", message: "SQLite is an experimental feature" });
  assert.equal(stderr.text(), "ExperimentalWarning: SQLite is an experimental feature\n");
});

test("stdout is named once in the script, inside the guard", () => {
  const source = readFileSync(fixtureScript, "utf8");
  assert.equal(source.split("process.stdout").length - 1, 1);
  assert.match(source, /export function guardStdout\(stdout = process\.stdout/);
});

test("stdout stays one parseable document while a dependency writes and warns", () => {
  const root = temporaryRoot();
  try {
    writeFileSync(join(root, "fixture.json"), JSON.stringify({
      version: 1,
      root,
      pid: null,
      baseUrl: "http://127.0.0.1:1",
      socket: join(root, "pm.sock"),
      auth: { installationId: "install" },
      fixtures: { bucketId: 1, projectId: 1, projectDir: root, sessions: [], items: [], plans: [] },
    }));
    // `status` reports a controller that is not answering, so it exits 1
    // having still written its one document.
    const status = runNoisily(["status", "--root", root], root);
    assert.equal(status.status, 1, status.stderr);
    assert.equal(JSON.parse(status.stdout).ready, false);
    assert.doesNotMatch(status.stdout, /STRAY|noisy fixture warning/);
    assert.match(status.stderr, /STRAY STDOUT FROM A DEPENDENCY/);
    assert.match(status.stderr, /STRAY CONSOLE LOG/);
    assert.match(status.stderr, /Warning: noisy fixture warning/);

    const stopped = runNoisily(["stop", "--root", root, "--keep"], root);
    assert.equal(stopped.status, 0, stopped.stderr);
    assert.deepEqual(JSON.parse(stopped.stdout), { root, outcome: "already stopped", removed: false });
    assert.doesNotMatch(stopped.stdout, /STRAY|noisy fixture warning/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a noisy controller writes to the fixture log, never to stdout", () => {
  const root = temporaryRoot();
  try {
    const noisyDaemon = join(root, "noisy-pm");
    writeFileSync(noisyDaemon, "#!/bin/sh\necho CONTROLLER-STDOUT\necho CONTROLLER-STDERR >&2\nexit 3\n");
    chmodSync(noisyDaemon, 0o755);
    const result = runNoisily(
      ["start", "--pm-bin", noisyDaemon, "--testagent-bin", noisyDaemon, "--root", join(root, "run")],
      root,
    );
    assert.equal(result.status, 2);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /failed during startup: the controller exited during startup with code 3/);
    assert.match(result.stderr, /reproduce with: \.\/scripts\/controller-fixture\.mjs start/);
    const log = readFileSync(join(root, "run", "daemon.log"), "utf8");
    assert.match(log, /CONTROLLER-STDOUT/);
    assert.match(log, /CONTROLLER-STDERR/);
    assert.match(readFileSync(join(root, "run", "failure.log"), "utf8"), /CONTROLLER-STDOUT/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

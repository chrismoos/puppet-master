#!/usr/bin/env node

// Starts a real controller in a throwaway directory and seeds it with
// projects, sessions, board items, plans and open decisions, so a client
// test suite (web, mobile, or the iOS UI suite) drives the same daemon a
// user would. Every path and port is explicit: nothing here may read or
// write the developer's own socket, database, or transcripts.

import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  closeSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { createServer } from "node:net";
import { homedir, tmpdir } from "node:os";
import { createRequire } from "node:module";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));

export const STATE_FILE = "fixture.json";
export const FAILURE_LOG_FILE = "failure.log";
export const STATE_VERSION = 1;

export const DEFAULT_READY_TIMEOUT_MS = 30_000;
export const DEFAULT_STOP_TIMEOUT_MS = 10_000;
export const LOG_TAIL_BYTES = 64 * 1024;
const READY_POLL_MS = 50;

/// macOS caps `sun_path` at 104 bytes including the terminator, which is
/// shorter than Linux's 108. A fixture root that works on one host and
/// not the other is worse than a refusal, so the shorter limit applies
/// everywhere.
export const MAX_SOCKET_PATH_BYTES = 103;

const BUILD_PROFILES = ["release", "e2e", "debug"];
const AGENT_SHIM = "codex";
const DEVICE_APP_INSTALLATION_ID = "controller-fixture-device";
const CREDENTIAL_BYTES = 24;

const ITEM_STATUSES = new Set([
  "inbox",
  "planned",
  "in_progress",
  "blocked",
  "blocked_external",
  "done",
  "dropped",
]);
const ITEM_PRIORITIES = new Set(["urgent", "high", "normal", "low"]);
const DECISION_MODES = new Set(["single", "multiple", "dialogue"]);
const BATCH_DECISION_MODES = new Set(["single", "multiple"]);
const LIVE_SESSION_STATES = new Set(["working", "idle", "needs-input", "starting"]);

export const COMMANDS = ["start", "run", "seed", "status", "stop", "enroll-token"];

/// A token handed to a client must still be valid when that client
/// finally launches, so one this close to expiry is refused rather than
/// advertised.
export const ENROLL_TOKEN_MIN_REMAINING_MS = 60_000;

/// The fixture a consumer gets when it passes no configuration. Every
/// name and key here is part of the contract client suites assert on.
export const DEFAULT_SPEC = {
  bucket: "controller-fixture",
  project: { name: "controller-fixture project", directory: "project" },
  agent: AGENT_SHIM,
  mobile: { accessTtlMinutes: 240, enrollTtlMinutes: 240 },
  sessions: [
    {
      key: "planning",
      title: "fixture-planning",
      prompt: "hold the planning conversation",
    },
    {
      key: "worker",
      title: "fixture-worker",
      prompt: "carry out the fixture work",
    },
  ],
  items: [
    {
      key: "inbox",
      title: "Fixture inbox item",
      status: "inbox",
      priority: "normal",
      body: "An unsorted item, as the board holds one before triage.",
    },
    {
      key: "planned",
      title: "Fixture planned item",
      status: "planned",
      priority: "high",
      body: "A triaged item with a body long enough to wrap in a detail pane.",
    },
  ],
  plans: [
    {
      key: "architecture",
      session: "planning",
      name: "Fixture architecture",
      summary: "Agree on the durable shape of the fixture system",
      markdownPath: "docs/fixture-architecture.md",
      markdown:
        "# Fixture architecture\n\n"
        + "The controller, its store, and the clients that read it.\n\n"
        + "## Open question\n\nWhich store the deployment defaults to.\n",
      decision: {
        key: "store",
        title: "Choose the primary store",
        promptMarkdown: "Pick the default that best fits the deployment.",
        detailMarkdown: "This choice drives the next planning question.",
        mode: "single",
        allowCustom: true,
        options: [
          { key: "postgres", label: "Postgres", detailMarkdown: "Strong relational guarantees." },
          { key: "sqlite", label: "SQLite", detailMarkdown: "Simple single-host operation." },
        ],
      },
    },
    {
      key: "release",
      session: "planning",
      name: "Fixture release choices",
      summary: "Settle the independent release defaults in one pass",
      markdownPath: "docs/fixture-release.md",
      markdown:
        "# Fixture release choices\n\n"
        + "Independent defaults that can be answered together.\n",
      batch: {
        batchKey: "release-defaults",
        decisions: [
          {
            key: "region",
            title: "Choose a region",
            promptMarkdown: "Where the fixture deployment lives.",
            mode: "single",
            options: [
              { key: "east", label: "East" },
              { key: "west", label: "West" },
            ],
          },
          {
            key: "notifications",
            title: "Choose notifications",
            promptMarkdown: "How the fixture deployment reports.",
            mode: "multiple",
            options: [
              { key: "email", label: "Email" },
              { key: "push", label: "Push" },
            ],
          },
        ],
      },
    },
  ],
};

function fail(message) {
  throw new Error(message);
}

/// A condition that will never become true however long the poll runs,
/// such as the daemon having exited. `waitForCondition` rethrows it at
/// once instead of spending the whole deadline on a dead process.
export class UnrecoverableError extends Error {}

function abort(message) {
  throw new UnrecoverableError(message);
}

function requireText(value, what) {
  if (typeof value !== "string" || value.trim() === "") fail(`${what} must be a non-empty string`);
  return value;
}

function requirePositiveInteger(value, what) {
  if (!Number.isInteger(value) || value <= 0) fail(`${what} must be a positive integer`);
  return value;
}

function requireRelativePath(value, what) {
  requireText(value, what);
  if (isAbsolute(value)) fail(`${what} must be relative to the fixture root, got ${value}`);
  if (value.split("/").includes("..")) fail(`${what} must not escape the fixture root, got ${value}`);
  return value;
}

function requireUniqueKeys(entries, what) {
  const seen = new Set();
  for (const entry of entries) {
    const key = requireText(entry?.key, `${what} key`);
    if (seen.has(key)) fail(`${what} keys must be unique, ${key} appears twice`);
    seen.add(key);
  }
}

function normalizeOptions(options, what) {
  if (!Array.isArray(options) || options.length === 0) fail(`${what} needs at least one option`);
  requireUniqueKeys(options, `${what} option`);
  return options.map((option) => ({
    key: option.key,
    label: requireText(option.label, `${what} option label`),
    detailMarkdown: option.detailMarkdown ?? "",
  }));
}

function normalizeDecision(decision, what, modes) {
  requireText(decision?.key, `${what} key`);
  requireText(decision.title, `${what} title`);
  if (!modes.has(decision.mode)) {
    fail(`${what} mode must be one of ${[...modes].join(", ")}, got ${decision.mode}`);
  }
  const normalized = {
    key: decision.key,
    title: decision.title,
    promptMarkdown: decision.promptMarkdown ?? "",
    detailMarkdown: decision.detailMarkdown ?? "",
    mode: decision.mode,
    allowCustom: decision.allowCustom === true,
  };
  if (decision.mode !== "dialogue") {
    normalized.options = normalizeOptions(decision.options, what);
  } else if (decision.options !== undefined) {
    normalized.options = normalizeOptions(decision.options, what);
  }
  return normalized;
}

/// Merges caller configuration over the defaults and rejects a shape the
/// daemon would only refuse later, or that would make the seeded fixture
/// ambiguous to a consumer matching on keys.
export function normalizeSpec(overrides = {}) {
  if (overrides === null || typeof overrides !== "object" || Array.isArray(overrides)) {
    fail("fixture configuration must be a JSON object");
  }
  const spec = {
    bucket: overrides.bucket ?? DEFAULT_SPEC.bucket,
    project: { ...DEFAULT_SPEC.project, ...(overrides.project ?? {}) },
    agent: overrides.agent ?? DEFAULT_SPEC.agent,
    mobile: { ...DEFAULT_SPEC.mobile, ...(overrides.mobile ?? {}) },
    sessions: overrides.sessions ?? DEFAULT_SPEC.sessions,
    items: overrides.items ?? DEFAULT_SPEC.items,
    plans: overrides.plans ?? DEFAULT_SPEC.plans,
  };

  requireText(spec.bucket, "bucket name");
  requireText(spec.project.name, "project name");
  requireRelativePath(spec.project.directory, "project directory");
  requireText(spec.agent, "agent");
  requirePositiveInteger(spec.mobile.accessTtlMinutes, "mobile.accessTtlMinutes");
  requirePositiveInteger(spec.mobile.enrollTtlMinutes, "mobile.enrollTtlMinutes");

  if (!Array.isArray(spec.sessions) || spec.sessions.length === 0) {
    fail("the fixture needs at least one session");
  }
  requireUniqueKeys(spec.sessions, "session");
  const titles = new Set();
  spec.sessions = spec.sessions.map((session) => {
    const title = requireText(session.title, "session title");
    if (titles.has(title)) fail(`session titles must be unique, ${title} appears twice`);
    titles.add(title);
    return { key: session.key, title, prompt: session.prompt ?? title };
  });

  if (!Array.isArray(spec.items)) fail("items must be an array");
  requireUniqueKeys(spec.items, "item");
  spec.items = spec.items.map((item) => {
    if (!ITEM_STATUSES.has(item.status)) fail(`item status ${item.status} is not a board status`);
    if (!ITEM_PRIORITIES.has(item.priority)) fail(`item priority ${item.priority} is unknown`);
    return {
      key: item.key,
      title: requireText(item.title, "item title"),
      status: item.status,
      priority: item.priority,
      body: item.body ?? "",
    };
  });

  if (!Array.isArray(spec.plans)) fail("plans must be an array");
  requireUniqueKeys(spec.plans, "plan");
  const sessionKeys = new Set(spec.sessions.map((session) => session.key));
  const planNames = new Set();
  spec.plans = spec.plans.map((plan) => {
    const name = requireText(plan.name, "plan name");
    if (planNames.has(name)) fail(`plan names must be unique, ${name} appears twice`);
    planNames.add(name);
    if (!sessionKeys.has(plan.session)) {
      fail(`plan ${plan.key} names session ${plan.session}, which the fixture does not seed`);
    }
    if (plan.decision && plan.batch) {
      fail(`plan ${plan.key} carries both a decision and a batch; a plan has one active decision`);
    }
    if (!plan.decision && !plan.batch) {
      fail(`plan ${plan.key} carries no decision; seed one so a consumer has an open decision`);
    }
    const normalized = {
      key: plan.key,
      session: plan.session,
      name,
      summary: plan.summary ?? "",
      markdownPath: requireRelativePath(plan.markdownPath, "plan markdownPath"),
      markdown: plan.markdown ?? `# ${name}\n`,
    };
    if (plan.decision) {
      normalized.decision = normalizeDecision(plan.decision, `plan ${plan.key} decision`, DECISION_MODES);
    } else {
      const batch = plan.batch;
      requireText(batch.batchKey, `plan ${plan.key} batchKey`);
      if (!Array.isArray(batch.decisions) || batch.decisions.length < 2 || batch.decisions.length > 8) {
        fail(`plan ${plan.key} batch needs between 2 and 8 decisions`);
      }
      requireUniqueKeys(batch.decisions, `plan ${plan.key} batch decision`);
      normalized.batch = {
        batchKey: batch.batchKey,
        decisions: batch.decisions.map((decision) =>
          normalizeDecision(decision, `plan ${plan.key} batch decision`, BATCH_DECISION_MODES)),
      };
    }
    return normalized;
  });

  return spec;
}

/// Every path the fixture owns, derived from one root so isolation is a
/// property of the root rather than of each caller remembering a flag.
export function fixtureLayout(rootDirectory, spec = DEFAULT_SPEC) {
  const base = resolve(rootDirectory);
  return {
    root: base,
    socket: join(base, "pm.sock"),
    db: join(base, "pm.db"),
    scrollbackDir: join(base, "scrollback"),
    logFile: join(base, "daemon.log"),
    failureLog: join(base, FAILURE_LOG_FILE),
    statePath: join(base, STATE_FILE),
    binDir: join(base, "bin"),
    agentHome: join(base, "agent-home"),
    projectDir: join(base, spec.project.directory),
  };
}

function isInside(parent, child) {
  const rel = relative(parent, child);
  return rel !== "" && !rel.startsWith("..") && !isAbsolute(rel);
}

/// The paths a default `pm daemon` and a default `pm` client would use.
/// The fixture refuses to name any of them.
export function reservedPaths(env = process.env, home = homedir()) {
  const dataDirs = [
    join(env.XDG_DATA_HOME ?? join(home, ".local", "share"), "puppet-master"),
    join(home, "Library", "Application Support", "puppet-master"),
  ];
  const configDirs = [
    join(env.XDG_CONFIG_HOME ?? join(home, ".config"), "puppet-master"),
    join(home, "Library", "Application Support", "puppet-master"),
  ];
  const runtimeDirs = env.XDG_RUNTIME_DIR ? [join(env.XDG_RUNTIME_DIR, "puppet-master")] : [];
  const directories = [...new Set([...dataDirs, ...configDirs, ...runtimeDirs].map((path) => resolve(path)))];
  const sockets = [
    ...(env.XDG_RUNTIME_DIR ? [join(env.XDG_RUNTIME_DIR, "puppet-master", "pm.sock")] : []),
    ...dataDirs.map((dir) => join(dir, "pm.sock")),
  ].map((path) => resolve(path));
  const databases = dataDirs.map((dir) => resolve(join(dir, "pm.db")));
  const scrollbacks = dataDirs.map((dir) => resolve(join(dir, "scrollback")));
  return { directories, sockets, databases, scrollbacks };
}

/// Refuses a root that is a shared or user-owned directory rather than a
/// throwaway of the fixture's own, and refuses any managed path that
/// escapes it or collides with what a default daemon uses.
export function assertIsolatedLayout(layout, options = {}) {
  const env = options.env ?? process.env;
  const home = resolve(options.home ?? homedir());
  const reserved = reservedPaths(env, home);
  const base = layout.root;

  if (!isAbsolute(base)) fail(`fixture root must be an absolute path, got ${base}`);
  if (base === resolve("/")) fail("fixture root must not be the filesystem root");
  if (base === home) fail("fixture root must not be the home directory");
  if (!isInside(home, base) && !isInside(resolve(tmpdir()), base) && !options.allowAnyRoot) {
    // A root outside both the home tree and the temp tree is almost always
    // a typo pointed at a system directory, and the fixture deletes it.
    fail(`fixture root ${base} is outside ${resolve(tmpdir())} and ${home}; pass an explicit --root inside one`);
  }
  for (const directory of reserved.directories) {
    if (base === directory || isInside(base, directory) || isInside(directory, base)) {
      fail(`fixture root ${base} overlaps the puppet-master directory ${directory}`);
    }
  }

  if (reserved.sockets.includes(layout.socket)) fail(`fixture socket ${layout.socket} is the default socket`);
  if (reserved.databases.includes(layout.db)) fail(`fixture database ${layout.db} is the default database`);
  if (reserved.scrollbacks.includes(layout.scrollbackDir)) {
    fail(`fixture scrollback directory ${layout.scrollbackDir} is the default transcript directory`);
  }

  const managed = {
    socket: layout.socket,
    db: layout.db,
    "scrollback directory": layout.scrollbackDir,
    "log file": layout.logFile,
    "state file": layout.statePath,
    "project directory": layout.projectDir,
  };
  for (const [what, path] of Object.entries(managed)) {
    if (!isInside(base, path)) fail(`fixture ${what} ${path} is not inside the fixture root ${base}`);
  }

  const socketBytes = Buffer.byteLength(layout.socket);
  if (socketBytes > MAX_SOCKET_PATH_BYTES) {
    fail(
      `fixture socket path is ${socketBytes} bytes, over the ${MAX_SOCKET_PATH_BYTES}-byte Unix socket limit: `
      + `${layout.socket}. Set TMPDIR to a shorter path or pass --root.`,
    );
  }
  return layout;
}

/// The daemon accepts any bind address; the fixture only ever wants one
/// reachable from this machine, so anything routable is a refusal.
export function assertLoopbackHost(host) {
  const text = requireText(host, "http host");
  if (text === "::1" || text === "[::1]") return text;
  const octets = text.split(".");
  const loopback = octets.length === 4
    && octets.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255)
    && Number(octets[0]) === 127;
  if (!loopback) fail(`fixture http host must be loopback, got ${text}`);
  return text;
}

/// Polls a condition the daemon actually publishes. Nothing in the
/// fixture waits a fixed interval and hopes; a probe that never becomes
/// true fails with the last error it saw.
export async function waitForCondition(description, probe, options = {}) {
  const timeoutMs = options.timeoutMs ?? DEFAULT_READY_TIMEOUT_MS;
  const intervalMs = options.intervalMs ?? READY_POLL_MS;
  const now = options.now ?? (() => Date.now());
  const sleep = options.sleep ?? ((ms) => new Promise((done) => setTimeout(done, ms)));
  const deadline = now() + timeoutMs;
  let attempts = 0;
  let lastError = null;
  for (;;) {
    attempts += 1;
    try {
      const value = await probe();
      if (value !== undefined && value !== null && value !== false) return value;
      lastError = null;
    } catch (error) {
      if (error instanceof UnrecoverableError) throw error;
      lastError = error;
    }
    if (now() >= deadline) {
      const detail = lastError ? `: ${lastError.message}` : "";
      fail(`timed out after ${timeoutMs}ms and ${attempts} probes waiting for ${description}${detail}`);
    }
    await sleep(intervalMs);
  }
}

export function tailBytes(text, limit = LOG_TAIL_BYTES) {
  const buffer = Buffer.from(text ?? "", "utf8");
  if (buffer.byteLength <= limit) return buffer.toString("utf8");
  return `… ${buffer.byteLength - limit} earlier bytes omitted …\n${buffer.subarray(buffer.byteLength - limit).toString("utf8")}`;
}

/// The original invocation pointed at the root that was kept, so the
/// command reproduces the failure against the preserved evidence rather
/// than against a fresh directory.
export function reproCommand(argv, rootDirectory) {
  const args = [];
  const source = argv.length > 0 ? argv : ["start"];
  for (let index = 0; index < source.length; index += 1) {
    if (source[index] === "--root") {
      index += 1;
      continue;
    }
    args.push(source[index]);
  }
  return `./scripts/controller-fixture.mjs ${args.join(" ")} --root ${rootDirectory}`;
}

/// What a consumer needs to see when the bootstrap failed: which step
/// broke, where the evidence was kept, and the one command that puts them
/// back in front of it.
export function failureReport({ stage, error, root: rootDirectory, repro, logTail }) {
  const lines = [
    `controller fixture failed during ${stage}: ${error}`,
    `preserved fixture root: ${rootDirectory}`,
    `reproduce with: ${repro}`,
  ];
  if (logTail && logTail.trim() !== "") {
    lines.push("--- daemon log tail ---", logTail.trimEnd(), "--- end daemon log tail ---");
  } else {
    lines.push("the daemon produced no output");
  }
  return `${lines.join("\n")}\n`;
}

/// The launch environment an XCUITest wrapper hands the app under test.
export function fixtureEnv(state) {
  const pairs = (entries, value) => entries.map((entry) => `${entry.key}=${value(entry)}`).join(",");
  return {
    PM_FIXTURE_ROOT: state.root,
    PM_FIXTURE_BASE_URL: state.baseUrl,
    PM_FIXTURE_SOCKET: state.socket,
    PM_FIXTURE_INSTALLATION_ID: state.auth.installationId,
    PM_FIXTURE_USERNAME: state.auth.username,
    PM_FIXTURE_PASSWORD: state.auth.password,
    PM_FIXTURE_SESSION_COOKIE: state.auth.sessionCookie,
    PM_FIXTURE_MOBILE_ENROLL_TOKEN: state.auth.mobileEnrollToken,
    PM_FIXTURE_MOBILE_ENROLL_TOKEN_EXPIRES_AT: String(state.auth.mobileEnrollTokenExpiresAtUnixMs),
    PM_FIXTURE_MOBILE_ACCESS_TOKEN: state.auth.mobileAccessToken,
    PM_FIXTURE_MOBILE_REFRESH_TOKEN: state.auth.mobileRefreshToken,
    PM_FIXTURE_MOBILE_DEVICE_ID: state.auth.mobileDeviceId,
    PM_FIXTURE_BUCKET_ID: String(state.fixtures.bucketId),
    PM_FIXTURE_PROJECT_ID: String(state.fixtures.projectId),
    PM_FIXTURE_PROJECT_DIR: state.fixtures.projectDir,
    PM_FIXTURE_SESSION_IDS: pairs(state.fixtures.sessions, (session) => session.id),
    PM_FIXTURE_SESSION_TITLES: pairs(state.fixtures.sessions, (session) => session.title),
    PM_FIXTURE_ITEM_IDS: pairs(state.fixtures.items, (item) => item.id),
    PM_FIXTURE_PLAN_IDS: pairs(state.fixtures.plans, (plan) => plan.id),
    PM_FIXTURE_DECISION_KEYS: pairs(state.fixtures.plans, (plan) => plan.decisionKeys.join("|")),
  };
}

/// Node's own warning handler honours --redirect-warnings, which can be
/// pointed at stdout and corrupt the document a consumer parses. Warnings
/// are diagnostics, so this routes them to stderr and nowhere else.
export function routeWarningsToStderr(emitter = process, stderr = process.stderr) {
  emitter.removeAllListeners("warning");
  emitter.on("warning", (warning) => {
    stderr.write(`${warning.name}: ${warning.message}\n`);
  });
}

/// stdout carries exactly one JSON document and nothing else. Every other
/// write to it — a stray log, a dependency's own output, anything added
/// later — is moved to stderr, and only the returned writer reaches
/// stdout.
export function guardStdout(stdout = process.stdout, stderr = process.stderr) {
  const write = stdout.write.bind(stdout);
  stdout.write = (chunk, encoding, callback) => stderr.write(chunk, encoding, callback);
  return (text) => write(text);
}

export function renderEnv(env) {
  return Object.entries(env)
    .map(([key, value]) => `${key}=${JSON.stringify(String(value))}`)
    .join("\n");
}

export function parseArgs(argv) {
  const [command, ...rest] = argv;
  if (command === undefined || command === "--help" || command === "-h") {
    fail(`usage: controller-fixture.mjs <${COMMANDS.join("|")}> [options]`);
  }
  if (!COMMANDS.includes(command)) {
    fail(`unknown command ${command}; expected one of ${COMMANDS.join(", ")}`);
  }
  const options = {
    root: null,
    pmBin: null,
    testAgentBin: null,
    config: null,
    httpHost: "127.0.0.1",
    timeoutMs: DEFAULT_READY_TIMEOUT_MS,
    format: "json",
    keep: false,
  };
  for (let index = 0; index < rest.length; index += 1) {
    const flag = rest[index];
    const value = () => {
      const next = rest[index + 1];
      if (next === undefined) fail(`${flag} needs a value`);
      index += 1;
      return next;
    };
    switch (flag) {
      case "--root": options.root = value(); break;
      case "--pm-bin": options.pmBin = value(); break;
      case "--testagent-bin": options.testAgentBin = value(); break;
      case "--config": options.config = value(); break;
      case "--http-host": options.httpHost = value(); break;
      case "--timeout-ms": options.timeoutMs = Number(value()); break;
      case "--format": options.format = value(); break;
      case "--keep": options.keep = true; break;
      default: fail(`unknown option ${flag}`);
    }
  }
  if (!["json", "env"].includes(options.format)) {
    fail(`--format must be json or env, got ${options.format}`);
  }
  if (!Number.isFinite(options.timeoutMs) || options.timeoutMs <= 0) {
    fail("--timeout-ms must be a positive number of milliseconds");
  }
  assertLoopbackHost(options.httpHost);
  if (["seed", "status", "stop", "enroll-token"].includes(command) && options.root === null) {
    fail(`${command} needs --root pointing at a fixture started earlier`);
  }
  return { command, options };
}

/// Where a built `pm` and `pm-testagent` are, in the order a developer
/// most likely has them. Absence is a named failure with the exact build
/// command, never a silent skip.
export function resolveBinaries(options = {}, env = process.env, root = repositoryRoot) {
  const explicit = {
    pm: options.pmBin ?? env.PM_FIXTURE_PM_BIN ?? null,
    testAgent: options.testAgentBin ?? env.PM_FIXTURE_TESTAGENT_BIN ?? null,
  };
  const targetDirectory = resolve(root, env.CARGO_TARGET_DIR ?? join(root, "target"));
  const searched = [];
  const find = (name, given) => {
    if (given) {
      const path = resolve(given);
      if (!existsSync(path)) fail(`${name} binary ${path} does not exist`);
      return path;
    }
    for (const profile of BUILD_PROFILES) {
      const candidate = join(targetDirectory, profile, name);
      searched.push(candidate);
      if (existsSync(candidate)) return candidate;
    }
    fail(
      `no ${name} binary found. Looked in:\n  ${searched.join("\n  ")}\n`
      + "Build one with:\n"
      + "  cargo build --profile e2e --features pm-daemon/testagent --bin pm --bin pm-testagent",
    );
  };
  return { pm: find("pm", explicit.pm), testAgent: find("pm-testagent", explicit.testAgent) };
}

/// The at-rest form of a token, matching how the daemon stores one.
export function enrollTokenHash(token) {
  return createHash("sha256").update(token).digest("hex");
}

/// Reads back what the daemon recorded for a token it just minted.
export function enrollTokenRecord(rows, token, now) {
  const hash = enrollTokenHash(token);
  const row = rows.find((candidate) => candidate.token_hash === hash);
  if (!row) return { known: false, used: false, remainingMs: 0 };
  return {
    known: true,
    used: row.used_at_unix_ms !== null,
    remainingMs: row.expires_at_unix_ms - now,
  };
}

function randomCredential() {
  return Buffer.from(crypto.getRandomValues(new Uint8Array(CREDENTIAL_BYTES))).toString("hex");
}

function freePort(host) {
  return new Promise((done, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, host, () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("failed to allocate a loopback port"));
        return;
      }
      server.close((error) => (error ? reject(error) : done(address.port)));
    });
  });
}

function readLogTail(logFile) {
  try {
    return tailBytes(readFileSync(logFile, "utf8"));
  } catch {
    return "";
  }
}

// node:sqlite announces itself as experimental the first time it loads.
// Loading it on demand keeps that notice behind `routeWarningsToStderr`,
// which a top-level import would fire before.
const load = createRequire(import.meta.url);
let sqlite = null;

function readDatabase(dbPath, read) {
  sqlite ??= load("node:sqlite");
  const database = new sqlite.DatabaseSync(dbPath, { readOnly: true });
  try {
    return read(database);
  } finally {
    database.close();
  }
}

class Controller {
  constructor({ layout, spec, binaries, httpHost, timeoutMs }) {
    this.layout = layout;
    this.spec = spec;
    this.binaries = binaries;
    this.httpHost = httpHost;
    this.timeoutMs = timeoutMs;
    this.child = null;
    this.baseUrl = null;
    this.pid = null;
  }

  daemonEnv() {
    return {
      ...process.env,
      PATH: `${this.layout.binDir}:${process.env.PATH ?? ""}`,
      SHELL: "/bin/sh",
      TERM: "xterm-256color",
      // Agent CLIs record trusted directories under their own home. The
      // fixture answers into a throwaway one so a run never edits the
      // developer's.
      CODEX_HOME: join(this.layout.agentHome, "codex"),
      PM_SOCKET: this.layout.socket,
    };
  }

  daemonCommand(httpPort, workerPort) {
    return [
      "daemon",
      "--db", this.layout.db,
      "--scrollback-dir", this.layout.scrollbackDir,
      "--socket", this.layout.socket,
      "--http", `${this.httpHost}:${httpPort}`,
      // The host plane's default port is fixed, so a fixture left on it
      // would fight the developer's own daemon and the E2E lanes.
      "--worker-listen", `${this.httpHost}:${workerPort}`,
    ];
  }

  cli(args) {
    return execFileSync(this.binaries.pm, args, {
      env: { ...this.daemonEnv(), PM_SOCKET: this.layout.socket },
      encoding: "utf8",
    });
  }

  async request(path, init = {}) {
    const response = await fetch(`${this.baseUrl}${path}`, init);
    if (!response.ok) {
      const body = await response.text().catch(() => "");
      fail(`${init.method ?? "GET"} ${path} returned ${response.status}: ${body.slice(0, 500)}`);
    }
    return response;
  }

  async agentTool(sessionId, name, args) {
    const token = readDatabase(this.layout.db, (database) =>
      database.prepare("SELECT session_token FROM sessions WHERE id = ?").get(sessionId)?.session_token);
    if (!token) fail(`session ${sessionId} has no agent token`);
    const response = await fetch(`${this.baseUrl}/mcp`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }),
    });
    const answer = await response.json();
    if (answer.error || answer.result?.isError) {
      fail(`agent tool ${name} failed: ${JSON.stringify(answer).slice(0, 500)}`);
    }
    return JSON.parse(answer.result?.content?.[0]?.text ?? "{}");
  }

  async start({ foreground }) {
    mkdirSync(this.layout.binDir, { recursive: true });
    mkdirSync(this.layout.agentHome, { recursive: true });
    mkdirSync(this.layout.projectDir, { recursive: true });
    const shim = join(this.layout.binDir, AGENT_SHIM);
    rmSync(shim, { force: true });
    symlinkSync(this.binaries.testAgent, shim);

    const httpPort = await freePort(this.httpHost);
    const workerPort = await freePort(this.httpHost);
    this.baseUrl = `http://${this.httpHost}:${httpPort}`;
    this.workerUrl = `wss://${this.httpHost}:${workerPort}`;

    const log = openSync(this.layout.logFile, "a");
    try {
      this.child = spawn(this.binaries.pm, this.daemonCommand(httpPort, workerPort), {
        env: this.daemonEnv(),
        stdio: ["ignore", log, log],
        detached: !foreground,
      });
    } finally {
      closeFileDescriptor(log);
    }
    if (!foreground) this.child.unref();
    this.pid = this.child.pid;

    const version = await waitForCondition("the controller's HTTP surface", async () => {
      if (this.child.exitCode !== null || this.child.signalCode !== null) {
        abort(`the controller exited during startup with code ${this.child.exitCode ?? this.child.signalCode}`);
      }
      const response = await fetch(`${this.baseUrl}/api/version`);
      return response.ok ? response.json() : false;
    }, { timeoutMs: this.timeoutMs });
    this.installationId = version.installationId;

    await waitForCondition("the controller's unix socket", async () => {
      this.cli(["bucket", "ls"]);
      return true;
    }, { timeoutMs: this.timeoutMs });
  }

  async authenticate() {
    const username = `fixture-${randomCredential().slice(0, 8)}`;
    const password = randomCredential();
    const setup = await fetch(`${this.baseUrl}/api/setup`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ username, password }),
    });
    if (!setup.ok) {
      fail(`the controller refused the fixture user: ${setup.status} ${await setup.text().catch(() => "")}`);
    }
    const cookie = (setup.headers.get("set-cookie") ?? "").match(/pm_session=([^;]+)/)?.[1];
    if (!cookie) fail("the controller returned no session cookie for the fixture user");
    return { username, password, sessionCookie: cookie };
  }

  async mintEnrollToken(cookie) {
    // The cookie only mints a dashboard bearer, and only with the dashboard's origin.
    const headers = { cookie: `pm_session=${cookie}`, origin: this.baseUrl };
    const web = await this.request("/api/web/token", { method: "POST", headers });
    const { accessToken } = await web.json();
    const response = await this.request("/api/mobile/devices/enroll-token", {
      method: "POST",
      headers: { ...headers, authorization: `Bearer ${accessToken}` },
    });
    const minted = await response.json();
    return { token: minted.token, expiresAtUnixMs: minted.expiresAtUnixMs };
  }

  /// The daemon holds tokens only as hashes, so an advertised token is
  /// checked against its own store rather than assumed good.
  assertAdvertisable(token) {
    const rows = readDatabase(this.layout.db, (database) =>
      database.prepare(
        "SELECT token_hash, expires_at_unix_ms, used_at_unix_ms FROM mobile_enrollment_tokens",
      ).all());
    const record = enrollTokenRecord(rows, token, Date.now());
    if (!record.known) fail("the enrollment token the fixture minted is not in the controller's store");
    if (record.used) fail("the enrollment token the fixture is about to advertise has already been used");
    if (record.remainingMs < ENROLL_TOKEN_MIN_REMAINING_MS) {
      fail(
        `the enrollment token expires in ${Math.round(record.remainingMs / 1000)}s, `
        + "too soon to hand to a client. Raise mobile.enrollTtlMinutes in the fixture configuration.",
      );
    }
    return record;
  }

  async enrollDevice(cookie) {
    // Seeding spends a token minted for itself alone, and the token a
    // client is given is minted afterwards and never touched again, so
    // no ordering change can make the fixture consume what it advertises.
    const internal = await this.mintEnrollToken(cookie);
    const response = await this.request("/api/mobile/devices/enroll", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        deviceId: DEVICE_APP_INSTALLATION_ID,
        name: "controller fixture",
        platform: "fixture",
        enrollToken: internal.token,
      }),
    });
    const enrollment = await response.json();
    const advertised = await this.mintEnrollToken(cookie);
    this.assertAdvertisable(advertised.token);
    return {
      mobileDeviceId: enrollment.device.id,
      mobileAccessToken: enrollment.tokens.accessToken,
      mobileRefreshToken: enrollment.tokens.refreshToken,
      installationId: enrollment.installationId,
      mobileEnrollToken: advertised.token,
      mobileEnrollTokenExpiresAtUnixMs: advertised.expiresAtUnixMs,
    };
  }

  bucketId() {
    const rows = readDatabase(this.layout.db, (database) =>
      database.prepare("SELECT id, name FROM buckets ORDER BY id").all());
    const existing = rows.find((row) => row.name === this.spec.bucket);
    if (existing) return existing.id;
    const projects = readDatabase(this.layout.db, (database) =>
      database.prepare("SELECT COUNT(*) AS count FROM projects").get().count);
    // A daemon's first run seeds one empty "Default" bucket. Replacing it
    // keeps the fixture's own bucket at id 1, which consumers deep-link to.
    if (rows.length === 1 && rows[0].name === "Default" && projects === 0) {
      this.cli(["bucket", "rm", String(rows[0].id)]);
    }
    this.cli(["bucket", "add", this.spec.bucket]);
    const created = readDatabase(this.layout.db, (database) =>
      database.prepare("SELECT id FROM buckets WHERE name = ?").get(this.spec.bucket));
    if (!created) fail(`bucket ${this.spec.bucket} was not created`);
    return created.id;
  }

  projectId(bucketId) {
    const find = () => readDatabase(this.layout.db, (database) =>
      database.prepare("SELECT id FROM projects WHERE bucket_id = ? AND name = ?").get(bucketId, this.spec.project.name));
    const existing = find();
    if (existing) return existing.id;
    this.cli(["project", "add", "--bucket", String(bucketId), this.spec.project.name, this.layout.projectDir]);
    const created = find();
    if (!created) fail(`project ${this.spec.project.name} was not created`);
    return created.id;
  }

  liveSessions(projectId) {
    return readDatabase(this.layout.db, (database) =>
      database.prepare("SELECT id, task_title, state FROM sessions WHERE project_id = ? ORDER BY id").all(projectId));
  }

  async seedSessions(projectId) {
    for (const session of this.spec.sessions) {
      const live = this.liveSessions(projectId)
        .find((row) => row.task_title === session.title && LIVE_SESSION_STATES.has(row.state));
      if (live) continue;
      this.cli([
        "spawn", "--project", String(projectId), "--agent", this.spec.agent,
        "--title", session.title, session.prompt,
      ]);
    }
    const seeded = [];
    for (const session of this.spec.sessions) {
      const row = await waitForCondition(`session ${session.title} to reach a live state`, () =>
        this.liveSessions(projectId)
          .filter((candidate) => candidate.task_title === session.title && LIVE_SESSION_STATES.has(candidate.state))
          .pop() ?? false, { timeoutMs: this.timeoutMs });
      const terminal = readDatabase(this.layout.db, (database) =>
        database.prepare("SELECT id FROM terminals WHERE session_id = ? AND kind = 'agent'").get(row.id));
      // Live state is deliberately absent: it changes as the daemon runs
      // (an open decision moves a session to needs-input), so a consumer
      // reads it from the API rather than from a snapshot taken at seed.
      seeded.push({ key: session.key, title: session.title, id: row.id, terminalId: terminal?.id ?? null });
    }
    return seeded;
  }

  seedItems(bucketId, projectId) {
    const find = (title) => readDatabase(this.layout.db, (database) =>
      database.prepare("SELECT item_number FROM items WHERE bucket_id = ? AND title = ? ORDER BY item_number").get(bucketId, title));
    return this.spec.items.map((item) => {
      if (!find(item.title)) {
        this.cli([
          "items", "add", "--bucket", String(bucketId), "--project", String(projectId),
          "--status", item.status, "--priority", item.priority, "--body", item.body, item.title,
        ]);
      }
      const row = find(item.title);
      if (!row) fail(`item ${item.title} was not created`);
      return { key: item.key, title: item.title, id: row.item_number, ref: `pm:item/${bucketId}/${row.item_number}` };
    });
  }

  async seedPlans(sessions) {
    const seeded = [];
    for (const plan of this.spec.plans) {
      const owner = sessions.find((session) => session.key === plan.session);
      const markdownFile = join(this.layout.projectDir, plan.markdownPath);
      mkdirSync(dirname(markdownFile), { recursive: true });
      writeFileSync(markdownFile, plan.markdown);

      const listed = await this.agentTool(owner.id, "list_plans", { include_archived: true });
      const existing = (listed.plans ?? []).find((candidate) => candidate.name === plan.name);
      const record = await this.agentTool(owner.id, "upsert_plan", {
        ...(existing ? { plan: existing.id } : {}),
        name: plan.name,
        summary: plan.summary,
        markdown_path: plan.markdownPath,
        state: "active",
      });
      await this.agentTool(owner.id, "sync_plan", { plan: record.id });

      let decisionKeys;
      if (plan.decision) {
        await this.agentTool(owner.id, "present_plan_decision", {
          plan: record.id,
          key: plan.decision.key,
          title: plan.decision.title,
          prompt_markdown: plan.decision.promptMarkdown,
          detail_markdown: plan.decision.detailMarkdown,
          mode: plan.decision.mode,
          allow_custom: plan.decision.allowCustom,
          ...(plan.decision.options ? { options: toolOptions(plan.decision.options) } : {}),
        });
        decisionKeys = [plan.decision.key];
      } else {
        await this.agentTool(owner.id, "present_plan_decision_batch", {
          plan: record.id,
          batch_key: plan.batch.batchKey,
          decisions: plan.batch.decisions.map((decision) => ({
            key: decision.key,
            title: decision.title,
            prompt_markdown: decision.promptMarkdown,
            detail_markdown: decision.detailMarkdown,
            mode: decision.mode,
            allow_custom: decision.allowCustom,
            options: toolOptions(decision.options),
          })),
        });
        decisionKeys = plan.batch.decisions.map((decision) => decision.key);
      }
      const stored = await this.agentTool(owner.id, "get_plan", { plan: record.id });
      if (!stored.plan?.activeDecisionId) fail(`plan ${plan.name} has no active decision after seeding`);
      seeded.push({
        key: plan.key,
        name: plan.name,
        id: record.id,
        sessionId: owner.id,
        markdownPath: plan.markdownPath,
        batchKey: plan.batch?.batchKey ?? null,
        decisionKeys,
        activeDecisionId: stored.plan.activeDecisionId,
      });
    }
    return seeded;
  }

  /// Token lifetimes are settings the daemon reads when it mints, so they
  /// are in force before the first token exists rather than after.
  configure() {
    this.cli(["config", "set", "mobile.access_token_ttl_minutes", String(this.spec.mobile.accessTtlMinutes)]);
    this.cli(["config", "set", "mobile.enrollment_token_ttl_minutes", String(this.spec.mobile.enrollTtlMinutes)]);
  }

  async seed(auth) {
    const bucketId = this.bucketId();
    const projectId = this.projectId(bucketId);
    const sessions = await this.seedSessions(projectId);
    const items = this.seedItems(bucketId, projectId);
    const plans = await this.seedPlans(sessions);
    return {
      version: STATE_VERSION,
      root: this.layout.root,
      pid: this.pid,
      baseUrl: this.baseUrl,
      workerUrl: this.workerUrl,
      socket: this.layout.socket,
      db: this.layout.db,
      scrollbackDir: this.layout.scrollbackDir,
      logFile: this.layout.logFile,
      pmBin: this.binaries.pm,
      testAgentBin: this.binaries.testAgent,
      auth: { ...auth, installationId: this.installationId },
      fixtures: { bucketId, projectId, projectDir: this.layout.projectDir, sessions, items, plans },
    };
  }
}

function toolOptions(options) {
  return (options ?? []).map((option) => ({
    key: option.key,
    label: option.label,
    detail_markdown: option.detailMarkdown,
  }));
}

function closeFileDescriptor(fd) {
  try {
    // The child holds its own duplicate once spawned.
    closeSync(fd);
  } catch {}
}

function loadState(rootDirectory) {
  const statePath = join(resolve(rootDirectory), STATE_FILE);
  if (!existsSync(statePath)) fail(`no fixture state at ${statePath}; start one with controller-fixture.mjs start`);
  const state = JSON.parse(readFileSync(statePath, "utf8"));
  if (state.version !== STATE_VERSION) {
    fail(`fixture state at ${statePath} is version ${state.version}, this script speaks ${STATE_VERSION}`);
  }
  return state;
}

function processAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

/// An explicit root is refused before anything is created, so a typo
/// pointed at a system directory never gets a mkdir or a chmod.
function prepareRoot(options, spec) {
  if (options.root !== null) {
    const layout = assertIsolatedLayout(fixtureLayout(options.root, spec));
    mkdirSync(layout.root, { recursive: true, mode: 0o700 });
    chmodSync(layout.root, 0o700);
    return layout;
  }
  const base = mkdtempSync(join(resolve(tmpdir()), "pm-fixture-"));
  chmodSync(base, 0o700);
  return assertIsolatedLayout(fixtureLayout(base, spec));
}

/// Refuses to signal a process that is not the daemon this fixture
/// started: a recorded pid can be reused once the daemon is gone.
async function stopDaemon(state, timeoutMs) {
  if (!state.pid || !processAlive(state.pid)) return "already stopped";
  let ours = existsSync(state.socket);
  try {
    const response = await fetch(`${state.baseUrl}/api/version`);
    ours = response.ok && (await response.json()).installationId === state.auth.installationId;
  } catch {}
  if (!ours) return `left pid ${state.pid} alone: it does not answer as this fixture's controller`;
  process.kill(state.pid, "SIGINT");
  try {
    await waitForCondition(`controller ${state.pid} to exit`, () => !processAlive(state.pid), { timeoutMs });
  } catch {
    process.kill(state.pid, "SIGKILL");
    await waitForCondition(`controller ${state.pid} to exit`, () => !processAlive(state.pid), { timeoutMs });
  }
  return "stopped";
}

function emit(writeDocument, state, format) {
  const payload = { ...state, env: fixtureEnv(state) };
  writeDocument(format === "env" ? `${renderEnv(payload.env)}\n` : `${JSON.stringify(payload, null, 2)}\n`);
}

async function bootstrap(options, argv, foreground) {
  const spec = normalizeSpec(options.config === null ? {} : JSON.parse(readFileSync(resolve(options.config), "utf8")));
  const binaries = resolveBinaries(options);
  const layout = prepareRoot(options, spec);
  const controller = new Controller({
    layout,
    spec,
    binaries,
    httpHost: assertLoopbackHost(options.httpHost),
    timeoutMs: options.timeoutMs,
  });
  let stage = "startup";
  try {
    await controller.start({ foreground });
    controller.configure();
    stage = "authentication";
    const auth = { ...(await controller.authenticate()) };
    Object.assign(auth, await controller.enrollDevice(auth.sessionCookie));
    stage = "seeding";
    const state = await controller.seed(auth);
    writeFileSync(layout.statePath, `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
    return { controller, state, layout };
  } catch (error) {
    const report = failureReport({
      stage,
      error: error instanceof Error ? error.message : String(error),
      root: layout.root,
      repro: reproCommand(argv, layout.root),
      logTail: readLogTail(layout.logFile),
    });
    writeFileSync(layout.failureLog, report);
    if (controller.child) {
      controller.child.kill("SIGKILL");
    }
    process.stderr.write(report);
    if (error instanceof Error) error.reported = true;
    throw error;
  }
}

async function runCommand(command, options, argv, writeDocument) {
  switch (command) {
    case "start": {
      const { state } = await bootstrap(options, argv, false);
      emit(writeDocument, state, options.format);
      return 0;
    }
    case "run": {
      const { controller, state, layout } = await bootstrap(options, argv, true);
      emit(writeDocument, state, options.format);
      let shuttingDown = false;
      const shutdown = async (signal) => {
        if (shuttingDown) return;
        shuttingDown = true;
        await stopDaemon(state, DEFAULT_STOP_TIMEOUT_MS).catch(() => {});
        if (!options.keep) rmSync(layout.root, { recursive: true, force: true });
        process.exit(signal === "SIGINT" ? 130 : 0);
      };
      for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
        process.on(signal, () => { void shutdown(signal); });
      }
      await new Promise((done) => controller.child.once("exit", done));
      if (!shuttingDown && !options.keep) rmSync(layout.root, { recursive: true, force: true });
      return 0;
    }
    case "seed": {
      const state = loadState(options.root);
      const spec = normalizeSpec(options.config === null ? {} : JSON.parse(readFileSync(resolve(options.config), "utf8")));
      const layout = assertIsolatedLayout(fixtureLayout(state.root, spec));
      const controller = new Controller({
        layout,
        spec,
        binaries: { pm: state.pmBin, testAgent: state.testAgentBin },
        httpHost: assertLoopbackHost(options.httpHost),
        timeoutMs: options.timeoutMs,
      });
      controller.baseUrl = state.baseUrl;
      controller.workerUrl = state.workerUrl;
      controller.pid = state.pid;
      controller.installationId = state.auth.installationId;
      controller.configure();
      const reseeded = await controller.seed(state.auth);
      writeFileSync(layout.statePath, `${JSON.stringify(reseeded, null, 2)}\n`, { mode: 0o600 });
      emit(writeDocument, reseeded, options.format);
      return 0;
    }
    case "status": {
      const state = loadState(options.root);
      const response = await fetch(`${state.baseUrl}/api/version`).catch(() => null);
      const version = response?.ok ? await response.json() : null;
      const ready = version?.installationId === state.auth.installationId && processAlive(state.pid);
      emit(writeDocument, { ...state, ready }, options.format);
      return ready ? 0 : 1;
    }
    // A long build can outlive any token minted at start, so a client
    // that is about to launch asks for a fresh one instead.
    case "enroll-token": {
      const state = loadState(options.root);
      const spec = normalizeSpec(options.config === null ? {} : JSON.parse(readFileSync(resolve(options.config), "utf8")));
      const layout = assertIsolatedLayout(fixtureLayout(state.root, spec));
      const controller = new Controller({
        layout,
        spec,
        binaries: { pm: state.pmBin, testAgent: state.testAgentBin },
        httpHost: assertLoopbackHost(options.httpHost),
        timeoutMs: options.timeoutMs,
      });
      controller.baseUrl = state.baseUrl;
      const minted = await controller.mintEnrollToken(state.auth.sessionCookie);
      const record = controller.assertAdvertisable(minted.token);
      writeDocument(`${JSON.stringify({
        token: minted.token,
        expiresAtUnixMs: minted.expiresAtUnixMs,
        expiresInSeconds: Math.floor(record.remainingMs / 1000),
      })}\n`);
      return 0;
    }
    case "stop": {
      const state = loadState(options.root);
      const outcome = await stopDaemon(state, options.timeoutMs);
      if (!options.keep) rmSync(resolve(state.root), { recursive: true, force: true });
      writeDocument(`${JSON.stringify({ root: state.root, outcome, removed: !options.keep })}\n`);
      return 0;
    }
    default:
      return fail(`unhandled command ${command}`);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2);
  routeWarningsToStderr();
  const writeDocument = guardStdout();
  // A warning is queued on the next tick, so exiting in the same turn
  // drops the diagnostic instead of writing it.
  const exit = async (code) => {
    await new Promise((drained) => setImmediate(drained));
    process.exit(code);
  };
  try {
    const { command, options } = parseArgs(argv);
    await exit(await runCommand(command, options, argv, writeDocument));
  } catch (error) {
    if (!(error instanceof Error) || error.reported !== true) {
      process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    }
    await exit(2);
  }
}

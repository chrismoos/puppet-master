#!/usr/bin/env node
// Cuts a pm release: unlock the signing key, gates, build, sign,
// upload, tag.
//
//   ./scripts/publish.mjs [patch|minor|major] [--channel NAME] [--dry-run]
//                         [--skip-tests] [--skip-worker-images]
//   ./scripts/publish.mjs promote <pre-release>  cut the stable it leads to
//   ./scripts/publish.mjs keygen                 create the release key
//
// A stable release moves latest.json. A channel release is a pre-release,
// X.Y.Z-NAME.N, published the same way but moving channels/NAME.json
// instead, so a host that asked for the channel sees it and nobody else
// does. Promoting a pre-release rebuilds its commit as X.Y.Z on the
// release/X.Y branch, publishes that as stable, and merges the branch
// back into master.
//
// Run this on macOS. Apple Silicon builds cannot be produced anywhere
// else, and the Linux targets come from Docker, which macOS has.
//
// latest.json is written last, so a release that fails partway through
// is never the one clients resolve.

import { spawnSync } from "node:child_process";
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign as signEd25519 } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
// Where releases are published. Both name the same store: the host is
// what installed binaries fetch from, the bucket is what backs it. They
// are fixed together, so neither is a per-machine setting. The overrides
// exist to rehearse against a staging pair, not to be set routinely.
const BUCKET_URL = process.env.PM_BASE_URL ?? "https://dl.puppet-master.xyz";
const RCLONE_BUCKET = process.env.PM_R2_BUCKET ?? "puppet-master-dl";
// Which rclone remote holds the credentials, which really is per-machine:
// it is whatever the operator named it in their own rclone config.
const RCLONE_REMOTE = process.env.PM_R2_REMOTE ?? "r2";
const SIGNING_KEY =
  process.env.PM_RELEASE_KEY ?? join(process.env.HOME ?? "", ".config/puppet-master/release-key.pem");
const STAGE = join(ROOT, "dist", "release");

// Where the sandboxed worker image is published. `pm worker --sandbox`
// defaults to this repository at the launcher's own version, so the name
// is read by a test in crates/pm/src/sandbox.rs as well. PM_IMAGE_REPO
// rehearses against another registry, the way PM_BASE_URL does for the
// artifact store.
const IMAGE_REPO = process.env.PM_IMAGE_REPO ?? "ghcr.io/chrismoos/puppet-master-worker";
// Architectures the image publishes for, as one manifest list. Every one
// of them needs a release target that builds its binary.
export const IMAGE_PLATFORMS = ["linux/amd64", "linux/arm64"];
// Multi-platform output needs a builder that is not the default docker
// driver, which can neither build for a foreign architecture nor push a
// manifest list.
const IMAGE_BUILDER = "pm-release";

const PROTO_SCHEMA = "proto/pm/v1/pm.proto";

export const TARGETS = [
  { triple: "aarch64-apple-darwin", how: "native" },
  { triple: "x86_64-unknown-linux-gnu", how: "docker", platform: "linux/amd64" },
  { triple: "aarch64-unknown-linux-gnu", how: "docker", platform: "linux/arm64" },
];

// The words on the command line: the positional command and argument, the
// channel named by `--channel NAME`, and the flags. `--channel` takes a
// value, so that value is not a positional word.
export function parseArgs(args) {
  const channelIndex = args.indexOf("--channel");
  const channel = channelIndex === -1 ? null : (args[channelIndex + 1] ?? "");
  const positional = args.filter(
    (a, index) => !a.startsWith("--") && (channelIndex === -1 || index !== channelIndex + 1),
  );
  const [command = null, commandArg = null] = positional;
  // A bare `make publish` says patch; a channel build without a bump keeps
  // heading for the stable its current pre-release already names.
  const bump = command === "promote" || command === "keygen" ? null : command;
  return {
    command,
    commandArg,
    channel,
    bump,
    dryRun: args.includes("--dry-run"),
    skipTests: args.includes("--skip-tests"),
    skipWorkerImages: args.includes("--skip-worker-images"),
  };
}

const args = process.argv.slice(2);
const { command, commandArg, channel, bump, dryRun, skipTests } = parseArgs(args);
// Releases the artifacts without the worker image. The release itself is
// complete either way: `pm worker --sandbox` resolves the image at the
// launcher's own version, so a version published without one leaves
// sandboxed workers on that version with no image to pull.
const { skipWorkerImages } = parseArgs(args);

class PublishError extends Error {}

function die(message) {
  throw new PublishError(message);
}

function run(command, commandArgs, options = {}) {
  const result = spawnSync(command, commandArgs, {
    cwd: ROOT,
    stdio: options.capture ? ["ignore", "pipe", "pipe"] : "inherit",
    encoding: "utf8",
    ...options,
  });
  if (result.status !== 0) {
    const detail = options.capture ? `\n${result.stderr ?? ""}` : "";
    die(`${command} ${commandArgs.join(" ")} failed${detail}`);
  }
  return (result.stdout ?? "").trim();
}

function capture(command, commandArgs) {
  return run(command, commandArgs, { capture: true });
}

function tryCapture(command, commandArgs) {
  const result = spawnSync(command, commandArgs, { cwd: ROOT, encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : null;
}

// --- gates -----------------------------------------------------------

function currentBranch() {
  return capture("git", ["rev-parse", "--abbrev-ref", "HEAD"]);
}

// A channel build always comes from master. A stable release comes from
// master, or from a release branch when it is a hotfix to a line master
// has moved past.
export function branchMayRelease(branch, channel) {
  if (branch === "master") return true;
  return channel === null && /^release\/\d+\.\d+$/.test(branch);
}

function requireCleanBranch(channel) {
  const branch = currentBranch();
  if (!branchMayRelease(branch, channel)) {
    die(
      channel === null
        ? `stable releases come from master or a release/X.Y branch, not ${branch}`
        : `channel releases come from master, not ${branch}`,
    );
  }
  requireCleanTree();
}

function requireCleanTree() {
  if (capture("git", ["status", "--porcelain"]) !== "") {
    die("the working tree has changes; commit or stash them first");
  }
}

function requireProtocolBump(previousTag) {
  if (!previousTag) return;
  const diff = capture("git", ["diff", "--unified=0", `${previousTag}..HEAD`, "--", "proto"]);
  if (!protoContractChanged(diff)) return;
  // The constant covers the worker plane, so a change the client plane
  // keeps to itself asks nothing of it. Both ends of that plane ship in
  // one binary and are never a version apart.
  const schemaBefore = tryCapture("git", ["show", `${previousTag}:${PROTO_SCHEMA}`]);
  const schemaAfter = readFileSync(join(ROOT, PROTO_SCHEMA), "utf8");
  if (!workerContractChanged(schemaBefore, schemaAfter)) return;
  const before = tryCapture("git", ["show", `${previousTag}:crates/pm-protocol/src/lib.rs`]);
  const after = readFileSync(join(ROOT, "crates/pm-protocol/src/lib.rs"), "utf8");
  const read = readProtocolFrom;
  if (read(before) === read(after)) {
    die(
      `the worker contract changed since ${previousTag} but WORKER_PROTOCOL_VERSION is still ${read(after)}. ` +
        "The version names what a peer can use and a worker announcing an older one still registers, " +
        "so nothing is stranded by a missed bump. A bump tells peers that new API arrived: bump it if " +
        "this change adds capability, and ask whether that deserves a minor release.",
    );
  }
}

function requireVersionUnpublished(version) {
  const url = `${BUCKET_URL}/v${version}/release.json`;
  const status = tryCapture("curl", ["-fsS", "-o", "/dev/null", "-w", "%{http_code}", url]);
  if (status && status !== "404") {
    die(
      `${url} already returns ${status}; releases are never overwritten. If a previous run ` +
        `uploaded this version and then failed, clear it with: rclone purge ` +
        `${RCLONE_REMOTE}:${RCLONE_BUCKET}/v${version}`,
    );
  }
  // A registry accepts a second push to the same tag and the old digest
  // simply stops being referenced, so the version tag needs the same
  // refusal the bucket gets: a launcher pinned to it would silently
  // start running other bytes. A run that pushes no image cannot do
  // that, so an existing tag is not its problem.
  if (skipWorkerImages) return;
  const tag = `${IMAGE_REPO}:${version}`;
  if (tryCapture("docker", ["buildx", "imagetools", "inspect", tag]) !== null) {
    die(`${tag} is already published; delete that package version before releasing over it`);
  }
}

// rclone exiting zero only says the object reached the bucket. It says
// nothing about whether the bucket is the one the release host serves,
// which is a configuration a release can otherwise pass straight
// through, tagging a version nobody can install.
function requireReachable(key, expect) {
  if (dryRun) return;
  const url = `${BUCKET_URL}/${key}`;
  const body = tryCapture("curl", ["-fsSL", url]);
  if (body === null) {
    die(
      `uploaded ${key} but ${url} does not serve it. The bucket rclone wrote to is not the ` +
        `one ${BUCKET_URL} is bound to; check the bucket name and the custom domain.`,
    );
  }
  if (expect !== undefined && !servesSameContent(body, expect)) {
    die(`${url} serves different bytes than were uploaded`);
  }
}

// rclone creates a missing bucket rather than refusing, so a wrong name
// uploads a whole release somewhere nobody serves. Fail before building.
function requireBucket() {
  if (dryRun) return;
  if (tryCapture("rclone", ["lsf", "--max-depth", "1", `${RCLONE_REMOTE}:${RCLONE_BUCKET}`]) !== null) {
    return;
  }
  const buckets = tryCapture("rclone", ["lsf", `${RCLONE_REMOTE}:`]);
  die(
    `${RCLONE_REMOTE}:${RCLONE_BUCKET} is not reachable. Check that the ${RCLONE_REMOTE} remote ` +
      `is configured and its token can see the bucket ${BUCKET_URL} serves.` +
      `${buckets ? ` Available: ${buckets.split("\n").join(" ")}` : ""}`,
  );
}

// --- the worker image ------------------------------------------------

// The image's pm for one platform, staged out of the release artifact
// that was already built and signed for that architecture.
export function imageBinaryPath(platform, stage = STAGE) {
  return join(stage, "image", platform.split("/")[1], "pm");
}

function stageImageBinary(binary, platform) {
  const path = imageBinaryPath(platform);
  mkdirSync(dirname(path), { recursive: true });
  run("install", ["-m", "0755", binary, path]);
}

// A platform whose binary nothing staged would fail inside the build as
// a missing COPY source, which reads as a broken Dockerfile rather than
// a release target that no longer covers what the image publishes.
export function unstagedPlatforms(platforms, exists) {
  return platforms.filter((platform) => !exists(imageBinaryPath(platform)));
}

export function imageBuildArgs({ version, gitRev, push }) {
  return [
    "buildx",
    "build",
    "--builder",
    IMAGE_BUILDER,
    "--platform",
    IMAGE_PLATFORMS.join(","),
    "--build-arg",
    "PM_BINARY=prebuilt",
    "--build-arg",
    `PM_GIT_REV=${gitRev}`,
    "--tag",
    `${IMAGE_REPO}:${version}`,
    "-f",
    "docker/sandbox-worker/Dockerfile",
    // A dry run still builds both architectures, because an image that
    // cannot be built is exactly what a rehearsal is for. Nothing is
    // kept and nothing is pushed.
    push ? "--push" : "--output=type=cacheonly",
    ".",
  ];
}

// The platforms a pushed manifest list actually carries, as
// `imagetools inspect` prints them.
export function manifestPlatforms(inspected) {
  return inspected
    .split("\n")
    .map((line) => /^\s*Platform:\s*(\S+)/.exec(line)?.[1])
    .filter((platform) => platform !== undefined);
}

// Buildx names every platform its builder can produce, emulated ones
// included, so this is also the check for whether QEMU is installed.
export function uncoveredPlatforms(inspected, platforms) {
  const available = new Set(
    inspected
      .split("\n")
      .flatMap((line) => /^Platforms:\s*(.+)/.exec(line)?.[1]?.split(",") ?? [])
      .map((platform) => platform.trim()),
  );
  return platforms.filter((platform) => !available.has(platform));
}

function ensureImageBuilder() {
  if (tryCapture("docker", ["buildx", "inspect", IMAGE_BUILDER]) === null) {
    process.stderr.write(`publish: creating the ${IMAGE_BUILDER} buildx builder\n`);
    run("docker", ["buildx", "create", "--name", IMAGE_BUILDER, "--driver", "docker-container", "--bootstrap"], {
      capture: true,
    });
  }
  const inspected = tryCapture("docker", ["buildx", "inspect", IMAGE_BUILDER, "--bootstrap"]);
  if (inspected === null) die(`the ${IMAGE_BUILDER} buildx builder cannot be started`);
  const missing = uncoveredPlatforms(inspected, IMAGE_PLATFORMS);
  if (missing.length > 0) {
    die(
      `the ${IMAGE_BUILDER} builder cannot build ${missing.join(" or ")}. Install QEMU ` +
        "emulation for the foreign architecture with: docker run --privileged --rm " +
        "tonistiigi/binfmt --install all",
    );
  }
}

// docker has no command for "am I logged in", so the credential is
// looked up the way docker itself looks it up. Finding one does not
// prove it can write to the repository, but not finding one proves the
// push fails, and that is worth knowing before the builds rather than
// after them.
function requireRegistryLogin() {
  const host = IMAGE_REPO.split("/")[0];
  const token = process.env.PM_GHCR_TOKEN;
  if (token) {
    const login = spawnSync("docker", ["login", host, "--username", "oauth", "--password-stdin"], {
      cwd: ROOT,
      input: token,
      encoding: "utf8",
    });
    if (login.status !== 0) die(`PM_GHCR_TOKEN was rejected by ${host}: ${login.stderr?.trim() ?? ""}`);
    return;
  }
  const hint =
    `no credential for ${host}. Log in once with: docker login ${host} ` +
    "(username is your account, password is a token with write:packages), or set PM_GHCR_TOKEN " +
    "for an unattended run.";
  let config;
  try {
    const dir = process.env.DOCKER_CONFIG ?? join(process.env.HOME ?? "", ".docker");
    config = JSON.parse(readFileSync(join(dir, "config.json"), "utf8"));
  } catch {
    die(hint);
  }
  const helper = config.credHelpers?.[host] ?? config.credsStore;
  if (helper) {
    const got = spawnSync(`docker-credential-${helper}`, ["get"], { input: `${host}\n`, encoding: "utf8" });
    if (got.status !== 0) die(hint);
    return;
  }
  if (!config.auths?.[host]) die(hint);
}

// Before the builds, like requireBucket: a release that cannot push its
// image should not find that out an hour in.
function requireRegistry() {
  if (skipWorkerImages) return;
  ensureImageBuilder();
  if (dryRun) return;
  requireRegistryLogin();
}

// Pushing says the blobs arrived. It does not say the manifest list
// carries both architectures, and a host on the missing one pulls a
// manifest it cannot run.
function requireImagePublished(version) {
  if (dryRun) return;
  const tag = `${IMAGE_REPO}:${version}`;
  const inspected = tryCapture("docker", ["buildx", "imagetools", "inspect", tag]);
  if (inspected === null) die(`pushed ${tag} but the registry does not serve it`);
  const platforms = manifestPlatforms(inspected);
  const missing = IMAGE_PLATFORMS.filter((platform) => !platforms.includes(platform));
  if (missing.length > 0) {
    die(`${tag} is published without ${missing.join(" and ")}; it carries ${platforms.join(", ")}`);
  }
}

// Retags server side rather than building again, so the moving tag is a
// manifest copy of the exact bytes already verified above.
function pointLatestAtImage(version) {
  if (dryRun || skipWorkerImages) return;
  run("docker", [
    "buildx",
    "imagetools",
    "create",
    "--tag",
    `${IMAGE_REPO}:latest`,
    `${IMAGE_REPO}:${version}`,
  ]);
}

// What curl hands back has its trailing newline stripped, while the file
// on disk keeps one. Compare what the bytes mean rather than how they
// were captured, or every release fails its own verification.
export function servesSameContent(served, uploaded) {
  return served.trim() === uploaded.trim();
}

function requireGreen() {
  if (skipTests) {
    process.stderr.write("publish: skipping checks and tests by request\n");
    return;
  }
  run("make", ["check"]);
  run("make", ["test"]);
}

// --- version ---------------------------------------------------------

function currentVersion() {
  const manifest = readFileSync(join(ROOT, "Cargo.toml"), "utf8");
  const version = manifest.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) die("Cargo.toml has no workspace version");
  return version;
}

// A release version: three numbers and an optional pre-release suffix. Build
// metadata never appears in a published version.
export function parseVersion(text) {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/.exec(text);
  if (!match) return null;
  const [, major, minor, patch, pre] = match;
  return { major: Number(major), minor: Number(minor), patch: Number(patch), pre: pre ?? null };
}

function core({ major, minor, patch }) {
  return `${major}.${minor}.${patch}`;
}

function bumpCore(parsed, how) {
  if (how === "major") return `${parsed.major + 1}.0.0`;
  if (how === "minor") return `${parsed.major}.${parsed.minor + 1}.0`;
  if (how === "patch") return `${parsed.major}.${parsed.minor}.${parsed.patch + 1}`;
  die("say patch, minor, or major");
}

// The next stable version. A pre-release is not bumped past: the stable it
// leads to comes from `promote`, which cuts it from the pre-release's own
// commit.
export function nextVersion(version, how) {
  const parsed = parseVersion(version);
  if (!parsed) die(`cannot bump ${version}`);
  if (parsed.pre !== null) {
    die(
      `${version} is a pre-release. Promote it with \`make promote FROM=${version}\`, or ` +
        "publish another channel build with CHANNEL=<name>",
    );
  }
  return bumpCore(parsed, how ?? "patch");
}

// Channel names become pointer file names and version suffixes.
export const RESERVED_CHANNELS = ["stable", "latest", "channels"];
export function isChannelName(name) {
  return /^[a-z][a-z0-9]*$/.test(name) && !RESERVED_CHANNELS.includes(name);
}

// The next pre-release on a channel: X.Y.Z-channel.N. Without a bump, a
// current pre-release keeps the stable it already heads for, whatever
// channel it was on; a bump retargets and the counter starts over. N is one
// past the highest tag already cut for that target on that channel.
export function nextChannelVersion(version, how, channelName, tags) {
  const parsed = parseVersion(version);
  if (!parsed) die(`cannot bump ${version}`);
  if (!isChannelName(channelName)) {
    die(
      `${JSON.stringify(channelName)} is not a channel name: lowercase letters and digits, ` +
        `starting with a letter, and not ${RESERVED_CHANNELS.join(", ")}`,
    );
  }
  let target = how === null && parsed.pre !== null ? core(parsed) : bumpCore(parsed, how ?? "patch");
  // A target that has already shipped as stable cannot be led up to again,
  // which happens when a promote's merge back into master did not land:
  // master still names the old pre-release. Head for the next patch instead.
  while (tags.includes(`v${target}`)) target = bumpCore(parseVersion(target), "patch");
  const pattern = new RegExp(`^v${target.replace(/\./g, "\\.")}-${channelName}\\.(\\d+)$`);
  const highest = tags
    .map((tag) => pattern.exec(tag)?.[1])
    .filter((n) => n !== undefined)
    .reduce((max, n) => Math.max(max, Number(n)), 0);
  return `${target}-${channelName}.${highest + 1}`;
}

// The stable a pre-release leads to.
export function stableFromPreRelease(version) {
  const parsed = parseVersion(version);
  if (!parsed || parsed.pre === null) die(`${version} is not a pre-release`);
  return core(parsed);
}

export function releaseBranchFor(version) {
  const parsed = parseVersion(version);
  if (!parsed) die(`cannot read ${version}`);
  return `release/${parsed.major}.${parsed.minor}`;
}

// Where a release's pointer lives in the store.
export function pointerKey(channelName) {
  return channelName === null ? "latest.json" : `channels/${channelName}.json`;
}

// Semver precedence, as pm compares versions when it decides what is an
// upgrade. Used to settle which version master keeps when a release branch
// merges back into it.
export function compareVersions(a, b) {
  const left = parseVersion(a);
  const right = parseVersion(b);
  if (!left || !right) die(`cannot compare ${a} and ${b}`);
  for (const part of ["major", "minor", "patch"]) {
    if (left[part] !== right[part]) return left[part] < right[part] ? -1 : 1;
  }
  if (left.pre === null || right.pre === null) {
    if (left.pre === right.pre) return 0;
    return left.pre === null ? 1 : -1;
  }
  const lefts = left.pre.split(".");
  const rights = right.pre.split(".");
  for (let i = 0; i < Math.max(lefts.length, rights.length); i += 1) {
    if (lefts[i] === undefined) return -1;
    if (rights[i] === undefined) return 1;
    const numeric = /^\d+$/.test(lefts[i]) && /^\d+$/.test(rights[i]);
    if (numeric) {
      if (Number(lefts[i]) !== Number(rights[i])) return Number(lefts[i]) < Number(rights[i]) ? -1 : 1;
    } else if (/^\d+$/.test(lefts[i])) {
      return -1;
    } else if (/^\d+$/.test(rights[i])) {
      return 1;
    } else if (lefts[i] !== rights[i]) {
      return lefts[i] < rights[i] ? -1 : 1;
    }
  }
  return 0;
}

function allTags() {
  return capture("git", ["tag", "--list", "v*"]).split("\n").filter((tag) => tag !== "");
}

function writeVersion(version) {
  const path = join(ROOT, "Cargo.toml");
  const manifest = readFileSync(path, "utf8");
  writeFileSync(path, manifest.replace(/^version\s*=\s*"[^"]+"/m, `version = "${version}"`));
  run("cargo", ["update", "--workspace", "--offline"], { capture: true });
}

// Comment-only proto edits carry no new capability, so only changed
// messages, fields, and numbers ask for a bump.
export function protoContractChanged(diff) {
  return diff
    .split("\n")
    .filter((line) => /^[+-]/.test(line) && !/^(\+\+\+|---)/.test(line))
    .map((line) => line.slice(1).trim())
    .some((line) => line !== "" && !line.startsWith("//"));
}

// The envelopes a worker and a controller exchange. WORKER_PROTOCOL_VERSION
// describes what a peer reached through these may use, and nothing else.
//
// RepoOp and RepoAnswer travel as opaque bytes so the controller sends one
// encoding to a local worker and a remote one alike, which puts them beyond
// the type graph while leaving them squarely in the worker contract. A type
// tunnelled that way has to be named here or nothing will notice it change.
const WORKER_ROOTS = ["WorkerMessage", "ControllerMessage", "RepoOp", "RepoAnswer"];

const SCALAR_TYPES = new Set([
  "double", "float", "int32", "int64", "uint32", "uint64", "sint32", "sint64",
  "fixed32", "fixed64", "sfixed32", "sfixed64", "bool", "string", "bytes",
]);

// Splits a .proto into its top-level message and enum bodies. The schema
// has no nested definitions, so brace depth is enough to find the ends.
function protoBlocks(source) {
  const blocks = new Map();
  const lines = source.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const start = /^(message|enum)\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{/.exec(lines[i]);
    if (!start) continue;
    let depth = 0;
    const body = [];
    for (let j = i; j < lines.length; j += 1) {
      body.push(lines[j]);
      depth += (lines[j].match(/\{/g) ?? []).length;
      depth -= (lines[j].match(/\}/g) ?? []).length;
      if (depth === 0) {
        i = j;
        break;
      }
    }
    blocks.set(start[2], body.join("\n"));
  }
  return blocks;
}

// Every declared type a block names, map values and oneof arms included.
function referencedTypes(body) {
  const names = new Set();
  const withoutComments = body.replace(/\/\/.*$/gm, "");
  for (const [, key, value] of withoutComments.matchAll(/map<\s*([A-Za-z0-9_.]+)\s*,\s*([A-Za-z0-9_.]+)\s*>/g)) {
    names.add(key);
    names.add(value);
  }
  const fields = withoutComments.replace(/map<[^>]*>/g, "map");
  for (const [, type] of fields.matchAll(/^\s*(?:repeated\s+|optional\s+)?([A-Za-z_][A-Za-z0-9_.]*)\s+[a-z_][a-z0-9_]*\s*=\s*\d+\s*;/gm)) {
    names.add(type);
  }
  for (const scalar of SCALAR_TYPES) names.delete(scalar);
  names.delete("map");
  return names;
}

// The messages and enums a worker peer can actually reach, walked from the
// worker envelopes. Membership of an envelope is not enough on its own: a
// type shared with the client plane still counts, and one reached only from
// a ClientMessage never does.
export function workerReachableTypes(source, roots = WORKER_ROOTS) {
  const blocks = protoBlocks(source);
  const reached = new Set();
  // An older revision predates whatever it predates, so an absent root
  // there is simply a type that had not been added yet.
  const queue = roots.filter((root) => blocks.has(root));
  while (queue.length > 0) {
    const name = queue.pop();
    if (reached.has(name)) continue;
    reached.add(name);
    for (const type of referencedTypes(blocks.get(name) ?? "")) {
      if (blocks.has(type) && !reached.has(type)) queue.push(type);
    }
  }
  return reached;
}

// The worker-facing schema alone, with comments and blank lines dropped so
// only a contract change registers. Types are emitted in a fixed order, so
// moving a message around the file reads as no change.
export function workerContract(source) {
  const blocks = protoBlocks(source);
  return [...workerReachableTypes(source)]
    .sort()
    .map((name) =>
      (blocks.get(name) ?? "")
        .replace(/\/\/.*$/gm, "")
        .split("\n")
        .map((line) => line.trim())
        .filter((line) => line !== "")
        .join("\n"),
    )
    .join("\n");
}

// Whether anything a worker peer can reach changed between two revisions of
// the schema. A client-plane edit reaches no worker and answers false.
export function workerContractChanged(before, after) {
  if (before == null || after == null) return true;
  // Only the schema in hand has to still name every root. A rename there
  // would walk nothing, and an empty contract compares equal forever.
  const missing = WORKER_ROOTS.filter((root) => !protoBlocks(after).has(root));
  if (missing.length > 0) {
    die(`the schema has no ${missing.join(", ")}; update WORKER_ROOTS to match it`);
  }
  return workerContract(before) !== workerContract(after);
}

// Reads the worker protocol constant out of pm-protocol's source.
export function readProtocolFrom(source) {
  return source?.match(/WORKER_PROTOCOL_VERSION:\s*u32\s*=\s*(\d+)/)?.[1] ?? null;
}

// The manifest published for a release, and read back by every client. The
// channel and the pre-release a stable was promoted from appear only when
// they apply, so a stable release's manifest reads as it always has.
export function releaseManifest(version, protocol, artifacts, { channel = null, promotedFrom = null } = {}) {
  const manifest = { version, protocolVersion: protocol, artifacts };
  if (channel !== null) manifest.channel = channel;
  if (promotedFrom !== null) manifest.promotedFrom = promotedFrom;
  return manifest;
}

function protocolVersion() {
  const value = readProtocolFrom(
    readFileSync(join(ROOT, "crates/pm-protocol/src/lib.rs"), "utf8"),
  );
  if (!value) die("cannot read WORKER_PROTOCOL_VERSION");
  return Number(value);
}

// --- build -----------------------------------------------------------

function buildTarget(target, gitRev, version) {
  const output = join(STAGE, `pm-${version}-${target.triple}`);
  if (target.how === "native") {
    if (process.platform !== "darwin") {
      die(`${target.triple} can only be built on macOS; run this from the host`);
    }
    run("cargo", ["build", "--locked", "--release", "-p", "pm", "--target", target.triple], {
      env: { ...process.env, PM_GIT_REV: gitRev },
    });
    run("install", ["-m", "0755", join(ROOT, "target", target.triple, "release", "pm"), output]);
  } else {
    // --output writes the scratch stage straight to disk, so no container
    // has to be created and removed to get one file out.
    run("docker", [
      "build",
      "--platform",
      target.platform,
      "--build-arg",
      `PM_GIT_REV=${gitRev}`,
      "-f",
      "docker/release/Dockerfile",
      "--output",
      `type=local,dest=${join(STAGE, target.triple)}`,
      ".",
    ]);
    run("install", ["-m", "0755", join(STAGE, target.triple, "pm"), output]);
    rmSync(join(STAGE, target.triple), { recursive: true, force: true });
  }
  return output;
}

// --- sign and publish ------------------------------------------------

// Ed25519 over the artifact bytes, base64 encoded. pm verifies this with
// the public half compiled into it.
function sign(path) {
  const signature = signEd25519(null, readFileSync(path), releaseKey());
  writeFileSync(`${path}.sig`, `${signature.toString("base64")}\n`);
  return `${path}.sig`;
}

let cachedKey = null;
function releaseKey() {
  if (cachedKey) return cachedKey;
  let pem;
  try {
    pem = readFileSync(SIGNING_KEY, "utf8");
  } catch {
    die(`no release key at ${SIGNING_KEY}; create one with ./scripts/publish.mjs keygen`);
  }
  const passphrase = process.env.PM_RELEASE_KEY_PASSPHRASE ?? promptSecret("release key passphrase: ");
  try {
    cachedKey = createPrivateKey({ key: pem, passphrase });
  } catch {
    die("the release key could not be unlocked; wrong passphrase?");
  }
  return cachedKey;
}

// Reads a line without echoing it, so a passphrase does not end up in
// the scrollback of whatever terminal published the release.
function promptSecret(prompt) {
  process.stderr.write(prompt);
  const hadEcho = spawnSync("stty", ["-echo"], { stdio: ["inherit", "ignore", "ignore"] }).status === 0;
  try {
    const buffer = Buffer.alloc(4096);
    let length = 0;
    while (length < buffer.length) {
      let read;
      try {
        read = readSync(0, buffer, length, 1, null);
      } catch {
        break;
      }
      if (read === 0 || buffer[length] === 0x0a) break;
      length += read;
    }
    return buffer.subarray(0, length).toString("utf8").replace(/\r$/, "");
  } finally {
    if (hadEcho) spawnSync("stty", ["echo"], { stdio: ["inherit", "ignore", "ignore"] });
    process.stderr.write("\n");
  }
}

// Creates the release key pair and prints the public half to paste into
// crates/pm-daemon/src/release_pubkey.txt.
function keygen() {
  if (existsSync(SIGNING_KEY)) die(`${SIGNING_KEY} already exists; refusing to overwrite it`);
  const passphrase = process.env.PM_RELEASE_KEY_PASSPHRASE ?? promptSecret("new release key passphrase: ");
  if (passphrase.length < 12) die("use a passphrase of at least 12 characters");
  const { privateKey, publicKey } = generateKeyPairSync("ed25519", {
    privateKeyEncoding: { type: "pkcs8", format: "pem", cipher: "aes-256-cbc", passphrase },
    publicKeyEncoding: { type: "spki", format: "pem" },
  });
  mkdirSync(dirname(SIGNING_KEY), { recursive: true, mode: 0o700 });
  writeFileSync(SIGNING_KEY, privateKey, { mode: 0o600 });
  process.stderr.write(`Wrote ${SIGNING_KEY}. Back it up: losing it means every installed pm stops trusting new releases.\n`);
  process.stdout.write(`${publicKeyBase64(publicKey)}\n`);
}

// The raw 32-byte public key, which is what pm compares against.
export function publicKeyBase64(publicKeyPem) {
  const jwk = createPublicKey(publicKeyPem).export({ format: "jwk" });
  return Buffer.from(jwk.x, "base64url").toString("base64");
}

function digest(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function upload(localPath, key, cacheControl) {
  if (dryRun) {
    process.stderr.write(`publish: would upload ${key}\n`);
    return;
  }
  run("rclone", [
    "copyto",
    localPath,
    `${RCLONE_REMOTE}:${RCLONE_BUCKET}/${key}`,
    "--header-upload",
    `Cache-Control: ${cacheControl}`,
  ]);
}

const IMMUTABLE = "public, max-age=31536000, immutable";
const ALWAYS_FRESH = "public, max-age=60, must-revalidate";

// --- main ------------------------------------------------------------

let bumped = false;
let committed = false;
// Set by a promote: the branch it created, if it created one, and where
// to return to.
let createdBranch = null;
let returnTo = null;

function requireTools() {
  for (const tool of ["git", "cargo", "make", "docker", "rclone", "curl"]) {
    if (!tryCapture("command", ["-v", tool]) && !tryCapture("which", [tool])) {
      die(`${tool} is required`);
    }
  }
}

// Builds, signs and uploads one version, then points the store at it. The
// version is already written and committed when this runs.
function buildAndPublish({ version, channelName, promotedFrom }) {
  const gitRev = capture("git", ["rev-parse", "--short", "HEAD"]);

  rmSync(STAGE, { recursive: true, force: true });
  mkdirSync(STAGE, { recursive: true });
  run("make", ["web"]);

  const artifacts = {};
  for (const target of TARGETS) {
    process.stderr.write(`publish: building ${target.triple}\n`);
    const binary = buildTarget(target, gitRev, version);
    if (target.platform && !skipWorkerImages) stageImageBinary(binary, target.platform);
    const signature = sign(binary);
    artifacts[target.triple] = {
      path: `v${version}/${binary.split("/").pop()}`,
      sha256: digest(binary),
    };
    upload(binary, artifacts[target.triple].path, IMMUTABLE);
    upload(signature, `${artifacts[target.triple].path}.sig`, IMMUTABLE);
  }

  // The image carries the Linux binaries just built, so it goes up with
  // them, under the version tag alone. Nothing resolves that tag by
  // accident, the same way a versioned artifact path does not.
  if (skipWorkerImages) {
    process.stderr.write("publish: skipping the worker image\n");
  } else {
    const unstaged = unstagedPlatforms(IMAGE_PLATFORMS, existsSync);
    if (unstaged.length > 0) {
      die(`the image publishes ${unstaged.join(" and ")}, which no release target built`);
    }
    process.stderr.write(`publish: building the worker image for ${IMAGE_PLATFORMS.join(" and ")}\n`);
    run("docker", imageBuildArgs({ version, gitRev, push: !dryRun }));
    requireImagePublished(version);
  }

  const release = releaseManifest(version, protocolVersion(), artifacts, {
    channel: channelName,
    promotedFrom,
  });
  const manifest = join(STAGE, "release.json");
  writeFileSync(manifest, `${JSON.stringify(release, null, 2)}\n`);
  const manifestSignature = sign(manifest);
  upload(manifest, `v${version}/release.json`, IMMUTABLE);
  upload(manifestSignature, `v${version}/release.json.sig`, IMMUTABLE);

  // Prove the release is actually served before anything points at it.
  const manifestBody = readFileSync(manifest, "utf8");
  requireReachable(`v${version}/release.json`, manifestBody);
  for (const artifact of Object.values(artifacts)) {
    requireReachable(`${artifact.path}.sig`);
  }

  // Everything above is addressable but unreferenced until this lands.
  const pointer = pointerKey(channelName);
  upload(manifest, pointer, ALWAYS_FRESH);
  upload(manifestSignature, `${pointer}.sig`, ALWAYS_FRESH);
  requireReachable(pointer, manifestBody);
  if (channelName === null) {
    upload(join(ROOT, "scripts/install.sh"), "install.sh", ALWAYS_FRESH);
    requireReachable("install.sh");
    pointLatestAtImage(version);
  }
}

function tagRelease(version) {
  run("git", ["tag", "-a", `v${version}`, "-m", `Release ${version}.`]);
}

async function publish() {
  if (channel !== null && !isChannelName(channel)) {
    die(
      `${JSON.stringify(channel)} is not a channel name: lowercase letters and digits, ` +
        `starting with a letter, and not ${RESERVED_CHANNELS.join(", ")}`,
    );
  }
  // A sandboxed worker pulls the image at the launcher's exact version, and
  // a channel build is exactly what a controller pulls its workers onto.
  if (channel !== null && skipWorkerImages) {
    die("a channel release needs its worker image; drop --skip-worker-images");
  }
  requireTools();
  releaseKey();
  requireCleanBranch(channel);
  const previousTag = tryCapture("git", ["describe", "--tags", "--abbrev=0", "--match", "v*"]);
  requireProtocolBump(previousTag);
  requireBucket();
  requireRegistry();
  requireGreen();

  const version =
    channel === null
      ? nextVersion(currentVersion(), bump)
      : nextChannelVersion(currentVersion(), bump, channel, allTags());
  requireVersionUnpublished(version);
  process.stderr.write(
    `publish: releasing ${version}${channel === null ? "" : ` on the ${channel} channel`}` +
      `${dryRun ? " (dry run)" : ""}\n`,
  );

  writeVersion(version);
  bumped = true;
  if (!dryRun) {
    run("git", ["commit", "-am", `Release ${version}.`]);
    committed = true;
  }

  buildAndPublish({ version, channelName: channel, promotedFrom: null });

  if (dryRun) {
    rollback({ bumped, committed: false });
    process.stderr.write(`publish: dry run complete, ${version} was built and signed but not published\n`);
    return;
  }
  tagRelease(version);
  const branch = currentBranch();
  process.stderr.write(`publish: ${version} is live. Push it with: git push origin ${branch} v${version}\n`);
  if (channel === null && branch !== "master") mergeBackToMaster(branch);
}

// Merges a release branch into master so master's Cargo.toml stays ahead of
// every published version. Only the version files may conflict, and they
// resolve to whichever version is higher; anything else is left to a person.
function mergeBackToMaster(branch) {
  run("git", ["checkout", "master"]);
  const merged = spawnSync("git", ["merge", "--no-ff", "-m", `Merge ${branch} into master.`, branch], {
    cwd: ROOT,
    encoding: "utf8",
  });
  if (merged.status === 0) {
    process.stderr.write(`publish: merged ${branch} into master; push master too\n`);
    return;
  }
  const conflicted = capture("git", ["diff", "--name-only", "--diff-filter=U"]).split("\n").filter(Boolean);
  if (conflicted.some((file) => file !== "Cargo.toml" && file !== "Cargo.lock")) {
    run("git", ["merge", "--abort"], { capture: true });
    die(
      `${branch} does not merge cleanly into master (${conflicted.join(", ")}). ` +
        `The release is published; merge ${branch} into master by hand.`,
    );
  }
  const ours = capture("git", ["show", "master:Cargo.toml"]).match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const theirs = capture("git", ["show", `${branch}:Cargo.toml`]).match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!ours || !theirs) die("cannot read the versions being merged");
  run("git", ["checkout", "--ours", "Cargo.toml", "Cargo.lock"], { capture: true });
  writeVersion(compareVersions(ours, theirs) >= 0 ? ours : theirs);
  run("git", ["add", "Cargo.toml", "Cargo.lock"]);
  run("git", ["commit", "--no-edit"], { capture: true });
  process.stderr.write(`publish: merged ${branch} into master; push master too\n`);
}

// Cuts the stable a pre-release leads to, from that pre-release's own
// commit, on the release branch for its line.
async function promote(from) {
  if (!from) die("say which pre-release to promote, e.g. promote 0.10.0-dev.3");
  const stable = stableFromPreRelease(from);
  if (skipWorkerImages) die("a promoted release needs its worker image; drop --skip-worker-images");
  requireTools();
  if (tryCapture("git", ["rev-parse", "--verify", `v${stable}^{commit}`]) !== null) {
    die(`${stable} is already released as v${stable}; cut a new channel build, which will head for the next patch`);
  }
  releaseKey();
  if (currentBranch() !== "master") die("start a promote from master, so the release branch can merge back into it");
  requireCleanTree();
  if (tryCapture("git", ["rev-parse", "--verify", `v${from}^{commit}`]) === null) {
    die(`no tag v${from} here; fetch tags or publish that pre-release first`);
  }
  const published = tryCapture("curl", ["-fsS", "-o", "/dev/null", "-w", "%{http_code}", `${BUCKET_URL}/v${from}/release.json`]);
  if (published !== "200") die(`${BUCKET_URL}/v${from}/release.json is not served, so ${from} was never published`);
  requireVersionUnpublished(stable);
  requireBucket();
  requireRegistry();

  const branch = releaseBranchFor(stable);
  const tagCommit = capture("git", ["rev-parse", `v${from}^{commit}`]);
  returnTo = "master";
  if (tryCapture("git", ["rev-parse", "--verify", `refs/heads/${branch}`]) === null) {
    run("git", ["checkout", "-b", branch, tagCommit]);
    createdBranch = branch;
  } else {
    const head = capture("git", ["rev-parse", branch]);
    if (head !== tagCommit && tryCapture("git", ["merge-base", "--is-ancestor", head, tagCommit]) === null) {
      die(
        `${branch} already exists at ${head.slice(0, 7)}, which cannot fast-forward to v${from}. ` +
          "Promote the pre-release cut from that branch, or publish a stable release from it.",
      );
    }
    run("git", ["checkout", branch]);
    if (head !== tagCommit) run("git", ["merge", "--ff-only", tagCommit]);
  }
  requireGreen();

  process.stderr.write(`publish: promoting ${from} to ${stable}${dryRun ? " (dry run)" : ""}\n`);
  writeVersion(stable);
  bumped = true;
  if (!dryRun) {
    run("git", ["commit", "-am", `Release ${stable}.`]);
    committed = true;
  }
  buildAndPublish({ version: stable, channelName: null, promotedFrom: from });

  if (dryRun) {
    rollback({ bumped, committed: false });
    process.stderr.write(`publish: dry run complete, ${stable} was built and signed but not published\n`);
    return;
  }
  tagRelease(stable);
  process.stderr.write(`publish: ${stable} is live. Push it with: git push origin ${branch} v${stable}\n`);
  mergeBackToMaster(branch);
}

// Undoes whatever the run had changed locally. Nothing is uploaded until
// every artifact is built and signed, and the pointer lands last, so a
// failed run leaves neither a release nor a trace of one.
function rollback({ bumped: wasBumped, committed: wasCommitted }) {
  if (wasBumped) {
    if (wasCommitted) {
      run("git", ["reset", "--hard", "HEAD~1"], { capture: true });
    } else {
      run("git", ["checkout", "--", "Cargo.toml", "Cargo.lock"], { capture: true });
    }
  }
  if (returnTo !== null && currentBranch() !== returnTo) {
    run("git", ["checkout", returnTo], { capture: true });
  }
  if (createdBranch !== null) {
    run("git", ["branch", "-D", createdBranch], { capture: true });
  }
}

async function main() {
  if (command === "keygen") {
    keygen();
    return;
  }
  if (command === "promote") {
    await promote(commandArg);
    return;
  }
  if (bump !== null && !["patch", "minor", "major"].includes(bump)) die("say patch, minor, or major");
  await publish();
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    await main();
  } catch (error) {
    if (!(error instanceof PublishError)) throw error;
    try {
      rollback({ bumped, committed });
    } catch (cleanup) {
      process.stderr.write(`publish: could not undo the version bump: ${cleanup.message}\n`);
    }
    process.stderr.write(`publish: ${error.message}\n`);
    process.exit(1);
  }
}

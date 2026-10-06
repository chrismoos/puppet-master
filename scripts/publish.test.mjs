import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  IMAGE_PLATFORMS,
  TARGETS,
  branchMayRelease,
  compareVersions,
  imageBinaryPath,
  imageBuildArgs,
  isChannelName,
  manifestPlatforms,
  nextChannelVersion,
  nextVersion,
  parseArgs,
  parseVersion,
  pointerKey,
  protoContractChanged,
  publicKeyBase64,
  readProtocolFrom,
  releaseBranchFor,
  releaseManifest,
  servesSameContent,
  stableFromPreRelease,
  uncoveredPlatforms,
  unstagedPlatforms,
  workerContract,
  workerContractChanged,
  workerReachableTypes,
} from "./publish.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

test("a bump moves exactly one component and clears the ones below it", () => {
  assert.equal(nextVersion("0.1.0", "patch"), "0.1.1");
  assert.equal(nextVersion("0.1.9", "minor"), "0.2.0");
  assert.equal(nextVersion("0.9.3", "major"), "1.0.0");
  assert.equal(nextVersion("0.9.3", null), "0.9.4", "no bump means patch");
});

test("the command line is read the same with and without a channel", () => {
  assert.deepEqual(parseArgs(["promote", "0.9.21-dev.1"]), {
    command: "promote", commandArg: "0.9.21-dev.1", channel: null, bump: null,
    dryRun: false, skipTests: false, skipWorkerImages: false,
  });
  assert.equal(parseArgs(["patch"]).bump, "patch");
  assert.equal(parseArgs([]).bump, null, "a bare publish leaves the bump to the script");
  assert.equal(parseArgs(["keygen"]).bump, null);
  const dev = parseArgs(["minor", "--channel", "dev", "--dry-run"]);
  assert.equal(dev.bump, "minor");
  assert.equal(dev.channel, "dev");
  assert.ok(dev.dryRun);
  const bare = parseArgs(["--channel", "dev"]);
  assert.equal(bare.bump, null);
  assert.equal(bare.channel, "dev");
  assert.equal(parseArgs(["--channel"]).channel, "", "a missing channel name is refused later, not read as a word");
});

test("a stable release is never bumped past a pre-release", () => {
  assert.throws(() => nextVersion("0.10.0-dev.3", "patch"), /promote/);
});

test("versions parse with and without a pre-release and nothing else", () => {
  assert.deepEqual(parseVersion("0.10.0"), { major: 0, minor: 10, patch: 0, pre: null });
  assert.deepEqual(parseVersion("0.10.0-dev.3"), { major: 0, minor: 10, patch: 0, pre: "dev.3" });
  for (const bad of ["0.10", "0.10.0.1", "0.10.0+abc", "v0.10.0", "0.10.0-"]) {
    assert.equal(parseVersion(bad), null, bad);
  }
});

test("a channel build heads for the next stable and counts within its target", () => {
  const tags = ["v0.9.17", "v0.10.0-dev.1", "v0.10.0-dev.2", "v0.10.0-foo.1"];
  // From a stable, the default target is the next patch.
  assert.equal(nextChannelVersion("0.9.17", null, "dev", []), "0.9.18-dev.1");
  // The counter comes from the tags already cut for that target on that channel.
  assert.equal(nextChannelVersion("0.9.17", "minor", "dev", tags), "0.10.0-dev.3");
  assert.equal(nextChannelVersion("0.9.17", "minor", "foo", tags), "0.10.0-foo.2");
  // A current pre-release keeps its target without a bump, on any channel.
  assert.equal(nextChannelVersion("0.10.0-dev.2", null, "dev", tags), "0.10.0-dev.3");
  assert.equal(nextChannelVersion("0.10.0-dev.2", null, "bar", tags), "0.10.0-bar.1");
  // A bump retargets and the counter starts over.
  assert.equal(nextChannelVersion("0.10.0-dev.2", "minor", "dev", tags), "0.11.0-dev.1");
  assert.equal(nextChannelVersion("0.10.0-dev.2", "patch", "dev", tags), "0.10.1-dev.1");
});

test("a channel build never heads for a stable that already shipped", () => {
  // master still says 0.10.0-dev.4 because the promote's merge back did not land.
  const tags = ["v0.10.0-dev.4", "v0.10.0", "v0.10.1", "v0.10.2-dev.1"];
  assert.equal(nextChannelVersion("0.10.0-dev.4", null, "dev", tags), "0.10.2-dev.2");
  assert.equal(nextChannelVersion("0.9.17", "minor", "dev", tags), "0.10.2-dev.2");
});

test("channel names are lowercase words and never a name the store uses", () => {
  for (const good of ["dev", "beta", "foo2"]) assert.ok(isChannelName(good), good);
  for (const bad of ["", "stable", "latest", "channels", "Dev", "dev-1", "2dev"]) {
    assert.ok(!isChannelName(bad), bad);
  }
  assert.throws(() => nextChannelVersion("0.9.17", null, "stable", []), /not a channel name/);
});

test("a promote cuts the stable a pre-release leads to, on its line's branch", () => {
  assert.equal(stableFromPreRelease("0.10.0-dev.3"), "0.10.0");
  assert.throws(() => stableFromPreRelease("0.10.0"), /not a pre-release/);
  assert.equal(releaseBranchFor("0.10.0"), "release/0.10");
  assert.equal(releaseBranchFor("1.2.3-rc.1"), "release/1.2");
});

test("each channel has its own pointer and stable keeps latest.json", () => {
  assert.equal(pointerKey(null), "latest.json");
  assert.equal(pointerKey("dev"), "channels/dev.json");
});

test("versions compare by semver precedence", () => {
  const ascending = ["0.9.17", "0.10.0-alpha", "0.10.0-alpha.1", "0.10.0-beta", "0.10.0-beta.2", "0.10.0-beta.11", "0.10.0-rc.1", "0.10.0", "0.10.1-dev.1", "0.10.1"];
  for (let i = 1; i < ascending.length; i += 1) {
    assert.equal(compareVersions(ascending[i - 1], ascending[i]), -1, `${ascending[i - 1]} < ${ascending[i]}`);
    assert.equal(compareVersions(ascending[i], ascending[i - 1]), 1);
  }
  assert.equal(compareVersions("0.10.0-dev.3", "0.10.0-dev.3"), 0);
});

test("channel builds come from master and stable also from a release branch", () => {
  assert.ok(branchMayRelease("master", null));
  assert.ok(branchMayRelease("master", "dev"));
  assert.ok(branchMayRelease("release/0.9", null));
  assert.ok(!branchMayRelease("release/0.9", "dev"));
  assert.ok(!branchMayRelease("feature/x", null));
  assert.ok(!branchMayRelease("release/0.9.1", null));
});

test("a manifest names its channel and provenance only when it has them", () => {
  const plain = releaseManifest("1.2.3", 5, {});
  assert.deepEqual(Object.keys(plain), ["version", "protocolVersion", "artifacts"]);
  const dev = releaseManifest("1.2.3-dev.1", 5, {}, { channel: "dev" });
  assert.equal(dev.channel, "dev");
  assert.ok(!("promotedFrom" in dev));
  const promoted = releaseManifest("1.2.3", 5, {}, { promotedFrom: "1.2.3-dev.1" });
  assert.equal(promoted.promotedFrom, "1.2.3-dev.1");
  assert.ok(!("channel" in promoted));
});

test("only a proto edit that touches the contract asks for a protocol bump", () => {
  const commentOnly = [
    "--- a/proto/pm/v1/pm.proto",
    "+++ b/proto/pm/v1/pm.proto",
    "@@ -1,2 +1,2 @@",
    "-  // Old wording.",
    "+  // New wording.",
    "+",
  ].join("\n");
  assert.equal(protoContractChanged(commentOnly), false);
  assert.equal(protoContractChanged(""), false);
  const field = `${commentOnly}\n+  uint32 http_port = 5;`;
  assert.equal(protoContractChanged(field), true);
  const reserved = `${commentOnly}\n-  reserved 4, 5;`;
  assert.equal(protoContractChanged(reserved), true);
});

test("the protocol constant is read from pm-protocol's source", () => {
  assert.equal(readProtocolFrom("pub const WORKER_PROTOCOL_VERSION: u32 = 7;"), "7");
  assert.equal(readProtocolFrom("nothing here"), null);
  assert.equal(readProtocolFrom(null), null);
});

// The gate compares this reading before and after a proto change, so it
// has to keep matching the real source rather than a shape that has
// drifted away from it.
test("the protocol constant is readable from the checked-in source", () => {
  const source = readFileSync(join(ROOT, "crates/pm-protocol/src/lib.rs"), "utf8");
  const value = readProtocolFrom(source);
  assert.ok(value !== null, "WORKER_PROTOCOL_VERSION should be readable");
  assert.ok(Number(value) > 0);
});

// Clients read these exact keys. install.sh parses them with sed, and
// the Rust client deserializes them, so the names are a contract.
test("the manifest carries the fields clients read", () => {
  const artifacts = {
    "aarch64-apple-darwin": { path: "v1.2.3/pm-1.2.3-aarch64-apple-darwin", sha256: "ab" },
  };
  const manifest = releaseManifest("1.2.3", 5, artifacts);
  assert.deepEqual(Object.keys(manifest), ["version", "protocolVersion", "artifacts"]);
  assert.equal(manifest.version, "1.2.3");
  assert.equal(manifest.protocolVersion, 5);
  assert.deepEqual(manifest.artifacts, artifacts);
});

// install.sh reads the manifest with sed rather than a JSON parser, so
// the serialized form has to stay within what that can follow.
test("install.sh can pull an artifact out of the serialized manifest", async () => {
  const { execFileSync } = await import("node:child_process");
  const manifest = JSON.stringify(
    releaseManifest("1.2.3", 5, {
      "x86_64-unknown-linux-gnu": { path: "v1.2.3/pm-x86", sha256: "aaa" },
      "aarch64-unknown-linux-gnu": { path: "v1.2.3/pm-arm", sha256: "bbb" },
    }),
    null,
    2,
  );
  const script = `
    manifest=$(cat); TARGET=aarch64-unknown-linux-gnu
    printf '%s' "$manifest" | tr -d '\\n ' |
      sed -n "s/.*\\"$TARGET\\":{\\([^}]*\\)}.*/\\1/p" |
      sed -n "s/.*\\"sha256\\":\\"\\([^\\"]*\\)\\".*/\\1/p"
  `;
  const found = execFileSync("sh", ["-c", script], { input: manifest, encoding: "utf8" }).trim();
  assert.equal(found, "bbb", "the parser must pick the requested target's digest");
});

// pm compares the raw 32-byte key, so the exported form has to be that
// and not the SPKI wrapper it is generated in.
test("the published public key is the raw Ed25519 key", async () => {
  const { generateKeyPairSync } = await import("node:crypto");
  const { publicKey } = generateKeyPairSync("ed25519", {
    publicKeyEncoding: { type: "spki", format: "pem" },
  });
  const encoded = publicKeyBase64(publicKey);
  assert.equal(Buffer.from(encoded, "base64").length, 32);
});

// The signature the release script writes is what pm verifies, so the
// encoding is a contract between the two.
test("a release signature is base64 of a raw Ed25519 signature", async () => {
  const { generateKeyPairSync, sign, verify } = await import("node:crypto");
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const payload = Buffer.from("artifact bytes");
  const signature = sign(null, payload, privateKey);
  assert.equal(signature.length, 64);
  const roundTripped = Buffer.from(signature.toString("base64"), "base64");
  assert.ok(verify(null, payload, publicKey, roundTripped));
});

// The manifest is written with a trailing newline and read back through
// curl, which strips it. Comparing the captured forms directly failed
// every release against a bucket that held exactly the right bytes.
test("a served manifest matches the uploaded one despite a trailing newline", () => {
  const uploaded = `${JSON.stringify(releaseManifest("1.2.3", 5, {}), null, 2)}\n`;
  const served = uploaded.trimEnd();
  assert.ok(servesSameContent(served, uploaded));
});

test("genuinely different content still fails", () => {
  const uploaded = `${JSON.stringify(releaseManifest("1.2.3", 5, {}), null, 2)}\n`;
  const other = `${JSON.stringify(releaseManifest("1.2.4", 5, {}), null, 2)}\n`;
  assert.ok(!servesSameContent(other, uploaded));
});

const SCHEMA = readFileSync(join(ROOT, "proto/pm/v1/pm.proto"), "utf8");

test("the worker plane is walked from its envelopes, not matched by name", () => {
  const reachable = workerReachableTypes(SCHEMA);
  // The envelopes themselves and a message carried inside one.
  assert.ok(reachable.has("WorkerMessage"));
  assert.ok(reachable.has("ControllerMessage"));
  assert.ok(reachable.has("WorkerRegister"));
  assert.ok(reachable.has("WorkerTerminal"));
  // Reached only through ControllerRepoRequest, which is what the review
  // repo-read capability added.
  assert.ok(reachable.has("RepoOp"));
  assert.ok(reachable.has("RepoAnswer"));
  // Client-plane traffic a worker never sees.
  assert.ok(!reachable.has("ClientMessage"));
  assert.ok(!reachable.has("ReviewViewerState"));
  assert.ok(!reachable.has("SetReviewViewerState"));
});

test("a client-plane field does not ask for a worker protocol bump", () => {
  const after = SCHEMA.replace(
    "message SetReviewViewerState {",
    "message SetReviewViewerState {\n  optional string draft_key = 99;",
  );
  assert.notEqual(after, SCHEMA);
  assert.equal(workerContractChanged(SCHEMA, after), false);
});

test("a field on the worker plane still asks for a bump", () => {
  const after = SCHEMA.replace(
    "message WorkerRegister {",
    "message WorkerRegister {\n  string new_capability = 99;",
  );
  assert.notEqual(after, SCHEMA);
  assert.equal(workerContractChanged(SCHEMA, after), true);
});

test("a field on a message shared with the client plane asks for a bump", () => {
  const after = SCHEMA.replace(
    "message WorkerTerminal {",
    "message WorkerTerminal {\n  bool new_flag = 99;",
  );
  assert.notEqual(after, SCHEMA);
  assert.equal(workerContractChanged(SCHEMA, after), true);
});

test("comments and moved messages are not contract changes", () => {
  const recommented = SCHEMA.replace(
    "// Scroll offset keyed by",
    "// Reworded entirely, keyed by",
  );
  assert.notEqual(recommented, SCHEMA);
  assert.equal(workerContractChanged(SCHEMA, recommented), false);
  assert.equal(workerContract(SCHEMA), workerContract(`${SCHEMA}\n`));
});

test("an unreadable revision is treated as a change rather than waved through", () => {
  assert.equal(workerContractChanged(null, SCHEMA), true);
  assert.equal(workerContractChanged(SCHEMA, null), true);
});

test("a change to a tunnelled worker type still asks for a bump", () => {
  const after = SCHEMA.replace(
    "message RepoOp {",
    "message RepoOp {\n  bool new_op = 99;",
  );
  assert.notEqual(after, SCHEMA);
  assert.equal(workerContractChanged(SCHEMA, after), true);
});

test("a renamed root is refused rather than silently walking nothing", () => {
  const renamed = SCHEMA.replace("message WorkerMessage {", "message WorkerEnvelope {");
  assert.notEqual(renamed, SCHEMA);
  assert.throws(() => workerContractChanged(SCHEMA, renamed), /WorkerMessage/);
  // The older side is allowed to predate a root without tripping it.
  assert.equal(workerContractChanged(renamed, SCHEMA), true);
});

// The key is only needed to sign, which is three release builds in. Asking
// for it there meant a missing key wasted the whole run and an unattended
// one blocked at the prompt.
test("a missing release key fails before any gate or build runs", async () => {
  const { execFileSync } = await import("node:child_process");
  const { mkdtempSync, writeFileSync, chmodSync, mkdirSync, existsSync, readFileSync: read } =
    await import("node:fs");
  const { tmpdir } = await import("node:os");

  const dir = mkdtempSync(join(tmpdir(), "pm-publish-"));
  const bin = join(dir, "bin");
  mkdirSync(bin);
  const log = join(dir, "invoked.log");
  for (const tool of ["git", "cargo", "make", "docker", "rclone", "curl"]) {
    const shim = join(bin, tool);
    writeFileSync(shim, `#!/bin/sh\necho ${tool} >> ${log}\nexit 0\n`);
    chmodSync(shim, 0o755);
  }

  let status = 0;
  let stderr = "";
  try {
    execFileSync(process.execPath, [join(ROOT, "scripts/publish.mjs"), "patch", "--dry-run"], {
      cwd: ROOT,
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: `${bin}:${process.env.PATH}`,
        PM_RELEASE_KEY: join(dir, "absent.pem"),
      },
    });
  } catch (error) {
    status = error.status;
    stderr = error.stderr ?? "";
  }

  assert.notEqual(status, 0, "publishing without a key must fail");
  assert.match(stderr, /no release key/);
  const invoked = existsSync(log) ? read(log, "utf8") : "";
  assert.doesNotMatch(invoked, /^make$/m, `make ran before the key check: ${invoked}`);
});

// The image is built from the release's own artifacts, so every platform
// it publishes needs a target that produces one. Adding a platform here
// without a target would fail as a missing COPY source inside the build.
test("every platform the image publishes has a release target that builds it", () => {
  const built = new Set(TARGETS.map((target) => target.platform).filter(Boolean));
  for (const platform of IMAGE_PLATFORMS) {
    assert.ok(built.has(platform), `no release target builds ${platform}`);
  }
});

test("the image takes its binary from the artifact built for that architecture", () => {
  assert.equal(imageBinaryPath("linux/amd64", "/stage"), "/stage/image/amd64/pm");
  assert.equal(imageBinaryPath("linux/arm64", "/stage"), "/stage/image/arm64/pm");
});

test("a platform whose binary was never staged is named before the build", () => {
  assert.deepEqual(unstagedPlatforms(IMAGE_PLATFORMS, () => true), []);
  assert.deepEqual(
    unstagedPlatforms(["linux/amd64", "linux/arm64"], (path) => path.includes("amd64")),
    ["linux/arm64"],
  );
});

// One build produces the manifest list, so both platforms are in one
// invocation. It must also compile nothing: the binaries are already
// built and signed, and a second compile would be emulated.
test("the image build covers both platforms from the prebuilt binaries", () => {
  const args = imageBuildArgs({ version: "1.2.3", gitRev: "abc1234", push: true });
  assert.ok(args.includes("--platform"));
  assert.equal(args[args.indexOf("--platform") + 1], "linux/amd64,linux/arm64");
  assert.equal(args[args.indexOf("--build-arg") + 1], "PM_BINARY=prebuilt");
  assert.ok(args.includes("--push"));
  assert.ok(args.some((arg) => arg.endsWith(":1.2.3")), `no version tag: ${args.join(" ")}`);
  assert.ok(
    !args.some((arg) => arg.includes("latest")),
    "the moving tag is written last, not by the build",
  );
});

test("a dry run builds the image and pushes nothing", () => {
  const args = imageBuildArgs({ version: "1.2.3", gitRev: "abc1234", push: false });
  assert.ok(!args.includes("--push"));
  assert.ok(args.includes("--output=type=cacheonly"));
  assert.equal(args[args.indexOf("--platform") + 1], "linux/amd64,linux/arm64");
});

test("a pushed manifest list is read for the platforms it actually carries", () => {
  const inspected = [
    "Name:      registry.example/puppet-master-worker:1.2.3",
    "MediaType: application/vnd.oci.image.index.v1+json",
    "Manifests:",
    "  Name:        registry.example/puppet-master-worker:1.2.3@sha256:aa",
    "  MediaType:   application/vnd.oci.image.manifest.v1+json",
    "  Platform:    linux/amd64",
    "",
    "  Name:        registry.example/puppet-master-worker:1.2.3@sha256:bb",
    "  Platform:    linux/arm64",
  ].join("\n");
  assert.deepEqual(manifestPlatforms(inspected), ["linux/amd64", "linux/arm64"]);
  assert.deepEqual(manifestPlatforms("Name: x\nMediaType: y"), []);
});

// A builder without QEMU for the foreign architecture lists only its
// own, which is the failure this catches before the release builds.
test("a builder missing a platform is reported rather than attempted", () => {
  const both = "Driver: docker-container\nPlatforms: linux/amd64, linux/arm64, linux/386\n";
  assert.deepEqual(uncoveredPlatforms(both, IMAGE_PLATFORMS), []);
  const native = "Driver: docker\nPlatforms: linux/arm64, linux/arm/v7\n";
  assert.deepEqual(uncoveredPlatforms(native, IMAGE_PLATFORMS), ["linux/amd64"]);
});

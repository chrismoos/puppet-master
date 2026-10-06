import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  archiveCommand,
  archivedBuildNumber,
  commitBuildNumber,
  commitMessage,
  exportOptionsPlist,
  parseProfile,
  pickDistributionProfile,
  resolveProfiles,
  systemPathFirst,
  exportCommand,
  findIpa,
  isBuildNumber,
  nextBuildNumber,
  isMarketingVersion,
  keyPath,
  parseArgs,
  preflight,
  uploadCommand,
  versionCommitMessage,
  writeBuildNumber,
  writeMarketingVersion,
} from "./testflight.mjs";

const ENV = {
  APPLE_TEAM_ID: "ABCDE12345",
  APP_STORE_CONNECT_KEY_ID: "K1234567",
  APP_STORE_CONNECT_ISSUER_ID: "69a6de70-0000-0000-0000-000000000000",
};

const PRESENT = { hasCommand: () => true, hasFile: () => true };

function valueAfter(command, flag) {
  const index = command.indexOf(flag);
  assert.notEqual(index, -1, `${flag} is not in ${command.join(" ")}`);
  return command[index + 1];
}

test("parseArgs defaults to a Release build that uploads", () => {
  assert.deepEqual(parseArgs([]), {
    check: false,
    dryRun: false,
    upload: true,
    fromArchive: false,
    manualSigning: true,
    buildNumber: null,
    configuration: "Release",
    commit: true,
    bumpOnly: false,
    setVersion: null,
  });
});

// Automatic signing consults the portal for profiles that are usually
// already installed, so it is the opt-in rather than the default.
test("--automatic-signing hands profile resolution back to Xcode", () => {
  assert.equal(parseArgs(["--automatic-signing"]).manualSigning, false);
  assert.equal(parseArgs(["--automatic-signing", "--manual-signing"]).manualSigning, true);
});

test("parseArgs reads every option", () => {
  const options = parseArgs(["--check", "--dry-run", "--no-upload", "--build-number", "42", "--configuration", "Debug"]);
  assert.equal(options.manualSigning, true);
  assert.equal(options.check, true);
  assert.equal(options.dryRun, true);
  assert.equal(options.upload, false);
  assert.equal(options.buildNumber, "42");
  assert.equal(options.configuration, "Debug");
});

test("--from-archive drops a build number it has no project to write it to", () => {
  const options = parseArgs(["--from-archive", "--build-number", "359"]);
  assert.equal(options.fromArchive, true);
  assert.equal(options.buildNumber, null);
});

test("parseArgs rejects an unknown option", () => {
  assert.throws(() => parseArgs(["--publish"]), /unknown option --publish/);
});

test("parseArgs rejects a build number CFBundleVersion cannot hold", () => {
  assert.throws(() => parseArgs(["--build-number", "f1f8fb9"]), /not a CFBundleVersion/);
  assert.throws(() => parseArgs(["--build-number"]), /not a CFBundleVersion/);
});

test("isBuildNumber accepts up to three numeric components", () => {
  assert.equal(isBuildNumber("7"), true);
  assert.equal(isBuildNumber("1.2.3"), true);
  assert.equal(isBuildNumber("1.2.3.4"), false);
  assert.equal(isBuildNumber("1.0-beta"), false);
});

test("preflight passes when every input is present", () => {
  assert.deepEqual(preflight(ENV, PRESENT), []);
});

test("preflight reports every missing input in one run", () => {
  const problems = preflight({}, { hasCommand: () => false, hasFile: () => false });
  assert.equal(problems.length, 4);
  assert.match(problems.join("\n"), /git is required/);
  assert.match(problems.join("\n"), /xcodebuild not available/);
  assert.match(problems.join("\n"), /APPLE_TEAM_ID/);
  assert.match(problems.join("\n"), /APP_STORE_CONNECT_KEY_ID/);
});

test("preflight names the key file it could not find", () => {
  const seen = [];
  const problems = preflight(ENV, {
    hasCommand: () => true,
    hasFile: (path) => {
      seen.push(path);
      return false;
    },
  });
  assert.deepEqual(seen, [keyPath(ENV.APP_STORE_CONNECT_KEY_ID)]);
  assert.equal(problems.length, 1);
  assert.match(problems[0], /AuthKey_K1234567\.p8/);
});

test("preflight does not look for a key file when the key id is unset", () => {
  const problems = preflight({ APPLE_TEAM_ID: ENV.APPLE_TEAM_ID }, PRESENT);
  assert.equal(problems.length, 1);
  assert.match(problems[0], /APP_STORE_CONNECT_KEY_ID/);
});

test("the archive lands under the build directory, not the filesystem root", () => {
  const path = valueAfter(archiveCommand(ENV, { configuration: "Release" }), "-archivePath");
  assert.equal(path, "ios/build/PuppetMaster.xcarchive");
  assert.equal(valueAfter(exportCommand(ENV), "-archivePath"), path);
});

test("archiveCommand builds the requested configuration for a device", () => {
  const command = archiveCommand(ENV, { configuration: "Release" });
  assert.equal(valueAfter(command, "-configuration"), "Release");
  assert.equal(valueAfter(command, "-destination"), "generic/platform=iOS");
  assert.equal(valueAfter(command, "-workspace"), "ios/PuppetMaster.xcworkspace");
  assert.ok(command.includes(`DEVELOPMENT_TEAM=${ENV.APPLE_TEAM_ID}`));
});

test("both xcodebuild steps authenticate provisioning updates with the App Store Connect key", () => {
  for (const command of [archiveCommand(ENV, { configuration: "Release" }), exportCommand(ENV)]) {
    assert.ok(command.includes("-allowProvisioningUpdates"));
    assert.equal(valueAfter(command, "-authenticationKeyID"), ENV.APP_STORE_CONNECT_KEY_ID);
    assert.equal(valueAfter(command, "-authenticationKeyIssuerID"), ENV.APP_STORE_CONNECT_ISSUER_ID);
    assert.equal(valueAfter(command, "-authenticationKeyPath"), keyPath(ENV.APP_STORE_CONNECT_KEY_ID));
  }
});

test("exportCommand exports through the checked-in options plist", () => {
  const command = exportCommand(ENV);
  assert.equal(valueAfter(command, "-exportOptionsPlist"), "ios-export-options.plist");
  assert.equal(valueAfter(command, "-exportPath"), "ios/build");
});

// --upload-package additionally demands --apple-id, --bundle-id,
// --bundle-version and --bundle-short-version-string, so an upload built
// from the IPA alone has to use --upload-app.
test("uploadCommand hands altool the IPA and nothing it would reject", () => {
  const command = uploadCommand(ENV, "ios/build/PuppetMaster.ipa");
  assert.ok(command.includes("--upload-app"));
  assert.ok(!command.includes("--upload-package"));
  assert.equal(valueAfter(command, "-f"), "ios/build/PuppetMaster.ipa");
  assert.equal(valueAfter(command, "-t"), "ios");
  assert.equal(valueAfter(command, "--apiKey"), ENV.APP_STORE_CONNECT_KEY_ID);
  assert.equal(valueAfter(command, "--apiIssuer"), ENV.APP_STORE_CONNECT_ISSUER_ID);
});

const ARCHIVE_PLIST = `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>ApplicationProperties</key>
	<dict>
		<key>CFBundleShortVersionString</key>
		<string>0.1.0</string>
		<key>CFBundleVersion</key>
		<string>359</string>
		<key>SigningIdentity</key>
		<string>Apple Development: Someone (XXXXXXXXXX)</string>
	</dict>
	<key>Name</key>
	<string>PuppetMaster</string>
</dict>
</plist>`;

test("archivedBuildNumber reads what the archive is actually shipping", () => {
  assert.equal(archivedBuildNumber(ARCHIVE_PLIST), "359");
});

// The archive's outer dict carries its own CFBundleVersion in some Xcode
// versions, and the app's is the one that reaches App Store Connect.
test("archivedBuildNumber reads the app's version, not an outer one", () => {
  const outerFirst = ARCHIVE_PLIST.replace(
    "\t<key>ApplicationProperties</key>",
    "\t<key>CFBundleVersion</key>\n\t<string>1</string>\n\t<key>ApplicationProperties</key>",
  );
  assert.equal(archivedBuildNumber(outerFirst), "359");
});

test("archivedBuildNumber gives up rather than guessing", () => {
  assert.equal(archivedBuildNumber("<plist><dict></dict></plist>"), null);
});

function profileXml({ name, appId, development }) {
  return `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
\t<key>AppIDName</key>
\t<string>Puppet Master</string>
\t<key>Entitlements</key>
\t<dict>
\t\t<key>com.apple.security.application-groups</key>
\t\t<array>
\t\t\t<string>group.com.example.puppetmaster</string>
\t\t</array>
\t\t<key>application-identifier</key>
\t\t<string>${appId}</string>
\t\t<key>keychain-access-groups</key>
\t\t<array>
\t\t\t<string>ABCDE12345.*</string>
\t\t</array>
\t\t<key>get-task-allow</key>
\t\t<${development ? "true" : "false"}/>
\t</dict>
\t<key>Name</key>
\t<string>${name}</string>
\t<key>TeamName</key>
\t<string>Example Company</string>
</dict>
</plist>`;
}

const APP_DIST = profileXml({ name: "Puppet Master", appId: "ABCDE12345.com.example.puppetmaster", development: false });
const APP_DEV = profileXml({ name: "iOS Team Provisioning Profile: com.example.puppetmaster", appId: "ABCDE12345.com.example.puppetmaster", development: true });
const NSE_DIST = profileXml({ name: "Puppet Master NSE", appId: "ABCDE12345.com.example.puppetmaster.nse", development: false });
const NSE_DEV = profileXml({ name: "iOS Team Provisioning Profile: com.example.puppetmaster.nse", appId: "ABCDE12345.com.example.puppetmaster.nse", development: true });

test("parseProfile reads the name, identifier and whether it is a development profile", () => {
  assert.deepEqual(parseProfile(APP_DIST), {
    name: "Puppet Master",
    appId: "ABCDE12345.com.example.puppetmaster",
    development: false,
  });
  assert.equal(parseProfile(APP_DEV).development, true);
});

// AppIDName and TeamName both end in Name and both precede the profile's
// own name in the file.
test("parseProfile does not mistake a neighbouring key for the name", () => {
  assert.equal(parseProfile(NSE_DIST).name, "Puppet Master NSE");
});

test("parseProfile gives up on something that is not a profile", () => {
  assert.equal(parseProfile("<plist><dict></dict></plist>"), null);
});

test("pickDistributionProfile refuses a development profile for the same id", () => {
  const installed = [parseProfile(APP_DEV), parseProfile(NSE_DEV)];
  assert.equal(pickDistributionProfile(installed, "ABCDE12345.com.example.puppetmaster"), null);
});

test("pickDistributionProfile takes the distribution one when both are installed", () => {
  const installed = [parseProfile(APP_DEV), parseProfile(APP_DIST)];
  assert.equal(pickDistributionProfile(installed, "ABCDE12345.com.example.puppetmaster").name, "Puppet Master");
});

test("pickDistributionProfile matches the identifier exactly", () => {
  const installed = [parseProfile(NSE_DIST)];
  assert.equal(pickDistributionProfile(installed, "ABCDE12345.com.example.puppetmaster"), null);
});

test("resolveProfiles names one profile per identifier", () => {
  const installed = [APP_DEV, APP_DIST, NSE_DEV, NSE_DIST].map(parseProfile);
  assert.deepEqual(resolveProfiles(installed, "ABCDE12345", ["com.example.puppetmaster", "com.example.puppetmaster.nse"]), {
    "com.example.puppetmaster": "Puppet Master",
    "com.example.puppetmaster.nse": "Puppet Master NSE",
  });
});

// The extension is the one that gets overlooked, so the error has to say
// which identifier is short rather than that signing failed.
test("resolveProfiles names the identifier that has no distribution profile", () => {
  const installed = [APP_DIST, NSE_DEV].map(parseProfile);
  assert.throws(
    () => resolveProfiles(installed, "ABCDE12345", ["com.example.puppetmaster", "com.example.puppetmaster.nse"]),
    /no App Store distribution profile installed for com\.example\.puppetmaster\.nse/,
  );
});

test("exportOptionsPlist pins manual signing to the resolved profiles", () => {
  const plist = exportOptionsPlist("ABCDE12345", {
    "com.example.puppetmaster": "Puppet Master",
    "com.example.puppetmaster.nse": "Puppet Master NSE",
  });
  assert.match(plist, /<key>signingStyle<\/key>\s*<string>manual<\/string>/);
  assert.match(plist, /<key>teamID<\/key>\s*<string>ABCDE12345<\/string>/);
  assert.match(plist, /<key>com\.example\.puppetmaster<\/key>\s*<string>Puppet Master<\/string>/);
  assert.match(plist, /<key>com\.example\.puppetmaster\.nse<\/key>\s*<string>Puppet Master NSE<\/string>/);
  assert.match(plist, /<string>app-store-connect<\/string>/);
});

// Manual signing exists to stop the export consulting the portal, so a
// leftover key or provisioning-update flag would defeat the whole mode.
test("a manual export asks the portal for nothing", () => {
  const command = exportCommand(ENV, { optionsPlist: "ios/build/export-options-manual.plist", manual: true });
  assert.ok(!command.includes("-allowProvisioningUpdates"));
  assert.ok(!command.some((arg) => String(arg).startsWith("-authenticationKey")));
  assert.equal(valueAfter(command, "-exportOptionsPlist"), "ios/build/export-options-manual.plist");
});

// xcodebuild packs the IPA with rsync, whose helper is resolved by name.
// A Homebrew rsync answering instead fails the export after every target
// has already been signed.
test("systemPathFirst puts the system directory ahead of a shadowing one", () => {
  assert.equal(
    systemPathFirst({ PATH: "/opt/homebrew/bin:/usr/local/bin:/bin" }).PATH,
    "/usr/bin:/opt/homebrew/bin:/usr/local/bin:/bin",
  );
});

test("systemPathFirst moves the system directory rather than repeating it", () => {
  assert.equal(
    systemPathFirst({ PATH: "/opt/homebrew/bin:/usr/bin:/bin" }).PATH,
    "/usr/bin:/opt/homebrew/bin:/bin",
  );
});

// An empty PATH entry means the current directory, which is not something
// to hand to a child that runs signing tools.
test("systemPathFirst never leaves an empty entry behind", () => {
  assert.equal(systemPathFirst({}).PATH, "/usr/bin");
  assert.equal(systemPathFirst({ PATH: "" }).PATH, "/usr/bin");
  assert.equal(systemPathFirst({ PATH: "/usr/bin" }).PATH, "/usr/bin");
});

test("systemPathFirst keeps the rest of the environment", () => {
  const result = systemPathFirst({ PATH: "/bin", APPLE_TEAM_ID: "ABCDE12345" });
  assert.equal(result.APPLE_TEAM_ID, "ABCDE12345");
});

test("findIpa picks the single exported archive", () => {
  assert.equal(findIpa(["PuppetMaster.ipa", "DistributionSummary.plist", "Packaging.log"]), "PuppetMaster.ipa");
});

test("findIpa refuses to guess", () => {
  assert.throws(() => findIpa(["ExportOptions.plist"]), /produced nothing to upload/);
  assert.throws(() => findIpa(["a.ipa", "b.ipa"]), /2 \.ipa files/);
});

function appJson(buildNumber) {
  const path = join(mkdtempSync(join(tmpdir(), "testflight-")), "app.json");
  writeFileSync(path, `${JSON.stringify({ expo: { name: "Puppet Master", ios: { bundleIdentifier: "com.example.puppetmaster", buildNumber } } }, null, 2)}\n`);
  return path;
}

test("writeBuildNumber records the new number and leaves the rest of the config alone", () => {
  const path = appJson("1");
  assert.deepEqual(writeBuildNumber(path, "884"), { previous: "1", changed: true });
  const config = JSON.parse(readFileSync(path, "utf8"));
  assert.equal(config.expo.ios.buildNumber, "884");
  assert.equal(config.expo.ios.bundleIdentifier, "com.example.puppetmaster");
  assert.equal(config.expo.name, "Puppet Master");
});

// An unconditional write would leave app.json newer than the generated
// project and force a prebuild and a pod install on every run.
test("writeBuildNumber does not touch app.json when the number is unchanged", () => {
  const path = appJson("884");
  const before = readFileSync(path, "utf8");
  assert.deepEqual(writeBuildNumber(path, "884"), { previous: "884", changed: false });
  assert.equal(readFileSync(path, "utf8"), before);
});

test("parseArgs recognizes --no-commit, --commit, --bump-only, and --bump", () => {
  assert.equal(parseArgs(["--no-commit"]).commit, false);
  assert.equal(parseArgs(["--no-commit", "--commit"]).commit, true);
  assert.equal(parseArgs(["--bump-only"]).bumpOnly, true);
  assert.equal(parseArgs(["--bump"]).bumpOnly, true);
});

test("commitMessage states the build number and ends with a period", () => {
  assert.equal(commitMessage("422"), "bump ios build to 422.");
});

test("commitBuildNumber does not commit on dryRun", () => {
  const path = appJson("422");
  const result = commitBuildNumber(path, "422", { dryRun: true });
  assert.deepEqual(result, { committed: false, reason: "dry-run" });
});

test("commitBuildNumber skips commit when app.json is clean", () => {
  const dir = mkdtempSync(join(tmpdir(), "testflight-git-"));
  spawnSync("git", ["init"], { cwd: dir });
  spawnSync("git", ["config", "user.name", "Test"], { cwd: dir });
  spawnSync("git", ["config", "user.email", "test@example.com"], { cwd: dir });
  const appPath = join(dir, "app.json");
  writeFileSync(appPath, JSON.stringify({ expo: { ios: { buildNumber: "422" } } }));
  spawnSync("git", ["add", "app.json"], { cwd: dir });
  spawnSync("git", ["commit", "-m", "initial."], { cwd: dir });
  const result = commitBuildNumber("app.json", "422", { cwd: dir });
  assert.deepEqual(result, { committed: false, reason: "clean" });
});

test("commitBuildNumber commits modified app.json and leaves other changes alone", () => {
  const dir = mkdtempSync(join(tmpdir(), "testflight-git-"));
  spawnSync("git", ["init"], { cwd: dir });
  spawnSync("git", ["config", "user.name", "Test"], { cwd: dir });
  spawnSync("git", ["config", "user.email", "test@example.com"], { cwd: dir });
  const appPath = join(dir, "app.json");
  writeFileSync(appPath, JSON.stringify({ expo: { ios: { buildNumber: "421" } } }));
  const otherPath = join(dir, "other.txt");
  writeFileSync(otherPath, "keep me");
  spawnSync("git", ["add", "."], { cwd: dir });
  spawnSync("git", ["commit", "-m", "initial."], { cwd: dir });

  writeFileSync(appPath, JSON.stringify({ expo: { ios: { buildNumber: "422" } } }));
  writeFileSync(otherPath, "modified other");

  const result = commitBuildNumber("app.json", "422", { cwd: dir });
  assert.deepEqual(result, { committed: true });

  const log = spawnSync("git", ["log", "-1", "--pretty=%s"], { cwd: dir, encoding: "utf8" });
  assert.equal(log.stdout.trim(), "bump ios build to 422.");

  const status = spawnSync("git", ["status", "--porcelain"], { cwd: dir, encoding: "utf8" });
  assert.equal(status.stdout.trim(), "M other.txt");
});

test("isMarketingVersion accepts one to three numeric components", () => {
  for (const good of ["1", "1.0", "1.0.0", "2.14.3"]) assert.equal(isMarketingVersion(good), true, good);
  for (const bad of ["", "v1.0.0", "1.0.0.0", "1.0-beta", "1..0", undefined]) {
    assert.equal(isMarketingVersion(bad), false, String(bad));
  }
});

test("parseArgs rejects a version CFBundleShortVersionString cannot hold", () => {
  assert.equal(parseArgs(["--set-version", "1.1.0"]).setVersion, "1.1.0");
  assert.throws(() => parseArgs(["--set-version", "1.1.0-rc1"]), /CFBundleShortVersionString/);
  assert.throws(() => parseArgs(["--set-version"]), /CFBundleShortVersionString/);
});

test("writeMarketingVersion records the version and leaves the build number alone", () => {
  const path = appJson("884");
  assert.deepEqual(writeMarketingVersion(path, "1.1.0"), { previous: undefined, changed: true });
  const config = JSON.parse(readFileSync(path, "utf8"));
  assert.equal(config.expo.version, "1.1.0");
  assert.equal(config.expo.ios.buildNumber, "884");
  assert.deepEqual(writeMarketingVersion(path, "1.1.0"), { previous: "1.1.0", changed: false });
});

test("versionCommitMessage states the version and ends with a period", () => {
  assert.equal(versionCommitMessage("1.1.0"), "set ios version to 1.1.0.");
});

// The script runs as a child process, so its own account of what it did
// reaches the test only through the captured streams.
function ranIn(dir, proc) {
  return [
    "",
    `  dir: ${dir}`,
    `  node: ${process.version}`,
    `  exit: ${proc.status}`,
    `  stdout: ${proc.stdout?.trim() || "(empty)"}`,
    `  stderr: ${proc.stderr?.trim() || "(empty)"}`,
  ].join("\n");
}

test("main with --bump-only updates app.json and commits the bump", () => {
  const dir = mkdtempSync(join(tmpdir(), "testflight-bump-"));
  spawnSync("git", ["init"], { cwd: dir });
  spawnSync("git", ["config", "user.name", "Test"], { cwd: dir });
  spawnSync("git", ["config", "user.email", "test@example.com"], { cwd: dir });
  const appPath = join(dir, "app.json");
  writeFileSync(
    appPath,
    `${JSON.stringify({ expo: { name: "Puppet Master", ios: { bundleIdentifier: "com.example.puppetmaster", buildNumber: "400" } } }, null, 2)}\n`,
  );
  spawnSync("git", ["add", "app.json"], { cwd: dir });
  spawnSync("git", ["commit", "-m", "initial."], { cwd: dir });

  const scriptsDir = join(dir, "scripts");
  mkdirSync(scriptsDir);
  copyFileSync(join(import.meta.dirname, "testflight.mjs"), join(scriptsDir, "testflight.mjs"));

  const proc = spawnSync("node", [join(scriptsDir, "testflight.mjs"), "--bump-only"], {
    cwd: dir,
    encoding: "utf8",
  });
  assert.equal(proc.status, 0, `the script failed${ranIn(dir, proc)}`);

  const updatedConfig = JSON.parse(readFileSync(appPath, "utf8"));
  assert.equal(
    updatedConfig.expo.ios.buildNumber,
    "401",
    `build number is ${updatedConfig.expo.ios.buildNumber}, expected 401${ranIn(dir, proc)}`,
  );

  const log = spawnSync("git", ["log", "-1", "--pretty=%s"], { cwd: dir, encoding: "utf8" });
  assert.equal(
    log.stdout.trim(),
    "bump ios build to 401.",
    `last commit is "${log.stdout.trim()}"${ranIn(dir, proc)}`,
  );
});

test("main with --set-version updates the version, commits it, and keeps the build number", () => {
  const dir = mkdtempSync(join(tmpdir(), "testflight-version-"));
  spawnSync("git", ["init"], { cwd: dir });
  spawnSync("git", ["config", "user.name", "Test"], { cwd: dir });
  spawnSync("git", ["config", "user.email", "test@example.com"], { cwd: dir });
  const appPath = join(dir, "app.json");
  writeFileSync(
    appPath,
    `${JSON.stringify({ expo: { version: "1.0.0", ios: { buildNumber: "400" } } }, null, 2)}\n`,
  );
  spawnSync("git", ["add", "app.json"], { cwd: dir });
  spawnSync("git", ["commit", "-m", "initial."], { cwd: dir });

  const scriptsDir = join(dir, "scripts");
  mkdirSync(scriptsDir);
  copyFileSync(join(import.meta.dirname, "testflight.mjs"), join(scriptsDir, "testflight.mjs"));

  const proc = spawnSync("node", [join(scriptsDir, "testflight.mjs"), "--set-version", "1.1.0"], {
    cwd: dir,
    encoding: "utf8",
  });
  assert.equal(proc.status, 0, `the script failed${ranIn(dir, proc)}`);

  const config = JSON.parse(readFileSync(appPath, "utf8"));
  assert.equal(config.expo.version, "1.1.0", ranIn(dir, proc));
  assert.equal(config.expo.ios.buildNumber, "400", ranIn(dir, proc));

  const log = spawnSync("git", ["log", "-1", "--pretty=%s"], { cwd: dir, encoding: "utf8" });
  assert.equal(log.stdout.trim(), "set ios version to 1.1.0.", ranIn(dir, proc));
});

// App Review rejects a declared background mode or permission string the
// app never exercises, and iPad support cannot be withdrawn once shipped.
test("app.json declares only the device family, background modes and permissions the app uses", () => {
  const { expo } = JSON.parse(readFileSync(join(import.meta.dirname, "..", "app.json"), "utf8"));
  assert.equal(expo.ios.supportsTablet, false);
  assert.deepEqual(expo.ios.infoPlist.UIBackgroundModes, ["remote-notification"]);
  const secureStore = expo.plugins.find((plugin) => Array.isArray(plugin) && plugin[0] === "expo-secure-store");
  assert.deepEqual(secureStore?.[1], { faceIDPermission: false });
  assert.equal(isMarketingVersion(expo.version), true);
});

// macOS reaches every temp directory through /var, a link to /private/var,
// so a guard that compares unresolved paths never runs main() there.
test("main runs when the script is reached through a symlinked path", () => {
  const dir = mkdtempSync(join(tmpdir(), "testflight-link-"));
  const real = join(dir, "real");
  mkdirSync(join(real, "scripts"), { recursive: true });
  writeFileSync(
    join(real, "app.json"),
    `${JSON.stringify({ expo: { ios: { buildNumber: "400" } } }, null, 2)}\n`,
  );
  copyFileSync(join(import.meta.dirname, "testflight.mjs"), join(real, "scripts", "testflight.mjs"));

  const link = join(dir, "link");
  symlinkSync(real, link);

  const proc = spawnSync(
    "node",
    [join(link, "scripts", "testflight.mjs"), "--bump-only", "--no-commit", "--build-number", "500"],
    { cwd: real, encoding: "utf8" },
  );
  assert.equal(proc.status, 0, `the script failed${ranIn(link, proc)}`);

  const config = JSON.parse(readFileSync(join(real, "app.json"), "utf8"));
  assert.equal(
    config.expo.ios.buildNumber,
    "500",
    `build number is ${config.expo.ios.buildNumber}, expected 500${ranIn(link, proc)}`,
  );
});

test("nextBuildNumber increments the saved value and preserves dotted versions", () => {
  for (const [previous, expected] of [["43", "44"], ["1.2.9", "1.2.10"], ["9007199254740992", "9007199254740993"]]) {
    assert.equal(nextBuildNumber(appJson(previous)), expected);
  }
});

test("nextBuildNumber rejects missing or invalid saved values", () => {
  for (const previous of [undefined, "", "abc", "1.2.3.4", 43]) {
    assert.throws(() => nextBuildNumber(appJson(previous)), /not a CFBundleVersion/);
  }
});

test("preflight rejects UI-test configuration before a TestFlight build", () => {
  const problems = preflight({ ...ENV, PM_UI_TEST_BUILD: "1" }, PRESENT);
  assert.equal(problems.length, 1);
  assert.match(problems[0], /unset PM_UI_TEST_BUILD/);
  assert.deepEqual(preflight({ ...ENV, PM_UI_TEST_BUILD: "0" }, PRESENT), []);
});

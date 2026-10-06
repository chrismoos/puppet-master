// Archives the iOS app, exports a signed IPA, and uploads it to TestFlight.
//
// Everything runs on this machine: no source and no signing identity
// leaves it. Xcode resolves the certificate and profile against the
// login keychain and the App Store Connect key on disk, so nothing
// signing-related is kept in the generated ios/ tree that prebuild
// discards.
//
//   node scripts/testflight.mjs                 archive, export, upload
//   node scripts/testflight.mjs --check         report what is missing, run nothing
//   node scripts/testflight.mjs --dry-run       print the commands instead
//   node scripts/testflight.mjs --no-upload     stop after the exported IPA
//   node scripts/testflight.mjs --from-archive  re-export the archive already built
//   node scripts/testflight.mjs --automatic-signing  let Xcode resolve the profiles
//   node scripts/testflight.mjs --set-version 1.1.0  set the App Store version and commit it
//
// The export signs with the distribution profiles already installed.
// --automatic-signing hands that back to Xcode, which resolves profiles
// against the portal and needs an App Store Connect key that may create
// them.
//
// `make ios-testflight` from the repository root is the way in.
import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SCHEME = "PuppetMaster";
const EXPORT_OPTIONS = "ios-export-options.plist";
const DERIVED = join("ios", "build");
const WORKSPACE = join("ios", `${SCHEME}.xcworkspace`);
const ARCHIVE = join(DERIVED, `${SCHEME}.xcarchive`);
const KEY_DIR = join(homedir(), ".appstoreconnect", "private_keys");
const MANUAL_OPTIONS = join(DERIVED, "export-options-manual.plist");
// Xcode 16 moved the directory; an older install still uses the other.
const PROFILE_DIRS = [
  join(homedir(), "Library", "Developer", "Xcode", "UserData", "Provisioning Profiles"),
  join(homedir(), "Library", "MobileDevice", "Provisioning Profiles"),
];
// Mirrors the suffix plugins/push-crypto/index.js appends during prebuild.
const NSE_SUFFIX = ".nse";

export function parseArgs(argv) {
  const options = {
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
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--check") options.check = true;
    else if (arg === "--dry-run") options.dryRun = true;
    else if (arg === "--no-upload") options.upload = false;
    else if (arg === "--from-archive") options.fromArchive = true;
    else if (arg === "--manual-signing") options.manualSigning = true;
    else if (arg === "--automatic-signing") options.manualSigning = false;
    else if (arg === "--no-commit") options.commit = false;
    else if (arg === "--commit") options.commit = true;
    else if (arg === "--bump-only" || arg === "--bump") options.bumpOnly = true;
    else if (arg === "--build-number") options.buildNumber = argv[++i];
    else if (arg === "--set-version") options.setVersion = argv[++i] ?? "";
    else if (arg === "--configuration") options.configuration = argv[++i];
    else throw new Error(`unknown option ${arg}`);
  }
  if (options.buildNumber !== null && !isBuildNumber(options.buildNumber)) {
    throw new Error(`--build-number ${options.buildNumber ?? "(none)"} is not a CFBundleVersion`);
  }
  if (options.setVersion !== null && !isMarketingVersion(options.setVersion)) {
    throw new Error(`--set-version ${options.setVersion || "(none)"} is not a CFBundleShortVersionString`);
  }
  if (options.fromArchive) options.buildNumber = null;
  return options;
}

export function isBuildNumber(value) {
  return /^\d+(\.\d+){0,2}$/.test(value ?? "");
}

// Reports every missing input at once, each named with what supplies it.
// A developer who is missing three of these should learn that in one run
// rather than in three.
export function preflight(env, { hasCommand, hasFile }) {
  const problems = [];
  if (env.PM_UI_TEST_BUILD === "1") {
    problems.push("TestFlight cannot use a UI-test build -> unset PM_UI_TEST_BUILD");
  }
  const keyId = env.APP_STORE_CONNECT_KEY_ID;
  if (!hasCommand("git")) {
    problems.push("git is required to commit the build number");
  }
  if (!hasCommand("xcodebuild")) {
    problems.push("xcodebuild not available -> install Xcode and run xcode-select --install");
  }
  if (!env.APPLE_TEAM_ID) {
    problems.push("no team id -> export APPLE_TEAM_ID=<10-char id from your Apple developer membership>");
  }
  if (!keyId || !env.APP_STORE_CONNECT_ISSUER_ID) {
    problems.push("no App Store Connect key -> export APP_STORE_CONNECT_KEY_ID and APP_STORE_CONNECT_ISSUER_ID");
  } else if (!hasFile(keyPath(keyId))) {
    problems.push(`key file missing -> put AuthKey_${keyId}.p8 in ${KEY_DIR}/`);
  }
  return problems;
}

export { systemPathFirst };

export function keyPath(keyId) {
  return join(KEY_DIR, `AuthKey_${keyId}.p8`);
}

// -allowProvisioningUpdates registers devices and renews profiles, which
// needs an authenticated account. Passing the App Store Connect key that
// the upload already requires keeps the whole run non-interactive
// instead of falling back to whichever Apple ID Xcode has signed in.
function authFlags(env) {
  return [
    "-allowProvisioningUpdates",
    "-authenticationKeyPath", keyPath(env.APP_STORE_CONNECT_KEY_ID),
    "-authenticationKeyID", env.APP_STORE_CONNECT_KEY_ID,
    "-authenticationKeyIssuerID", env.APP_STORE_CONNECT_ISSUER_ID,
  ];
}

export function archiveCommand(env, { configuration }) {
  return [
    "xcodebuild", "archive",
    "-workspace", WORKSPACE,
    "-scheme", SCHEME,
    "-configuration", configuration,
    "-archivePath", ARCHIVE,
    "-destination", "generic/platform=iOS",
    "-derivedDataPath", DERIVED,
    `DEVELOPMENT_TEAM=${env.APPLE_TEAM_ID}`,
    ...authFlags(env),
  ];
}

// Manual signing resolves everything from what is already on disk, so it
// passes no key and asks for no provisioning update. That is the point of
// it: automatic signing consults the portal even when the profiles it
// would be given are already installed.
export function exportCommand(env, { optionsPlist = EXPORT_OPTIONS, manual = false } = {}) {
  return [
    "xcodebuild", "-exportArchive",
    "-archivePath", ARCHIVE,
    "-exportOptionsPlist", optionsPlist,
    "-exportPath", DERIVED,
    ...(manual ? [] : authFlags(env)),
  ];
}

export function parseProfile(xml) {
  const name = /<key>Name<\/key>\s*<string>([^<]*)<\/string>/.exec(xml);
  const appId = /<key>application-identifier<\/key>\s*<string>([^<]*)<\/string>/.exec(xml);
  if (!name || !appId) return null;
  return {
    name: name[1],
    appId: appId[1],
    development: /<key>get-task-allow<\/key>\s*<true\/>/.test(xml),
  };
}

// Only an exact match counts. A wildcard profile cannot carry the app
// groups and push entitlements these targets claim, so one that appears
// to cover the id would fail at signing instead of here.
export function pickDistributionProfile(profiles, appId) {
  return profiles.find((p) => p && p.appId === appId && !p.development) ?? null;
}

export function exportOptionsPlist(teamId, profiles) {
  const entries = Object.entries(profiles)
    .map(([bundleId, name]) => `    <key>${bundleId}</key>\n    <string>${name}</string>`)
    .join("\n");
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key>
  <string>app-store-connect</string>
  <key>signingStyle</key>
  <string>manual</string>
  <key>teamID</key>
  <string>${teamId}</string>
  <key>signingCertificate</key>
  <string>Apple Distribution</string>
  <key>provisioningProfiles</key>
  <dict>
${entries}
  </dict>
  <key>uploadSymbols</key>
  <true/>
  <key>destination</key>
  <string>export</string>
</dict>
</plist>
`;
}

// Names what is missing per identifier rather than reporting that signing
// failed, because the two targets fail independently and the extension is
// the one that gets overlooked.
export function resolveProfiles(installed, teamId, bundleIds) {
  const chosen = {};
  const missing = [];
  for (const bundleId of bundleIds) {
    const profile = pickDistributionProfile(installed, `${teamId}.${bundleId}`);
    if (profile) chosen[bundleId] = profile.name;
    else missing.push(bundleId);
  }
  if (missing.length > 0) {
    throw new Error(
      `no App Store distribution profile installed for ${missing.join(" and ")}` +
        " -> create one per identifier in the developer portal and install it",
    );
  }
  return chosen;
}

// --upload-app takes the IPA and reads its own metadata. --upload-package
// does not: it additionally demands --apple-id, --bundle-id,
// --bundle-version and --bundle-short-version-string, and fails without
// them.
export function uploadCommand(env, ipa) {
  return [
    "xcrun", "altool", "--upload-app",
    "-f", ipa,
    "-t", "ios",
    "--apiKey", env.APP_STORE_CONNECT_KEY_ID,
    "--apiIssuer", env.APP_STORE_CONNECT_ISSUER_ID,
  ];
}

export function nextBuildNumber(appJsonPath) {
  const config = JSON.parse(readFileSync(appJsonPath, "utf8"));
  const previous = config.expo?.ios?.buildNumber;
  if (typeof previous !== "string" || !isBuildNumber(previous)) {
    throw new Error(`app.json ios.buildNumber ${previous ?? "(unset)"} is not a CFBundleVersion`);
  }
  const parts = previous.split(".");
  const last = parts.length - 1;
  parts[last] = String(BigInt(parts[last]) + 1n);
  return parts.join(".");
}

// Writing the same value back would leave app.json newer than the
// generated project and force a prebuild and a pod install on every run.
export function writeBuildNumber(appJsonPath, value) {
  const config = JSON.parse(readFileSync(appJsonPath, "utf8"));
  const previous = config.expo.ios.buildNumber;
  if (previous === value) return { previous, changed: false };
  config.expo.ios.buildNumber = value;
  writeFileSync(appJsonPath, `${JSON.stringify(config, null, 2)}\n`);
  return { previous, changed: true };
}

// App Store Connect compares this numerically and refuses anything but
// one to three integers.
export function isMarketingVersion(value) {
  return /^\d+(\.\d+){0,2}$/.test(value ?? "");
}

export function writeMarketingVersion(appJsonPath, value) {
  const config = JSON.parse(readFileSync(appJsonPath, "utf8"));
  const previous = config.expo.version;
  if (previous === value) return { previous, changed: false };
  config.expo.version = value;
  writeFileSync(appJsonPath, `${JSON.stringify(config, null, 2)}\n`);
  return { previous, changed: true };
}

export function versionCommitMessage(version) {
  return `set ios version to ${version}.`;
}

export function commitMessage(buildNumber) {
  return `bump ios build to ${buildNumber}.`;
}

export function commitBuildNumber(appJsonPath, buildNumber, { cwd, dryRun = false, env, message = commitMessage(buildNumber) } = {}) {
  const file = relative(cwd ?? process.cwd(), resolve(cwd ?? process.cwd(), appJsonPath));
  if (dryRun) {
    console.log(`testflight: would commit ${file} for build ${buildNumber}`);
    return { committed: false, reason: "dry-run" };
  }
  const statusResult = spawnSync("git", ["status", "--porcelain", file], { cwd, encoding: "utf8" });
  if (statusResult.status !== 0) {
    throw new Error(`git status failed: ${statusResult.stderr}`);
  }
  if (!statusResult.stdout.trim()) {
    console.log(`testflight: ${file} is clean, skipping commit`);
    return { committed: false, reason: "clean" };
  }
  run(["git", "commit", "-m", message, file], { cwd, dryRun: false, env });
  console.log(`testflight: committed build ${buildNumber} in ${file}`);
  return { committed: true };
}

// The archive's own Info.plist is the only record of what a re-export is
// shipping, since nothing is regenerated from app.json in that mode.
export function archivedBuildNumber(plist) {
  const properties = plist.slice(plist.indexOf("ApplicationProperties"));
  const match = /<key>CFBundleVersion<\/key>\s*<string>([^<]*)<\/string>/.exec(properties);
  return match ? match[1] : null;
}

export function findIpa(entries) {
  const ipas = entries.filter((name) => name.endsWith(".ipa"));
  if (ipas.length === 1) return ipas[0];
  if (ipas.length === 0) {
    throw new Error(`no .ipa in ${DERIVED} -> the export step produced nothing to upload`);
  }
  throw new Error(`${ipas.length} .ipa files in ${DERIVED} -> remove the stale ones and run again`);
}

// xcodebuild packs the IPA with /usr/bin/rsync, which spawns its helper
// process by name rather than by path. A Homebrew rsync earlier on PATH
// answers instead and rejects the -E it is passed, so the export fails
// on "Copy failed" after every target has already been signed. Putting
// the system directory first for the child only affects what xcodebuild
// resolves, not the caller's shell.
function systemPathFirst(env) {
  const path = env.PATH ?? "";
  // An empty entry means the current directory, so it is dropped rather
  // than carried into a child that runs signing tools.
  const parts = path.split(":").filter((entry) => entry !== "/usr/bin" && entry !== "");
  return { ...env, PATH: ["/usr/bin", ...parts].join(":") };
}

function run(command, { cwd, dryRun, env }) {
  console.log(`$ ${command.join(" ")}`);
  if (dryRun) return;
  const [bin, ...args] = command;
  const result = spawnSync(bin, args, { cwd, stdio: "inherit", env });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${bin} exited ${result.status ?? result.signal}`);
  }
}

// The profiles are CMS-signed, so the plist inside comes back through
// the security tool rather than being read directly.
function installedProfiles() {
  const profiles = [];
  for (const dir of PROFILE_DIRS) {
    if (!existsSync(dir)) continue;
    for (const entry of readdirSync(dir)) {
      if (!entry.endsWith(".mobileprovision")) continue;
      const decoded = spawnSync("security", ["cms", "-D", "-i", join(dir, entry)], { encoding: "utf8" });
      if (decoded.status !== 0) continue;
      profiles.push(parseProfile(decoded.stdout));
    }
  }
  return profiles;
}

function main(argv, env) {
  const options = parseArgs(argv);
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

  if (options.setVersion !== null) {
    const appJson = join(root, "app.json");
    const { previous, changed } = writeMarketingVersion(appJson, options.setVersion);
    const would = options.dryRun ? "would be " : "";
    console.log(`ios version ${previous ?? "(unset)"} ${would}-> ${options.setVersion}`);
    if (options.dryRun && changed) {
      writeMarketingVersion(appJson, previous);
    }
    if (options.commit) {
      commitBuildNumber(appJson, options.setVersion, {
        cwd: root,
        dryRun: options.dryRun,
        env,
        message: versionCommitMessage(options.setVersion),
      });
    }
    return;
  }

  if (options.bumpOnly) {
    const buildNumber = options.buildNumber ?? nextBuildNumber(join(root, "app.json"));
    if (!isBuildNumber(buildNumber)) {
      throw new Error(`${buildNumber} is not a CFBundleVersion`);
    }
    const { previous, changed } = writeBuildNumber(join(root, "app.json"), buildNumber);
    const would = options.dryRun ? "would be " : "";
    console.log(`ios build number ${previous ?? "(unset)"} ${would}-> ${buildNumber}`);
    if (options.dryRun && changed) {
      writeBuildNumber(join(root, "app.json"), previous);
    }
    if (options.commit) {
      commitBuildNumber(join(root, "app.json"), buildNumber, { cwd: root, dryRun: options.dryRun, env });
    }
    return;
  }

  const problems = preflight(env, {
    hasCommand: (bin) => spawnSync("command", ["-v", bin], { shell: true }).status === 0,
    hasFile: existsSync,
  });
  if (problems.length > 0) {
    for (const problem of problems) console.error(problem);
    process.exit(1);
  }
  if (options.check) {
    console.log("testflight: every build input is present");
    return;
  }

  // Export re-signs the archive rather than reading its signature, so a
  // run that reached an archive and then failed to sign needs the export
  // repeated and nothing else once the signing assets exist.
  let buildNumber;
  if (options.fromArchive) {
    const plist = join(root, ARCHIVE, "Info.plist");
    if (!existsSync(plist)) {
      throw new Error(`no archive at ${ARCHIVE} -> run without --from-archive to build one`);
    }
    buildNumber = archivedBuildNumber(readFileSync(plist, "utf8")) ?? "(unknown)";
    console.log(`re-exporting the archive already built, ios build number ${buildNumber}`);
  } else {
    buildNumber = options.buildNumber ?? nextBuildNumber(join(root, "app.json"));
    if (!isBuildNumber(buildNumber)) {
      throw new Error(`${buildNumber} is not a CFBundleVersion`);
    }

    // The build number has to reach app.json before prebuild, which is
    // what generates the Info.plist that carries it. Writing it afterwards
    // uploads the previous run's number, and App Store Connect rejects a
    // build number it has already seen.
    const { previous, changed } = writeBuildNumber(join(root, "app.json"), buildNumber);
    const would = options.dryRun ? "would be " : "";
    console.log(`ios build number ${previous ?? "(unset)"} ${would}-> ${buildNumber}`);
    if (options.dryRun && changed) {
      writeBuildNumber(join(root, "app.json"), previous);
    }

    run([env.MAKE ?? "make", "prebuild", "sync-nse"], { cwd: root, dryRun: options.dryRun, env });
    run(archiveCommand(env, options), { cwd: root, dryRun: options.dryRun, env: systemPathFirst(env) });
  }

  let exportOptions = {};
  if (options.manualSigning) {
    const bundleId = JSON.parse(readFileSync(join(root, "app.json"), "utf8")).expo.ios.bundleIdentifier;
    const installed = installedProfiles();
    const chosen = resolveProfiles(installed, env.APPLE_TEAM_ID, [bundleId, bundleId + NSE_SUFFIX]);
    for (const [id, name] of Object.entries(chosen)) console.log(`signing ${id} with "${name}"`);
    writeFileSync(join(root, MANUAL_OPTIONS), exportOptionsPlist(env.APPLE_TEAM_ID, chosen));
    exportOptions = { optionsPlist: MANUAL_OPTIONS, manual: true };
  }
  run(exportCommand(env, exportOptions), { cwd: root, dryRun: options.dryRun, env: systemPathFirst(env) });

  if (!options.upload) {
    console.log(`testflight: exported to ${DERIVED}, upload skipped`);
    if (options.commit && !options.fromArchive) {
      commitBuildNumber(join(root, "app.json"), buildNumber, { cwd: root, dryRun: options.dryRun, env });
    }
    return;
  }

  const exported = options.dryRun ? [`${SCHEME}.ipa`] : readdirSync(join(root, DERIVED));
  const ipa = join(DERIVED, findIpa(exported));
  run(uploadCommand(env, ipa), { cwd: root, dryRun: options.dryRun, env });
  console.log(
    options.dryRun
      ? `testflight: nothing ran, build ${buildNumber} was not uploaded`
      : `testflight: uploaded build ${buildNumber}, it appears in TestFlight once processing finishes`,
  );
  if (options.commit && !options.fromArchive) {
    commitBuildNumber(join(root, "app.json"), buildNumber, { cwd: root, dryRun: options.dryRun, env });
  }
}

// `import.meta.url` has its symlinks resolved and `process.argv[1]` does
// not, so comparing them raw skips main() whenever the invoking path
// crosses a link. macOS puts every temp directory behind one: TMPDIR is
// under /var, which is a link to /private/var.
function invokedDirectly() {
  const entry = process.argv[1];
  if (!entry) return false;
  try {
    return realpathSync(entry) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (invokedDirectly()) {
  try {
    main(process.argv.slice(2), process.env);
  } catch (error) {
    console.error(`testflight: ${error.message}`);
    process.exit(1);
  }
}

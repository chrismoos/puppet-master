// Expo config plugin: adds the Notification Service Extension target
// and App Group entitlements for HPKE-sealed push decryption.
//
// This replaces what would otherwise be manual Xcode configuration
// that prebuild would destroy on every regeneration.

const {
  withXcodeProject,
  withEntitlementsPlist,
  withInfoPlist,
  IOSConfig,
} = require("expo/config-plugins");
const fs = require("fs");
const path = require("path");

const APP_GROUP_ID = "group.com.tech9.puppetmaster";
const NSE_TARGET_NAME = "PuppetMasterNSE";
const NSE_BUNDLE_ID_SUFFIX = ".nse";

/** Entry point: chains all the mods together. */
function withPushCrypto(config) {
  // 1. Add App Group to the main app entitlements.
  config = withAppGroupEntitlement(config);
  // 2. Add the NSE target to the Xcode project.
  config = withNSETarget(config);
  // PushCryptoModule is now compiled as a separate pod via Expo
  // autolinking (expo-module.config.json + podspec). Copying it into the
  // app target would cause duplicate symbols.
  return config;
}

// ── App Group entitlement on the main app ──────────────────────────

function withAppGroupEntitlement(config) {
  return withEntitlementsPlist(config, (mod) => {
    const groups = mod.modResults["com.apple.security.application-groups"] || [];
    if (!groups.includes(APP_GROUP_ID)) {
      groups.push(APP_GROUP_ID);
    }
    mod.modResults["com.apple.security.application-groups"] = groups;
    return mod;
  });
}

// ── NSE target ─────────────────────────────────────────────────────

function withNSETarget(config) {
  return withXcodeProject(config, async (mod) => {
    const projectRoot = mod.modRequest.projectRoot;
    const projectName = mod.modRequest.projectName || "PuppetMaster";
    const project = mod.modResults;
    const bundleId =
      config.ios?.bundleIdentifier + NSE_BUNDLE_ID_SUFFIX;

    // Create the NSE directory and copy sources.
    const nseDir = path.join(projectRoot, "ios", NSE_TARGET_NAME);
    fs.mkdirSync(nseDir, { recursive: true });

    // Copy the NSE Swift source.
    const nseSrc = path.join(__dirname, "ios", "NotificationService.swift");
    const nseDst = path.join(nseDir, "NotificationService.swift");
    fs.copyFileSync(nseSrc, nseDst);

    // Write the NSE Info.plist.
    const infoPlist = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>${NSE_TARGET_NAME}</string>
    <key>CFBundleExecutable</key>
    <string>$(EXECUTABLE_NAME)</string>
    <key>CFBundleIdentifier</key>
    <string>$(PRODUCT_BUNDLE_IDENTIFIER)</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>$(PRODUCT_NAME)</string>
    <key>CFBundlePackageType</key>
    <string>$(PRODUCT_BUNDLE_PACKAGE_TYPE)</string>
    <key>CFBundleShortVersionString</key>
    <string>$(MARKETING_VERSION)</string>
    <key>CFBundleVersion</key>
    <string>$(CURRENT_PROJECT_VERSION)</string>
    <key>NSExtension</key>
    <dict>
        <key>NSExtensionPointIdentifier</key>
        <string>com.apple.usernotifications.service</string>
        <key>NSExtensionPrincipalClass</key>
        <string>$(PRODUCT_MODULE_NAME).NotificationService</string>
    </dict>
</dict>
</plist>`;
    fs.writeFileSync(path.join(nseDir, "Info.plist"), infoPlist);

    // Write the NSE entitlements.
    const entitlements = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.application-groups</key>
    <array>
        <string>${APP_GROUP_ID}</string>
    </array>
    <key>aps-environment</key>
    <string>production</string>
    <key>keychain-access-groups</key>
    <array>
        <string>$(AppIdentifierPrefix)${APP_GROUP_ID}</string>
    </array>
</dict>
</plist>`;
    const entitlementsPath = path.join(nseDir, `${NSE_TARGET_NAME}.entitlements`);
    fs.writeFileSync(entitlementsPath, entitlements);

    // Add the NSE target to the Xcode project using pbxproj APIs.
    addNSETargetToProject(project, {
      projectName,
      targetName: NSE_TARGET_NAME,
      bundleId,
      nseDir,
      entitlementsPath: `${NSE_TARGET_NAME}/${NSE_TARGET_NAME}.entitlements`,
    });

    mod.modResults = project;
    return mod;
  });
}

function addNSETargetToProject(project, opts) {
  const { targetName, bundleId, entitlementsPath } = opts;

  // Check if target already exists (idempotent).
  const existingTarget = project.pbxTargetByName(targetName);
  if (existingTarget) return;

  // Add the NSE target.
  const target = project.addTarget(
    targetName,
    "app_extension",
    targetName,
    bundleId,
  );

  // Name the source by its path under the target's directory. A bare
  // filename is stored with sourceTree "<group>" against the root group,
  // which resolves to ios/NotificationService.swift — a path nothing
  // writes — and the build fails with "Build input file cannot be found".
  project.addBuildPhase(
    [`${targetName}/NotificationService.swift`],
    "PBXSourcesBuildPhase",
    "Sources",
    target.uuid,
  );

  // Set build settings on this target's own configurations.
  //
  // Selecting them by bundle identifier does not work: addTarget stores
  // the value quoted, so a comparison against the bare id matches nothing
  // and every setting below is silently skipped. The build then fails on
  // SWIFT_VERSION '' with no sign of why, and the entitlements and
  // Info.plist paths never reach the target either. The target names its
  // own configuration list, so ask it.
  const configs = project.pbxXCBuildConfigurationSection();
  const ours = targetConfigKeys(project, target);
  for (const configKey in configs) {
    const config = configs[configKey];
    if (typeof config === "object" && config.buildSettings && ours.has(configKey)) {
      config.buildSettings.SWIFT_VERSION = "5.0";
      config.buildSettings.IPHONEOS_DEPLOYMENT_TARGET = "17.0";
      config.buildSettings.CODE_SIGN_ENTITLEMENTS = entitlementsPath;
      config.buildSettings.TARGETED_DEVICE_FAMILY = '"1,2"';
      config.buildSettings.GENERATE_INFOPLIST_FILE = "NO";
      config.buildSettings.INFOPLIST_FILE = `${targetName}/Info.plist`;
      config.buildSettings.CURRENT_PROJECT_VERSION = "1";
      config.buildSettings.MARKETING_VERSION = "1.0";
    }
  }
}

/**
 * The build configuration keys belonging to one target.
 *
 * Selecting them by bundle identifier does not work: addTarget stores the
 * value quoted, so comparing it to the bare id matches nothing and every
 * setting is silently skipped. The build then fails on SWIFT_VERSION ''
 * with no sign of why, and the entitlements and Info.plist paths never
 * reach the target either. The target names its own configuration list,
 * so ask it.
 */
function targetConfigKeys(project, target) {
  const lists = project.pbxXCConfigurationList();
  const listKey = target.pbxNativeTarget.buildConfigurationList;
  return new Set(
    (lists[listKey]?.buildConfigurations || []).map((entry) => entry.value),
  );
}

module.exports = withPushCrypto;

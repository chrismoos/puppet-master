// Expo config plugin: adds an XCUITest UI test target so geometric
// and interaction assertions survive prebuild regeneration.
//
// The `xcode` npm package used by Expo does not know about the
// `ui_testing_bundle` product type, so we create the target as a
// `unit_test_bundle` and then patch its productType to the UI testing
// variant and set TEST_HOST to empty (UI tests launch their own app).

const { withXcodeProject, withPodfile } = require("expo/config-plugins");
const fs = require("fs");
const path = require("path");

const UI_TEST_TARGET_NAME = "PuppetMasterUITests";
const UI_TEST_BUNDLE_ID_SUFFIX = ".uitests";
const UI_TEST_PRODUCT_TYPE = "com.apple.product-type.bundle.ui-testing";

function withUITests(config) {
  // In UI-test builds, exclude expo-dev-client from Expo module
  // autolinking. Its native code crashes with an XPC assertion in
  // Release builds without a dev server.
  if (process.env.PM_UI_TEST_BUILD === "1") {
    config = withPodfile(config, (mod) => {
      mod.modResults.contents = mod.modResults.contents.replace(
        "use_expo_modules!",
        "use_expo_modules!(exclude: ['expo-dev-client', 'expo-dev-launcher', 'expo-dev-menu', 'expo-dev-menu-interface'])"
      );
      return mod;
    });
  }

  return withXcodeProject(config, async (mod) => {
    const projectRoot = mod.modRequest.projectRoot;
    const project = mod.modResults;
    const bundleId =
      config.ios?.bundleIdentifier + UI_TEST_BUNDLE_ID_SUFFIX;

    // Create the UI test directory and copy sources.
    const testDir = path.join(projectRoot, "ios", UI_TEST_TARGET_NAME);
    fs.mkdirSync(testDir, { recursive: true });

    // Copy all Swift test sources from the plugin's ios/ directory.
    const srcDir = path.join(__dirname, "ios");
    const files = fs.readdirSync(srcDir).filter((f) => f.endsWith(".swift"));
    for (const file of files) {
      fs.copyFileSync(path.join(srcDir, file), path.join(testDir, file));
    }

    const appTargetName = mod.modRequest.projectName || "PuppetMaster";
    addUITestTarget(project, {
      targetName: UI_TEST_TARGET_NAME,
      bundleId,
      appTargetName,
      files: files.map((f) => `${UI_TEST_TARGET_NAME}/${f}`),
    });

    mod.modResults = project;
    return mod;
  });
}

function addUITestTarget(project, opts) {
  const { targetName, bundleId, appTargetName, files } = opts;

  // Idempotent: skip if already present.
  const existing = project.pbxTargetByName(targetName);
  if (existing) return;

  // Create as unit_test_bundle (the only test type the xcode package
  // recognises), then fix the productType to the UI-testing variant.
  const target = project.addTarget(
    targetName,
    "unit_test_bundle",
    targetName,
    bundleId,
  );

  // Patch productType from unit-test to ui-testing in the native target.
  const nativeTargets = project.pbxNativeTargetSection();
  for (const key in nativeTargets) {
    const entry = nativeTargets[key];
    if (typeof entry === "object" && entry.name === `"${targetName}"`) {
      entry.productType = `"${UI_TEST_PRODUCT_TYPE}"`;
    }
  }

  project.addBuildPhase(
    files,
    "PBXSourcesBuildPhase",
    "Sources",
    target.uuid,
  );

  // Configure build settings on this target's own configurations.
  const configs = project.pbxXCBuildConfigurationSection();
  const ours = targetConfigKeys(project, target);
  for (const configKey in configs) {
    const config = configs[configKey];
    if (typeof config === "object" && config.buildSettings && ours.has(configKey)) {
      config.buildSettings.SWIFT_VERSION = "5.0";
      config.buildSettings.IPHONEOS_DEPLOYMENT_TARGET = "17.0";
      config.buildSettings.TARGETED_DEVICE_FAMILY = '"1,2"';
      config.buildSettings.GENERATE_INFOPLIST_FILE = "YES";
      delete config.buildSettings.INFOPLIST_FILE;
      config.buildSettings.TEST_HOST = '""';
      config.buildSettings.TEST_TARGET_NAME = `"${appTargetName}"`;
      config.buildSettings.PRODUCT_BUNDLE_IDENTIFIER = `"${bundleId}"`;
    }
  }
}

function targetConfigKeys(project, target) {
  const lists = project.pbxXCConfigurationList();
  const listKey = target.pbxNativeTarget.buildConfigurationList;
  return new Set(
    (lists[listKey]?.buildConfigurations || []).map((entry) => entry.value),
  );
}

module.exports = withUITests;

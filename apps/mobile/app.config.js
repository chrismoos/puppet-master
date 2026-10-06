const base = require("./app.json");

module.exports = ({ config }) => {
  const merged = { ...base.expo, ...config };
  const isUITest = process.env.PM_UI_TEST_BUILD === "1";

  if (isUITest) {
    // ── UI-test build profile ──
    // Produces an isolated binary that auto-enrolls against a throwaway
    // daemon seeded by scripts/controller-fixture.mjs. Built with Xcode
    // Release configuration so the dev-launcher UI is bypassed and the
    // JS bundle is embedded. The binary uses a distinct bundle identifier
    // so it cannot replace development or release installs.
    //
    // Required env vars (from the fixture's PM_FIXTURE_* output):
    //   PM_UI_TEST_BUILD=1
    //   PM_DEV_CONTROLLER_URL   — fixture's PM_FIXTURE_BASE_URL
    //   PM_DEV_ENROLL_TOKEN     — fixture's PM_FIXTURE_MOBILE_ENROLL_TOKEN

    const controllerUrl = process.env.PM_DEV_CONTROLLER_URL;
    const enrollToken = process.env.PM_DEV_ENROLL_TOKEN;
    if (!controllerUrl) {
      throw new Error(
        "PM_UI_TEST_BUILD=1 requires PM_DEV_CONTROLLER_URL"
      );
    }
    if (!enrollToken) {
      throw new Error(
        "PM_UI_TEST_BUILD=1 requires PM_DEV_ENROLL_TOKEN"
      );
    }

    merged.ios = {
      ...merged.ios,
      bundleIdentifier: "com.tech9.puppetmaster.uitest",
    };
    merged.name = "PM UITest";

    // Remove expo-dev-client plugin: its native code crashes with an
    // XPC assertion in Release builds without a dev server.
    merged.plugins = (merged.plugins || []).filter(
      (p) => p !== "expo-dev-client"
    );

    // Inject only the controller URL, enrollment token, and the build
    // flag. No username or password — the token is single-use and scoped
    // to the throwaway daemon.
    merged.extra = {
      ...merged.extra,
      PM_UI_TEST_BUILD: true,
      PM_DEV_CONTROLLER_URL: controllerUrl,
      PM_DEV_ENROLL_TOKEN: enrollToken,
    };
  }

  return merged;
};

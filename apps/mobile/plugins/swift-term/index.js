// SwiftTerm ships as SwiftPM source, while Expo's local modules are CocoaPods
// targets. Compile the pinned sources directly in our pod to avoid Xcode
// embedding the static SwiftPM product twice in the final application.

const { withDangerousMod } = require("expo/config-plugins");
const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const SWIFTTERM_URL = "https://github.com/migueldeicaza/SwiftTerm.git";
const SWIFTTERM_REVISION = "139a0e853958a5a6a2da8e2962b489fff6eb3201"; // v1.17.0

module.exports = function withPuppetSwiftTerm(config) {
  return withDangerousMod(config, ["ios", async (mod) => {
    const vendorDir = path.join(mod.modRequest.projectRoot, "plugins", "swift-term", "ios", "vendor", "SwiftTerm");
    const revisionFile = path.join(vendorDir, ".puppet-revision");
    if (fs.existsSync(revisionFile) && fs.readFileSync(revisionFile, "utf8").trim() === SWIFTTERM_REVISION) {
      return mod;
    }

    fs.rmSync(vendorDir, { recursive: true, force: true });
    fs.mkdirSync(path.dirname(vendorDir), { recursive: true });
    execFileSync("git", ["clone", "--filter=blob:none", "--no-checkout", SWIFTTERM_URL, vendorDir], { stdio: "inherit" });
    execFileSync("git", ["-C", vendorDir, "checkout", "--detach", SWIFTTERM_REVISION], { stdio: "inherit" });
    fs.writeFileSync(revisionFile, `${SWIFTTERM_REVISION}\n`);
    return mod;
  }]);
};

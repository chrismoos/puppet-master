import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";

const require = createRequire(import.meta.url);
const { podDeploymentTargetPodfile, POD_MIN_IOS } = require("./index.js") as {
  podDeploymentTargetPodfile(contents: string): string;
  POD_MIN_IOS: string;
};

const PODFILE = [
  "target 'PuppetMaster' do",
  "  post_install do |installer|",
  "    react_native_post_install(installer)",
  "  end",
  "end",
  "",
].join("\n");

describe("pod deployment target floor", () => {
  it("raises pod targets inside post_install, after React Native's own step", () => {
    const out = podDeploymentTargetPodfile(PODFILE);
    const rn = out.indexOf("react_native_post_install");
    const floor = out.indexOf(`Gem::Version.new('${POD_MIN_IOS}')`);
    const end = out.indexOf("\n  end\n");
    expect(rn).toBeGreaterThan(0);
    expect(floor).toBeGreaterThan(rn);
    expect(end).toBeGreaterThan(floor);
    expect(out).toContain(`build.build_settings['IPHONEOS_DEPLOYMENT_TARGET'] = '${POD_MIN_IOS}'`);
  });

  it("adds the floor once when prebuild runs over an existing project", () => {
    const once = podDeploymentTargetPodfile(PODFILE);
    expect(podDeploymentTargetPodfile(once)).toBe(once);
  });

  it("refuses a Podfile with no post_install block rather than skipping the floor", () => {
    expect(() => podDeploymentTargetPodfile("target 'PuppetMaster' do\nend\n")).toThrow(/post_install/);
  });
});

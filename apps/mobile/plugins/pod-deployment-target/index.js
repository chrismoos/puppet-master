// Raises every pod target below POD_MIN_IOS to it. Xcode 27 refuses
// deployment targets under iOS 15, and react-native-svg's filter subspec
// still declares 12.4, which the app's own deployment target does not
// override.

const { withPodfile } = require("expo/config-plugins");

const POD_MIN_IOS = "15.0";
const MARKER = "# pod-deployment-target";

const BLOCK = `    ${MARKER}
    installer.pods_project.targets.each do |target|
      target.build_configurations.each do |build|
        current = build.build_settings['IPHONEOS_DEPLOYMENT_TARGET']
        if current && Gem::Version.new(current) < Gem::Version.new('${POD_MIN_IOS}')
          build.build_settings['IPHONEOS_DEPLOYMENT_TARGET'] = '${POD_MIN_IOS}'
        end
      end
    end
`;

/** The Podfile with the floor added at the end of post_install, once. */
function podDeploymentTargetPodfile(contents) {
  if (contents.includes(MARKER)) return contents;
  const start = contents.indexOf("post_install do |installer|");
  if (start < 0) {
    throw new Error("pod-deployment-target plugin: the Podfile has no post_install block");
  }
  const close = contents.indexOf("\n  end\n", start);
  if (close < 0) {
    throw new Error("pod-deployment-target plugin: the Podfile's post_install block has no end");
  }
  return `${contents.slice(0, close + 1)}${BLOCK}${contents.slice(close + 1)}`;
}

function withPodDeploymentTarget(config) {
  return withPodfile(config, (mod) => {
    mod.modResults.contents = podDeploymentTargetPodfile(mod.modResults.contents);
    return mod;
  });
}

module.exports = withPodDeploymentTarget;
module.exports.podDeploymentTargetPodfile = podDeploymentTargetPodfile;
module.exports.POD_MIN_IOS = POD_MIN_IOS;

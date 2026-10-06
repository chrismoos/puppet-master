const { getDefaultConfig } = require("expo/metro-config");
const path = require("path");

// The workspace hoists dependencies to the repository root, so Metro must
// watch the monorepo and resolve from both node_modules trees.
const projectRoot = __dirname;
const workspaceRoot = path.resolve(projectRoot, "../..");

const config = getDefaultConfig(projectRoot);
config.watchFolders = [workspaceRoot];
config.resolver.nodeModulesPaths = [
  path.join(projectRoot, "node_modules"),
  path.join(workspaceRoot, "node_modules"),
];

module.exports = config;

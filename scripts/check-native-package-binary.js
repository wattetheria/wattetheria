#!/usr/bin/env node

const fs = require("node:fs");
const path = require("node:path");

const BASE_NAME = "wattetheria-client-cli";

function binaryName(platform) {
  return platform === "win32" ? `${BASE_NAME}.exe` : BASE_NAME;
}

function readPackageJson(packageDir) {
  const manifestPath = path.join(packageDir, "package.json");
  if (!fs.existsSync(manifestPath)) {
    throw new Error(`package.json not found in ${packageDir}`);
  }
  return JSON.parse(fs.readFileSync(manifestPath, "utf8"));
}

function firstArrayValue(manifest, key) {
  if (!Array.isArray(manifest[key]) || manifest[key].length !== 1) {
    throw new Error(`${manifest.name || "package"} must define exactly one ${key} value.`);
  }
  return manifest[key][0];
}

function checkNativePackageBinary(packageDir) {
  const manifest = readPackageJson(packageDir);
  const platform = firstArrayValue(manifest, "os");
  firstArrayValue(manifest, "cpu");

  const binaryPath = path.join(packageDir, "bin", binaryName(platform));
  if (!fs.existsSync(binaryPath)) {
    throw new Error(
      `Missing native CLI binary for ${manifest.name}: ${binaryPath}. ` +
        "Run npm run stage:native-cli with the matching --platform and --arch first."
    );
  }
  console.log(`native binary present for ${manifest.name}: ${binaryPath}`);
}

try {
  checkNativePackageBinary(path.resolve(process.argv[2] || process.cwd()));
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exit(1);
}

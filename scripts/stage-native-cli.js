#!/usr/bin/env node

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const ROOT_DIR = path.resolve(__dirname, "..");
const BASE_NAME = "wattetheria-client-cli";
const NATIVE_PACKAGE_PREFIX = "@wattetheria/cli";
const SUPPORTED_PLATFORMS = new Set(["darwin", "linux", "win32"]);
const SUPPORTED_ARCHES = new Set(["x64", "arm64"]);

function parseArgs(argv) {
  const options = {
    platform: process.env.WATTETHERIA_NATIVE_PLATFORM || process.platform,
    arch: process.env.WATTETHERIA_NATIVE_ARCH || process.arch,
    source: process.env.WATTETHERIA_NATIVE_CLI_BIN || "",
    releaseAssetsDir: "",
    extras: [],
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--platform") {
      options.platform = requireValue(arg, argv[++index]);
    } else if (arg === "--arch") {
      options.arch = requireValue(arg, argv[++index]);
    } else if (arg === "--source") {
      options.source = requireValue(arg, argv[++index]);
    } else if (arg === "--release-assets-dir") {
      options.releaseAssetsDir = requireValue(arg, argv[++index]);
    } else if (arg === "--extra") {
      options.extras.push(requireValue(arg, argv[++index]));
    } else {
      throw new Error(`Unknown option: ${arg}`);
    }
  }

  return options;
}

function requireValue(flag, value) {
  if (!value || value.startsWith("-")) {
    throw new Error(`Missing value for ${flag}`);
  }
  return value;
}

function binaryName(platform) {
  return platform === "win32" ? `${BASE_NAME}.exe` : BASE_NAME;
}

function defaultSource(platform) {
  return path.join(ROOT_DIR, "target", "release", binaryName(platform));
}

function targetKey(platform, arch) {
  if (!SUPPORTED_PLATFORMS.has(platform)) {
    throw new Error(`Unsupported native CLI platform: ${platform}`);
  }
  if (!SUPPORTED_ARCHES.has(arch)) {
    throw new Error(`Unsupported native CLI arch: ${arch}`);
  }
  return `${platform}-${arch}`;
}

function nativePackageDir(key) {
  return path.join(ROOT_DIR, "npm", "native", key);
}

function copyBinary(source, targetDir, platform, targetName = binaryName(platform)) {
  const target = path.join(targetDir, targetName);
  fs.mkdirSync(targetDir, { recursive: true });
  fs.copyFileSync(source, target);
  if (platform !== "win32") {
    fs.chmodSync(target, 0o755);
  }
  return target;
}

function stageNativeCli(options) {
  const key = targetKey(options.platform, options.arch);
  const source = path.resolve(options.source || defaultSource(options.platform));
  if (!fs.existsSync(source)) {
    throw new Error(
      `Native CLI binary not found at ${source}. Build it first or pass --source.`
    );
  }

  const packageDir = nativePackageDir(key);
  if (!fs.existsSync(path.join(packageDir, "package.json"))) {
    throw new Error(
      `Native package manifest not found for ${NATIVE_PACKAGE_PREFIX}-${key}.`
    );
  }

  const mainTarget = copyBinary(
    source,
    path.join(ROOT_DIR, "bin", "native", key),
    options.platform
  );
  const packageTarget = copyBinary(
    source,
    path.join(packageDir, "bin"),
    options.platform
  );
  console.log(`staged ${mainTarget}`);
  console.log(`staged ${packageTarget}`);

  // Native node binaries (wattetheria-kernel, wattswarm, wattswarm-runtime)
  // ship beside the client CLI for no-Docker deployments.
  for (const extra of options.extras) {
    const extraSource = path.resolve(extra);
    if (!fs.existsSync(extraSource)) {
      throw new Error(`Native binary not found at ${extraSource}.`);
    }
    const extraName = path.basename(extraSource);
    for (const dir of [path.join(ROOT_DIR, "bin", "native", key), path.join(packageDir, "bin")]) {
      console.log(`staged ${copyBinary(extraSource, dir, options.platform, extraName)}`);
    }
  }

  if (options.releaseAssetsDir) {
    fs.mkdirSync(options.releaseAssetsDir, { recursive: true });
    for (const baseName of [BASE_NAME, "wattetheria-kernel", "wattswarm", "wattswarm-runtime"]) {
      const name = options.platform === "win32" ? `${baseName}.exe` : baseName;
      const sourcePath = path.join(packageDir, "bin", name);
      const assetName = `wattetheria-native-${key}-${name}`;
      copyBinary(sourcePath, options.releaseAssetsDir, options.platform, assetName);
    }
  }
}

try {
  stageNativeCli(parseArgs(process.argv.slice(2)));
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exit(1);
}

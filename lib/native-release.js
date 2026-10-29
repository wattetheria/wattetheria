"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const NATIVE_RELEASE_PACKAGE_PREFIX = "@wattetheria/native";
const BINARY_NAMES = [
  "wattetheria-client-cli",
  "wattetheria-kernel",
  "wattswarm",
  "wattswarm-runtime"
];
const SUPPORTED_PLATFORMS = new Set(["darwin", "linux", "win32"]);
const SUPPORTED_ARCHES = new Set(["x64", "arm64"]);
const RELEASE_TAG_RE = /^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$/;
const RELEASE_VERSION_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

function nativePlatformKey(platform, arch) {
  if (!SUPPORTED_PLATFORMS.has(platform) || !SUPPORTED_ARCHES.has(arch)) {
    return "";
  }
  return `${platform}-${arch}`;
}

function nativeCacheRoot() {
  return process.env.WATTETHERIA_NATIVE_CACHE_DIR
    ? path.resolve(process.env.WATTETHERIA_NATIVE_CACHE_DIR)
    : path.join(os.homedir(), ".wattetheria", "native");
}

function binaryName(baseName, platform) {
  return platform === "win32" ? `${baseName}.exe` : baseName;
}

function nativeReleasePackageName(platformKey) {
  return `${NATIVE_RELEASE_PACKAGE_PREFIX}-${platformKey}`;
}

function npmOutput(runNpm, args, action, options = {}) {
  const result = runNpm(args, { ...options, encoding: "utf8" });
  if (result.error) {
    throw new Error(`Failed to ${action}: ${result.error.message}`);
  }
  if (result.status !== 0) {
    const detail = String(result.stderr || "").trim();
    throw new Error(`Failed to ${action} from npm${detail ? `: ${detail}` : "."}`);
  }
  return String(result.stdout || "").trim();
}

function latestNativeRelease(platform, arch, runNpm) {
  const platformKey = nativePlatformKey(platform, arch);
  if (!platformKey) {
    throw new Error(`Native runtime is not available for ${platform}-${arch}.`);
  }
  const packageName = nativeReleasePackageName(platformKey);
  const version = npmOutput(
    runNpm,
    ["view", packageName, "version"],
    `query the latest ${packageName} version`
  );
  if (!RELEASE_VERSION_RE.test(version)) {
    throw new Error(`npm returned an invalid ${packageName} version: ${version}`);
  }
  return { packageName, platformKey, version };
}

function currentNativeBinDir({
  platform = process.platform,
  arch = process.arch,
  cacheRoot = nativeCacheRoot()
} = {}) {
  const platformKey = nativePlatformKey(platform, arch);
  if (!platformKey) {
    return "";
  }
  const pointerPath = path.join(cacheRoot, "current.json");
  if (!fs.existsSync(pointerPath)) {
    return "";
  }
  try {
    const current = JSON.parse(fs.readFileSync(pointerPath, "utf8"));
    if (current.platformKey !== platformKey || !RELEASE_TAG_RE.test(current.tag || "")) {
      return "";
    }
    const version = current.tag.match(RELEASE_TAG_RE)[1];
    const binDir = path.join(cacheRoot, version, platformKey);
    return BINARY_NAMES.every((name) => fs.existsSync(path.join(binDir, binaryName(name, platform))))
      ? binDir
      : "";
  } catch (_error) {
    return "";
  }
}

function writeCurrentRelease(cacheRoot, tag, platformKey) {
  fs.mkdirSync(cacheRoot, { recursive: true });
  const pointerPath = path.join(cacheRoot, "current.json");
  const temporaryPath = `${pointerPath}.${process.pid}.tmp`;
  fs.writeFileSync(temporaryPath, `${JSON.stringify({ tag, platformKey })}\n`);
  try {
    fs.renameSync(temporaryPath, pointerPath);
  } catch (error) {
    if (process.platform !== "win32" || !["EEXIST", "EPERM"].includes(error.code)) {
      throw error;
    }
    fs.rmSync(pointerPath, { force: true });
    fs.renameSync(temporaryPath, pointerPath);
  }
}

function installNativeReleasePackage({
  packageName,
  platform,
  platformKey,
  version,
  cacheRoot,
  runNpm
}) {
  const versionDir = path.join(cacheRoot, version, platformKey);
  if (BINARY_NAMES.every((name) => fs.existsSync(path.join(versionDir, binaryName(name, platform))))) {
    return versionDir;
  }

  const stagingDir = path.join(cacheRoot, `.staging-${platformKey}-${process.pid}`);
  fs.rmSync(stagingDir, { recursive: true, force: true });
  fs.mkdirSync(stagingDir, { recursive: true });
  try {
    // runNpm uses a shell on Windows, so keep cache paths out of the argument list.
    npmOutput(
      runNpm,
      [
        "install",
        "--prefix", ".",
        "--no-save",
        "--no-package-lock",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        `${packageName}@${version}`
      ],
      `download ${packageName}@${version}`,
      { cwd: stagingDir }
    );
    const packageBinDir = path.join(stagingDir, "node_modules", ...packageName.split("/"), "bin");
    for (const baseName of BINARY_NAMES) {
      const targetPath = path.join(packageBinDir, binaryName(baseName, platform));
      if (!fs.existsSync(targetPath)) {
        throw new Error(`${packageName}@${version} is missing ${binaryName(baseName, platform)}.`);
      }
      if (platform !== "win32") {
        fs.chmodSync(targetPath, 0o755);
      }
    }

    fs.mkdirSync(path.dirname(versionDir), { recursive: true });
    if (fs.existsSync(versionDir)) {
      fs.rmSync(versionDir, { recursive: true, force: true });
    }
    fs.renameSync(packageBinDir, versionDir);
  } finally {
    fs.rmSync(stagingDir, { recursive: true, force: true });
  }
  return versionDir;
}

function ensureLatestNativeBinaries({
  platform = process.platform,
  arch = process.arch,
  cacheRoot = nativeCacheRoot(),
  runNpm
}) {
  const latest = latestNativeRelease(platform, arch, runNpm);
  fs.mkdirSync(cacheRoot, { recursive: true });
  const binDir = installNativeReleasePackage({ ...latest, platform, cacheRoot, runNpm });
  const tag = `v${latest.version}`;
  writeCurrentRelease(cacheRoot, tag, latest.platformKey);
  return {
    tag,
    version: latest.version,
    platformKey: latest.platformKey,
    binDir
  };
}

module.exports = {
  currentNativeBinDir,
  ensureLatestNativeBinaries,
  nativePlatformKey,
  nativeReleasePackageName
};

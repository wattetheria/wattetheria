"use strict";

const fs = require("node:fs");
const { createWriteStream } = fs;
const os = require("node:os");
const path = require("node:path");
const { Readable } = require("node:stream");
const { pipeline } = require("node:stream/promises");

const RELEASES_URL = "https://api.github.com/repos/wattetheria/wattetheria/releases?per_page=100";
const BINARY_NAMES = [
  "wattetheria-client-cli",
  "wattetheria-kernel",
  "wattswarm",
  "wattswarm-runtime"
];
const SUPPORTED_PLATFORMS = new Set(["darwin", "linux", "win32"]);
const SUPPORTED_ARCHES = new Set(["x64", "arm64"]);
const RELEASE_TAG_RE = /^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$/;
const FETCH_TIMEOUT_MS = 300000;

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

function releaseAssetName(platformKey, name) {
  return `wattetheria-native-${platformKey}-${name}`;
}

function releaseAssetMap(release, platformKey, platform) {
  const assets = new Map((release.assets || []).map((asset) => [asset.name, asset]));
  const binaries = new Map();
  for (const baseName of BINARY_NAMES) {
    const name = binaryName(baseName, platform);
    const asset = assets.get(releaseAssetName(platformKey, name));
    if (!asset || typeof asset.browser_download_url !== "string") {
      return null;
    }
    binaries.set(name, asset);
  }
  return binaries;
}

async function fetchResponse(fetchImpl, url, headers) {
  const response = await fetchImpl(url, {
    headers,
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS)
  });
  if (!response.ok) {
    const detail = (await response.text()).trim();
    throw new Error(
      `GitHub request failed (${response.status})${detail ? `: ${detail}` : ""}`
    );
  }
  return response;
}

async function latestNativeRelease(platform, arch, fetchImpl) {
  const platformKey = nativePlatformKey(platform, arch);
  if (!platformKey) {
    throw new Error(`Native runtime is not available for ${platform}-${arch}.`);
  }

  const response = await fetchResponse(fetchImpl, RELEASES_URL, {
    Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
    "User-Agent": "wattetheria-cli"
  });
  const releases = await response.json();
  if (!Array.isArray(releases)) {
    throw new Error("GitHub returned an invalid Wattetheria release list.");
  }
  const release = releases.find((candidate) =>
    !candidate.draft
    && !candidate.prerelease
    && RELEASE_TAG_RE.test(candidate.tag_name || "")
    && releaseAssetMap(candidate, platformKey, platform)
  );
  if (!release) {
    throw new Error(`No published Wattetheria native release was found for ${platformKey}.`);
  }
  return {
    release,
    platformKey,
    version: release.tag_name.match(RELEASE_TAG_RE)[1],
    assets: releaseAssetMap(release, platformKey, platform)
  };
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

async function downloadReleaseAssets({
  release,
  assets,
  platform,
  platformKey,
  version,
  cacheRoot,
  fetchImpl
}) {
  const versionDir = path.join(cacheRoot, version, platformKey);
  if (BINARY_NAMES.every((name) => fs.existsSync(path.join(versionDir, binaryName(name, platform))))) {
    return versionDir;
  }

  const stagingDir = path.join(cacheRoot, `.staging-${platformKey}-${process.pid}`);
  fs.rmSync(stagingDir, { recursive: true, force: true });
  fs.mkdirSync(stagingDir, { recursive: true });
  try {
    for (const [name, asset] of assets) {
      const expectedUrlPrefix = `https://github.com/wattetheria/wattetheria/releases/download/${encodeURIComponent(release.tag_name)}/`;
      if (!asset.browser_download_url.startsWith(expectedUrlPrefix)) {
        throw new Error(`Unexpected native release asset URL: ${asset.browser_download_url}`);
      }
      const response = await fetchResponse(fetchImpl, asset.browser_download_url, {
        Accept: "application/octet-stream",
        "User-Agent": "wattetheria-cli"
      });
      if (!response.body) {
        throw new Error(`Native release asset ${asset.name} has no response body.`);
      }
      const targetPath = path.join(stagingDir, name);
      await pipeline(Readable.fromWeb(response.body), createWriteStream(targetPath, { flags: "wx" }));
      if (platform !== "win32") {
        fs.chmodSync(targetPath, 0o755);
      }
    }

    fs.mkdirSync(path.dirname(versionDir), { recursive: true });
    if (fs.existsSync(versionDir)) {
      fs.rmSync(versionDir, { recursive: true, force: true });
    }
    fs.renameSync(stagingDir, versionDir);
  } catch (error) {
    fs.rmSync(stagingDir, { recursive: true, force: true });
    throw error;
  }
  return versionDir;
}

async function ensureLatestNativeBinaries({
  platform = process.platform,
  arch = process.arch,
  cacheRoot = nativeCacheRoot(),
  fetchImpl = globalThis.fetch
} = {}) {
  if (typeof fetchImpl !== "function") {
    throw new Error("Native release downloads require Node.js 20 or newer.");
  }
  const latest = await latestNativeRelease(platform, arch, fetchImpl);
  fs.mkdirSync(cacheRoot, { recursive: true });
  const binDir = await downloadReleaseAssets({ ...latest, platform, cacheRoot, fetchImpl });
  writeCurrentRelease(cacheRoot, latest.release.tag_name, latest.platformKey);
  return {
    tag: latest.release.tag_name,
    version: latest.version,
    platformKey: latest.platformKey,
    binDir
  };
}

module.exports = {
  currentNativeBinDir,
  ensureLatestNativeBinaries,
  nativePlatformKey,
  releaseAssetName
};

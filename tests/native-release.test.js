"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

const {
  currentNativeBinDir,
  ensureLatestNativeBinaries,
  releaseAssetName
} = require("../lib/native-release");

const BINARY_NAMES = [
  "wattetheria-client-cli",
  "wattetheria-kernel",
  "wattswarm",
  "wattswarm-runtime"
];

function release(tag, platformKey, platform, binaryNames = BINARY_NAMES) {
  return {
    tag_name: tag,
    draft: false,
    prerelease: false,
    assets: binaryNames.map((baseName) => {
      const name = platform === "win32" ? `${baseName}.exe` : baseName;
      const assetName = releaseAssetName(platformKey, name);
      return {
        name: assetName,
        browser_download_url: `https://github.com/wattetheria/wattetheria/releases/download/${tag}/${assetName}`
      };
    })
  };
}

test("native release installs the latest complete platform asset set and caches it", async (context) => {
  const cacheRoot = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-release-"));
  context.after(() => fs.rmSync(cacheRoot, { recursive: true, force: true }));
  const platform = process.platform === "win32" ? "win32" : "linux";
  const arch = process.arch === "arm64" ? "arm64" : "x64";
  const platformKey = `${platform}-${arch}`;
  const releases = [
    release("v1.3.0", platformKey, platform, BINARY_NAMES.slice(0, 2)),
    release("v1.2.0", platformKey, platform)
  ];
  const calls = [];
  const fetchImpl = async (url) => {
    calls.push(url);
    if (url.includes("/releases?per_page=")) {
      return new Response(JSON.stringify(releases), { status: 200 });
    }
    return new Response(Buffer.from(`asset:${path.basename(url)}`), { status: 200 });
  };

  const installed = await ensureLatestNativeBinaries({
    platform,
    arch,
    cacheRoot,
    fetchImpl
  });

  assert.equal(installed.tag, "v1.2.0");
  assert.equal(installed.version, "1.2.0");
  assert.equal(currentNativeBinDir({ platform, arch, cacheRoot }), installed.binDir);
  for (const baseName of BINARY_NAMES) {
    const name = platform === "win32" ? `${baseName}.exe` : baseName;
    const binaryPath = path.join(installed.binDir, name);
    assert.equal(fs.readFileSync(binaryPath, "utf8"), `asset:${releaseAssetName(platformKey, name)}`);
    if (platform !== "win32") {
      assert.ok(fs.statSync(binaryPath).mode & 0o111, `${name} should be executable`);
    }
  }

  const assetRequests = calls.filter((url) => !url.includes("/releases?per_page="));
  assert.equal(assetRequests.length, BINARY_NAMES.length);
  await ensureLatestNativeBinaries({ platform, arch, cacheRoot, fetchImpl });
  assert.equal(calls.filter((url) => !url.includes("/releases?per_page=")).length, BINARY_NAMES.length);
});

test("native release rejects unsupported platform targets", async () => {
  await assert.rejects(
    ensureLatestNativeBinaries({ platform: "freebsd", arch: "x64", fetchImpl: async () => {} }),
    /Native runtime is not available for freebsd-x64/
  );
});

test("native staging names each binary as a platform-specific release asset", (context) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-stage-test-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const scriptDirectory = path.join(root, "scripts");
  const packageDirectory = path.join(root, "npm", "native", "linux-x64");
  const sourceDirectory = path.join(root, "source");
  const releaseAssetsDirectory = path.join(root, "release-assets");
  fs.mkdirSync(scriptDirectory, { recursive: true });
  fs.mkdirSync(packageDirectory, { recursive: true });
  fs.mkdirSync(sourceDirectory, { recursive: true });
  fs.copyFileSync(path.join(__dirname, "..", "scripts", "stage-native-cli.js"), path.join(scriptDirectory, "stage-native-cli.js"));
  fs.writeFileSync(path.join(packageDirectory, "package.json"), JSON.stringify({
    name: "@wattetheria/cli-linux-x64"
  }));

  const binaryNames = [
    "wattetheria-client-cli",
    "wattetheria-kernel",
    "wattswarm",
    "wattswarm-runtime"
  ];
  const sourcePaths = binaryNames.map((name, index) => {
    const sourcePath = path.join(sourceDirectory, name);
    fs.writeFileSync(sourcePath, `binary-${index}`);
    return sourcePath;
  });
  const result = spawnSync(process.execPath, [
    path.join(scriptDirectory, "stage-native-cli.js"),
    "--platform", "linux",
    "--arch", "x64",
    "--source", sourcePaths[0],
    "--extra", sourcePaths[1],
    "--extra", sourcePaths[2],
    "--extra", sourcePaths[3],
    "--release-assets-dir", releaseAssetsDirectory
  ], { encoding: "utf8" });

  assert.equal(result.status, 0, result.stderr);
  for (const [index, name] of binaryNames.entries()) {
    const assetName = `wattetheria-native-linux-x64-${name}`;
    assert.equal(fs.readFileSync(path.join(releaseAssetsDirectory, assetName), "utf8"), `binary-${index}`);
  }
});

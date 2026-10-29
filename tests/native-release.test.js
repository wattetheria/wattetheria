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
  nativeReleasePackageName
} = require("../lib/native-release");

const BINARY_NAMES = [
  "wattetheria-client-cli",
  "wattetheria-kernel",
  "wattswarm",
  "wattswarm-runtime"
];

function fakeNpmRegistry(version, platform, binaryNames = BINARY_NAMES) {
  const calls = [];
  const runNpm = (args, options) => {
    calls.push({ args, options });
    if (args[0] === "view") {
      return { status: 0, stdout: `${version}\n`, stderr: "" };
    }
    const prefix = path.resolve(options.cwd, args[args.indexOf("--prefix") + 1]);
    const [packageName] = args[args.length - 1].split(/@(?=[^@]*$)/);
    const binDirectory = path.join(prefix, "node_modules", ...packageName.split("/"), "bin");
    fs.mkdirSync(binDirectory, { recursive: true });
    for (const baseName of binaryNames) {
      const name = platform === "win32" ? `${baseName}.exe` : baseName;
      fs.writeFileSync(path.join(binDirectory, name), `package:${args[args.length - 1]}:${name}`);
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  return { calls, runNpm };
}

test("native release installs the latest npm native package and caches it", (context) => {
  const cacheRoot = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-release-"));
  context.after(() => fs.rmSync(cacheRoot, { recursive: true, force: true }));
  const platform = process.platform === "win32" ? "win32" : "linux";
  const arch = process.arch === "arm64" ? "arm64" : "x64";
  const platformKey = `${platform}-${arch}`;
  const packageName = nativeReleasePackageName(platformKey);
  const { calls, runNpm } = fakeNpmRegistry("1.2.0", platform);

  const installed = ensureLatestNativeBinaries({ platform, arch, cacheRoot, runNpm });

  assert.equal(packageName, `@wattetheria/native-${platformKey}`);
  assert.equal(installed.tag, "v1.2.0");
  assert.equal(installed.version, "1.2.0");
  assert.equal(installed.binDir, path.join(cacheRoot, "1.2.0", platformKey));
  assert.equal(currentNativeBinDir({ platform, arch, cacheRoot }), installed.binDir);
  assert.deepEqual(calls[0].args, ["view", packageName, "version"]);
  const install = calls[1];
  assert.equal(install.args[0], "install");
  assert.equal(install.args[install.args.indexOf("--prefix") + 1], ".");
  assert.ok(install.options.cwd.startsWith(path.join(cacheRoot, ".staging-")));
  assert.ok(install.args.every((arg) => !arg.includes(cacheRoot)), "cache paths must not be npm arguments");
  assert.equal(install.args[install.args.length - 1], `${packageName}@1.2.0`);
  for (const baseName of BINARY_NAMES) {
    const name = platform === "win32" ? `${baseName}.exe` : baseName;
    const binaryPath = path.join(installed.binDir, name);
    assert.equal(fs.readFileSync(binaryPath, "utf8"), `package:${packageName}@1.2.0:${name}`);
    if (platform !== "win32") {
      assert.ok(fs.statSync(binaryPath).mode & 0o111, `${name} should be executable`);
    }
  }
  assert.deepEqual(
    fs.readdirSync(cacheRoot).filter((entry) => entry.startsWith(".staging-")),
    []
  );

  ensureLatestNativeBinaries({ platform, arch, cacheRoot, runNpm });
  assert.equal(calls.filter((call) => call.args[0] === "install").length, 1);
});

test("native release rejects an npm native package with missing binaries", (context) => {
  const cacheRoot = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-release-"));
  context.after(() => fs.rmSync(cacheRoot, { recursive: true, force: true }));
  const { runNpm } = fakeNpmRegistry("1.3.0", "linux", BINARY_NAMES.slice(0, 2));

  assert.throws(
    () => ensureLatestNativeBinaries({ platform: "linux", arch: "x64", cacheRoot, runNpm }),
    /@wattetheria\/native-linux-x64@1\.3\.0 is missing wattswarm\./
  );
  assert.equal(currentNativeBinDir({ platform: "linux", arch: "x64", cacheRoot }), "");
  assert.equal(fs.existsSync(path.join(cacheRoot, "1.3.0")), false);
});

test("native release reports npm registry failures", () => {
  const runNpm = () => ({ status: 1, stdout: "", stderr: "npm error 404 Not Found" });

  assert.throws(
    () => ensureLatestNativeBinaries({ platform: "linux", arch: "x64", cacheRoot: os.tmpdir(), runNpm }),
    /Failed to query the latest @wattetheria\/native-linux-x64 version from npm: npm error 404 Not Found/
  );
});

test("native release rejects unsupported platform targets", () => {
  assert.throws(
    () => ensureLatestNativeBinaries({ platform: "freebsd", arch: "x64", runNpm: () => {} }),
    /Native runtime is not available for freebsd-x64/
  );
});

test("native staging names release assets and fills the native release package", (context) => {
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
  const releasePackageDirectory = path.join(root, "npm", "native-release", "linux-x64");
  fs.mkdirSync(releasePackageDirectory, { recursive: true });
  fs.writeFileSync(path.join(releasePackageDirectory, "package.json"), JSON.stringify({
    name: "@wattetheria/native-linux-x64"
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
    assert.equal(fs.readFileSync(path.join(releasePackageDirectory, "bin", name), "utf8"), `binary-${index}`);
  }
});

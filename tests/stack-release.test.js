"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

const generator = path.join(__dirname, "..", "scripts", "generate-stack-release.js");

function runWithMissingManifest(context, downloadStatus) {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "watt-stack-release-test-"));
  context.after(() => fs.rmSync(tempDir, { recursive: true, force: true }));
  const preload = path.join(tempDir, "mock-gh.js");
  const outDir = path.join(tempDir, "output");

  fs.writeFileSync(
    preload,
    `const childProcess = require("node:child_process");
const originalSpawnSync = childProcess.spawnSync;
childProcess.spawnSync = function (command, args, options) {
  if (command === "gh" && args[0] === "release" && args[1] === "list") {
    return { status: 0, stdout: '[{"tagName":"v1.1.2"},{"tagName":"v1.1.1"}]', stderr: "" };
  }
  if (command === "gh" && args[0] === "release" && args[1] === "download") {
    return { status: ${downloadStatus}, stdout: "", stderr: "manifest unavailable" };
  }
  return originalSpawnSync(command, args, options);
};
`,
  );

  const result = spawnSync(process.execPath, ["--require", preload, generator], {
    encoding: "utf8",
    env: {
      ...process.env,
      RELEASE: "1.1.2",
      PREVIOUS_STACK_MANIFEST: "",
      STACK_RELEASE_SKIP_GITHUB: "0",
      STACK_RELEASE_OUT_DIR: outDir,
    },
  });

  assert.notEqual(result.status, 0, result.stdout);
  assert.equal(fs.existsSync(outDir), false, "release outputs must not be written");
  return result.stderr;
}

test("stack release stops when the previous manifest download fails", (context) => {
  const stderr = runWithMissingManifest(context, 1);
  assert.match(stderr, /gh release download v1\.1\.1 .* failed/);
});

test("stack release stops when download succeeds without a manifest asset", (context) => {
  const stderr = runWithMissingManifest(context, 0);
  assert.match(stderr, /no release manifest asset found on v1\.1\.1/);
});

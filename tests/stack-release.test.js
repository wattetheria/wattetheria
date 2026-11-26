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

function makeCheckout(workspace, name) {
  const repoPath = path.join(workspace, name);
  fs.mkdirSync(repoPath, { recursive: true });
  const git = (...args) => {
    const result = spawnSync("git", args, { cwd: repoPath, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  };
  git("init", "--quiet");
  git("-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "--quiet", "--allow-empty", "-m", "init");
}

test("stack release covers only the open-source repositories", (context) => {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "watt-stack-release-test-"));
  context.after(() => fs.rmSync(workspace, { recursive: true, force: true }));
  makeCheckout(workspace, "wattetheria");
  makeCheckout(workspace, "wattswarm");
  const outDir = path.join(workspace, "output");

  const result = spawnSync(process.execPath, [generator], {
    encoding: "utf8",
    env: {
      ...process.env,
      RELEASE: "9.9.9",
      PREVIOUS_STACK_MANIFEST: "",
      STACK_RELEASE_SKIP_GITHUB: "1",
      STACK_RELEASE_WORKSPACE: workspace,
      STACK_RELEASE_OUT_DIR: outDir,
    },
  });
  assert.equal(result.status, 0, result.stderr);

  const manifest = JSON.parse(
    fs.readFileSync(path.join(outDir, "release-manifest.9.9.9.json"), "utf8"),
  );
  assert.deepEqual(
    manifest.components.map((component) => component.repo),
    ["wattetheria/wattetheria", "wattetheria/wattswarm"],
  );

  const notes = fs.readFileSync(path.join(outDir, "release-notes.md"), "utf8");
  const headings = notes.match(/^### .*$/gm);
  assert.deepEqual(headings, ["### wattetheria", "### wattswarm"]);
});

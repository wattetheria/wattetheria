#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const ROOT_DIR = path.resolve(__dirname, "..");
const ROOT_PACKAGE_PATH = path.join(ROOT_DIR, "package.json");
const NATIVE_PACKAGE_DIR = path.join(ROOT_DIR, "npm", "native");
const VERSION_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
const NATIVE_PACKAGE_PREFIX = "@wattetheria/cli-";
const REQUIRE_UNPUBLISHED =
  process.env.NPM_CLI_REQUIRE_UNPUBLISHED === "1" ||
  process.env.NPM_CLI_REQUIRE_UNPUBLISHED === "true";

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd || ROOT_DIR,
    encoding: "utf8",
    env: process.env,
  });

  if (result.status !== 0 && !options.allowFailure) {
    const details = [result.stdout, result.stderr].filter(Boolean).join("\n").trim();
    throw new Error(`${command} ${args.join(" ")} failed${details ? `\n${details}` : ""}`);
  }

  return {
    ok: result.status === 0,
    stdout: result.stdout || "",
    stderr: result.stderr || "",
  };
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function normalizeVersion(value) {
  const version = String(value || "").trim().replace(/^v/, "");
  if (!VERSION_RE.test(version)) {
    throw new Error(`invalid npm version: ${value}`);
  }
  return version;
}

function parseStableVersion(value) {
  const version = normalizeVersion(value);
  const [core] = version.split("-");
  return core.split(".").map((part) => Number(part));
}

function bumpVersion(version, bump) {
  const [major, minor, patch] = parseStableVersion(version);

  if (bump === "major") {
    return `${major + 1}.0.0`;
  }
  if (bump === "minor") {
    return `${major}.${minor + 1}.0`;
  }
  if (bump === "patch") {
    return `${major}.${minor}.${patch + 1}`;
  }

  throw new Error(`unsupported version bump: ${bump}`);
}

function npmViewVersion(packageName) {
  if (process.env.NPM_CLI_RELEASE_LATEST) {
    return normalizeVersion(process.env.NPM_CLI_RELEASE_LATEST);
  }

  const result = run("npm", ["view", packageName, "version", "--json"]);
  return normalizeVersion(JSON.parse(result.stdout));
}

function resolveReleaseVersion(rootPackage) {
  const explicitVersion = process.env.NPM_CLI_RELEASE_VERSION || process.env.RELEASE_VERSION;
  if (explicitVersion) {
    return {
      version: normalizeVersion(explicitVersion),
      source: "workflow input",
      latest: null,
    };
  }

  const latest = npmViewVersion(rootPackage.name);
  const bump = process.env.NPM_CLI_VERSION_BUMP || "patch";
  return {
    version: bumpVersion(latest, bump),
    source: `npm latest ${latest} + ${bump}`,
    latest,
  };
}

function nativePackagePaths() {
  return fs
    .readdirSync(NATIVE_PACKAGE_DIR)
    .map((entry) => path.join(NATIVE_PACKAGE_DIR, entry, "package.json"))
    .filter((manifestPath) => fs.existsSync(manifestPath))
    .sort();
}

function updateRootPackage(rootPackage, version, nativePackageNames) {
  rootPackage.version = version;
  rootPackage.optionalDependencies = rootPackage.optionalDependencies || {};

  for (const packageName of nativePackageNames) {
    rootPackage.optionalDependencies[packageName] = version;
  }
}

function updateNativePackages(version) {
  const packageNames = [];

  for (const manifestPath of nativePackagePaths()) {
    const manifest = readJson(manifestPath);
    if (!manifest.name || !manifest.name.startsWith(NATIVE_PACKAGE_PREFIX)) {
      throw new Error(`unexpected native package name in ${manifestPath}: ${manifest.name}`);
    }
    manifest.version = version;
    writeJson(manifestPath, manifest);
    packageNames.push(manifest.name);
  }

  return packageNames.sort();
}

function requireUnpublishedVersion(packageNames, version) {
  if (!REQUIRE_UNPUBLISHED) {
    return;
  }

  for (const packageName of packageNames) {
    const result = run("npm", ["view", `${packageName}@${version}`, "version", "--json"], {
      allowFailure: true,
    });
    if (result.ok) {
      throw new Error(`${packageName}@${version} is already published`);
    }
  }
}

function writeGithubOutput(values) {
  if (!process.env.GITHUB_OUTPUT) {
    return;
  }

  const lines = Object.entries(values).map(([key, value]) => `${key}=${value}`);
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `${lines.join("\n")}\n`);
}

function main() {
  const rootPackage = readJson(ROOT_PACKAGE_PATH);
  const release = resolveReleaseVersion(rootPackage);
  const nativePackageNames = updateNativePackages(release.version);
  requireUnpublishedVersion([rootPackage.name, ...nativePackageNames], release.version);
  updateRootPackage(rootPackage, release.version, nativePackageNames);
  writeJson(ROOT_PACKAGE_PATH, rootPackage);

  writeGithubOutput({
    version: release.version,
    source: release.source,
    latest: release.latest || "",
  });

  console.log(`prepared npm CLI release version ${release.version} (${release.source})`);
  console.log(`updated ${ROOT_PACKAGE_PATH}`);
  for (const packageName of nativePackageNames) {
    console.log(`updated ${packageName} -> ${release.version}`);
  }
}

try {
  main();
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exit(1);
}

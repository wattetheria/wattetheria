#!/usr/bin/env node

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const ROOT_DIR = path.resolve(__dirname, "..");
const DEFAULT_NPM_VIEW_ATTEMPTS = 1;
const DEFAULT_NPM_VIEW_RETRY_DELAY_MS = 10_000;

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function positiveIntegerEnv(name, fallback) {
  const raw = process.env[name];
  if (!raw) {
    return fallback;
  }
  const value = Number.parseInt(raw, 10);
  return Number.isFinite(value) && value > 0 ? value : fallback;
}

function readRootPackage() {
  return JSON.parse(fs.readFileSync(path.join(ROOT_DIR, "package.json"), "utf8"));
}

function npmView(packageName, version) {
  return spawnSync("npm", ["view", `${packageName}@${version}`, "version"], {
    cwd: ROOT_DIR,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"]
  });
}

function npmPackageVersionExists(packageName, version) {
  const attempts = positiveIntegerEnv("WATTETHERIA_NPM_VIEW_ATTEMPTS", DEFAULT_NPM_VIEW_ATTEMPTS);
  const retryDelayMs = positiveIntegerEnv(
    "WATTETHERIA_NPM_VIEW_RETRY_DELAY_MS",
    DEFAULT_NPM_VIEW_RETRY_DELAY_MS
  );

  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const result = npmView(packageName, version);
    if (result.status === 0 && result.stdout.trim() === version) {
      return true;
    }
    if (attempt < attempts) {
      sleep(retryDelayMs);
    }
  }
  return false;
}

function checkNativePackages() {
  const rootPackage = readRootPackage();
  const optionalDependencies = rootPackage.optionalDependencies || {};
  const entries = Object.entries(optionalDependencies)
    .filter(([name]) => name.startsWith("@wattetheria/cli-"));

  if (entries.length === 0) {
    throw new Error("No Wattetheria native CLI optionalDependencies are configured.");
  }

  const missing = [];
  for (const [name, version] of entries) {
    if (version !== rootPackage.version) {
      missing.push(`${name}@${version} (expected ${rootPackage.version})`);
      continue;
    }
    if (!npmPackageVersionExists(name, version)) {
      missing.push(`${name}@${version}`);
    }
  }

  if (missing.length > 0) {
    throw new Error([
      "Native CLI packages must be published before publishing the main wattetheria package.",
      "Missing from npm registry:",
      ...missing.map((name) => `  - ${name}`),
      "Publish each native package first, then rerun npm publish --access public."
    ].join("\n"));
  }

  console.log(`verified ${entries.length} native CLI optional packages on npm`);
}

try {
  if (process.env.WATTETHERIA_SKIP_NATIVE_PACKAGE_CHECK === "1") {
    console.log("skipping native package registry check");
  } else {
    checkNativePackages();
  }
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exit(1);
}

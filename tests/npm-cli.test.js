"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

const ROOT_DIR = path.resolve(__dirname, "..");
const CLI_PATH = path.join(ROOT_DIR, "bin", "wattetheria.js");

function runCli(args) {
  return spawnSync(process.execPath, [CLI_PATH, ...args], {
    cwd: ROOT_DIR,
    encoding: "utf8",
    env: { ...process.env, WATTETHERIA_NO_BANNER: "1" },
  });
}

test("help separates network commands and lists all subcommands", () => {
  const result = runCli(["help"]);

  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.stdout, /Agent subcommands:/);
  assert.doesNotMatch(result.stdout, /^\s+identity\s*$/m);
  assert.doesNotMatch(result.stdout, /^\s+servicenet\s*$/m);
  const generalCommands = result.stdout.match(/Commands:\n([\s\S]*?)\nNetwork:/);
  assert.ok(generalCommands);
  assert.doesNotMatch(generalCommands[1], /^\s+network\s/m);

  const registrationSection = result.stdout.match(/Network:\n([\s\S]*?)\nOptions:/);
  assert.ok(registrationSection);
  const subcommands = [...registrationSection[1].matchAll(/^    ([a-z][a-z-]+)\s+/gm)]
    .map((match) => match[1]);
  assert.deepEqual(subcommands, [
    "authority-init",
    "authority-show",
    "create-request",
    "inspect-request",
    "export-trust-bundle",
    "issue-credential",
    "verify-credential",
    "import-credential",
    "list-credentials",
    "revoke-credential",
    "verify-revocation",
    "import-revocation",
  ]);
});

test(
  "network forwards to the native node CLI",
  { skip: process.platform === "win32" && "uses a POSIX executable shim" },
  (context) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-test-"));
    const marker = path.join(directory, "args.txt");
    const binary = path.join(directory, "wattetheria-client-cli");
    fs.writeFileSync(
      binary,
      `#!/bin/sh\nif [ "$1" = "--help" ]; then exit 0; fi\nprintf '%s\\n' "$@" > '${marker}'\n`
    );
    fs.chmodSync(binary, 0o755);
    context.after(() => fs.rmSync(directory, { recursive: true, force: true }));

    const result = spawnSync(
      process.execPath,
      [
        CLI_PATH,
        "network",
        "--data-dir",
        "/tmp/wattetheria",
        "authority-show",
      ],
      {
        cwd: ROOT_DIR,
        encoding: "utf8",
        env: {
          ...process.env,
          WATTETHERIA_CLI_BIN: binary,
          WATTETHERIA_NO_BANNER: "1",
        },
      }
    );

    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(fs.readFileSync(marker, "utf8").trim().split(/\r?\n/), [
      "network",
      "--data-dir",
      "/tmp/wattetheria",
      "authority-show",
    ]);
  }
);

const removedCommands = [
  { args: ["identity"], error: /Unknown command: identity/ },
  { args: ["servicenet"], error: /Unknown command: servicenet/ },
  { args: ["register"], error: /Unknown command: register/ },
  { args: ["network-registration"], error: /Unknown command: network-registration/ },
  { args: ["provider", "register"], error: /Unknown option: register/ },
];

for (const { args, error } of removedCommands) {
  const command = args.join(" ");
  test(`${command} is not exposed by the npm CLI`, () => {
    const result = runCli(args);

    assert.equal(result.status, 1);
    assert.match(result.stderr, error);
    assert.doesNotMatch(result.stderr, /wattetheria servicenet/);
  });
}

function fakeCommandDirectory(version, npmMarker, dockerMarker) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-npm-test-"));
  if (process.platform === "win32") {
    fs.writeFileSync(
      path.join(directory, "npm.cmd"),
      `@echo invoked>>"${npmMarker}"\r\n@echo ${version}\r\n`
    );
    fs.writeFileSync(
      path.join(directory, "docker.cmd"),
      `@echo invoked>>"${dockerMarker}"\r\n@exit /b 0\r\n`
    );
  } else {
    const npmPath = path.join(directory, "npm");
    fs.writeFileSync(
      npmPath,
      `#!/bin/sh\nprintf 'invoked\\n' >> '${npmMarker}'\nprintf '%s\\n' '${version}'\n`
    );
    fs.chmodSync(npmPath, 0o755);
    const dockerPath = path.join(directory, "docker");
    fs.writeFileSync(
      dockerPath,
      `#!/bin/sh\nprintf 'invoked\\n' >> '${dockerMarker}'\nexit 0\n`
    );
    fs.chmodSync(dockerPath, 0o755);
  }
  return directory;
}

function invocationCount(markerPath) {
  if (!fs.existsSync(markerPath)) {
    return 0;
  }
  return fs.readFileSync(markerPath, "utf8").trim().split(/\r?\n/).length;
}

for (const command of ["setup", "install", "update"]) {
  test(`${command} rejects an outdated npm CLI`, (context) => {
    const markerDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-markers-"));
    const npmMarker = path.join(markerDirectory, "npm");
    const dockerMarker = path.join(markerDirectory, "docker");
    const npmDirectory = fakeCommandDirectory("999.0.0", npmMarker, dockerMarker);
    context.after(() => fs.rmSync(npmDirectory, { recursive: true, force: true }));
    context.after(() => fs.rmSync(markerDirectory, { recursive: true, force: true }));

    const result = spawnSync(process.execPath, [CLI_PATH, command], {
      cwd: ROOT_DIR,
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: npmDirectory,
        WATTETHERIA_NO_BANNER: "1",
      },
    });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /Wattetheria CLI is outdated/);
    assert.match(result.stderr, new RegExp(`wattetheria ${command}`));
    assert.equal(invocationCount(npmMarker), 1);
    if (process.platform !== "win32") {
      assert.equal(invocationCount(dockerMarker), 0);
      assert.doesNotMatch(`${result.stdout}\n${result.stderr}`, /Docker/);
    }
  });
}

test(
  "setup checks the CLI version once when it delegates to install",
  { skip: process.platform === "win32" && "requires an executable Docker test shim" },
  (context) => {
    const testDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-setup-test-"));
    const npmMarker = path.join(testDirectory, "npm");
    const dockerMarker = path.join(testDirectory, "docker");
    const commandDirectory = fakeCommandDirectory(
      require("../package.json").version,
      npmMarker,
      dockerMarker
    );
    const deploymentDirectory = path.join(testDirectory, "deployment");
    context.after(() => fs.rmSync(commandDirectory, { recursive: true, force: true }));
    context.after(() => fs.rmSync(testDirectory, { recursive: true, force: true }));

    const result = spawnSync(
      process.execPath,
      [
        CLI_PATH,
        "setup",
        "--dir",
        deploymentDirectory,
        "--tag",
        "test",
        "--no-health-checks",
      ],
      {
        cwd: ROOT_DIR,
        encoding: "utf8",
        env: {
          ...process.env,
          PATH: `${commandDirectory}${path.delimiter}${process.env.PATH || ""}`,
          WATTETHERIA_NO_BANNER: "1",
        },
      }
    );

    assert.equal(result.status, 0, result.stderr);
    assert.equal(invocationCount(npmMarker), 1);
    assert.ok(invocationCount(dockerMarker) > 0);
  }
);

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
  const generalCommands = result.stdout.match(/Commands:\n([\s\S]*?)\nOptions:/);
  assert.ok(generalCommands);
  assert.doesNotMatch(generalCommands[1], /^\s+network\s/m);

  const registrationSection = result.stdout.match(/Network:\n([\s\S]*)$/);
  assert.ok(registrationSection);
  assert.ok(result.stdout.indexOf("Options:") < result.stdout.indexOf("Network:"));
  assert.match(registrationSection[1], /\n  Commands:\n/);
  assert.doesNotMatch(registrationSection[1], /Subcommands:/);
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
    assert.match(
      fs.readFileSync(path.join(deploymentDirectory, ".env"), "utf8"),
      /^WATTETHERIA_DEPLOYMENT_RUNTIME=docker$/m
    );
  }
);

test("deployment runtime is read from the deployment env", (context) => {
  const deploymentDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-runtime-test-"));
  context.after(() => fs.rmSync(deploymentDirectory, { recursive: true, force: true }));
  const envPath = path.join(deploymentDirectory, ".env");
  const version = () => runCli(["version", "--dir", deploymentDirectory]);

  fs.writeFileSync(envPath, "WATTETHERIA_DEPLOYMENT_RUNTIME=native\n");
  assert.match(version().stdout, /\(native\)/);

  fs.writeFileSync(envPath, "WATTETHERIA_DEPLOYMENT_RUNTIME=docker\nWATTETHERIA_KERNEL_IMAGE=ghcr.io/wattetheria/wattetheria-kernel:1.2.3\n");
  assert.doesNotMatch(version().stdout, /native/);

  fs.writeFileSync(envPath, "WATTETHERIA_KERNEL_IMAGE=ghcr.io/wattetheria/wattetheria-kernel:1.2.3\n");
  assert.doesNotMatch(version().stdout, /native/);

  fs.writeFileSync(envPath, "WATTETHERIA_DEPLOYMENT_RUNTIME=podman\n");
  const invalid = version();
  assert.equal(invalid.status, 1);
  assert.match(invalid.stderr, /Invalid WATTETHERIA_DEPLOYMENT_RUNTIME=podman in deployment env/);
});

test("help documents the native runtime option", () => {
  const result = runCli(["help"]);

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /--runtime <name>\s+With install or setup: docker \(default\) or native/);
});

test("install rejects an unknown runtime", () => {
  const result = runCli(["install", "--runtime", "podman"]);

  assert.equal(result.status, 1);
  assert.match(result.stderr, /Invalid --runtime: podman\. Use docker or native\./);
});

test("native kernel flags mirror the Docker kernel entrypoint", () => {
  const { kernelArgs } = require("../lib/native");
  const env = new Map([
    ["WATTETHERIA_CONTROL_PLANE_BIND_HOST", "0.0.0.0"],
    ["WATTETHERIA_CONTROL_PLANE_PORT", "17777"],
    ["WATTSWARM_UI_PORT", "17788"],
    ["WATTSWARM_SYNC_GRPC_PORT", "17791"],
    ["WATTETHERIA_AUTONOMY_ENABLED", "true"],
    ["WATTETHERIA_BRAIN_PROVIDER_KIND", "openai-compatible"],
    ["WATTETHERIA_BRAIN_MODEL", ""],
    ["WATTETHERIA_MCP_TOKEN_AUTH", "true"],
    ["WATTETHERIA_GATEWAY_URLS", "https://a.example, ,https://b.example"],
  ]);

  const args = kernelArgs({
    env,
    wattetheriaDataDir: "/state/wattetheria",
    wattswarmStateDir: "/state/wattswarm",
  });

  const flag = (name) => args[args.indexOf(name) + 1];
  assert.equal(flag("--data-dir"), "/state/wattetheria");
  assert.equal(flag("--control-plane-bind"), "0.0.0.0:17777");
  assert.equal(flag("--wattswarm-agent-event-callback-base-url"), "http://127.0.0.1:17777");
  assert.equal(flag("--wattswarm-ui-base-url"), "http://127.0.0.1:17788");
  assert.equal(flag("--wattswarm-sync-grpc-endpoint"), "http://127.0.0.1:17791");
  assert.equal(flag("--agent-host-data-dir"), "/state/wattetheria");
  assert.equal(flag("--gateway-config-path"), path.join("/state/wattswarm", "startup_config.json"));
  assert.equal(flag("--brain-provider-kind"), "openai-compatible");
  assert.ok(args.includes("--autonomy-enabled"));
  assert.ok(args.includes("--mcp-token-auth-required"));
  assert.ok(!args.includes("--brain-model"));
  assert.deepEqual(
    args.flatMap((value, index) => (value === "--gateway-url" ? [args[index + 1]] : [])),
    ["https://a.example", "https://b.example"]
  );
});

test("orphan process identity matches the deployment arguments, not only the binary", () => {
  const { commandLineMatchesIdentity } = require("../lib/native");
  const commandLine = '"C:\\Program Files\\Watt\\wattswarm.exe" --state-dir "C:\\Users\\PVer\\.wattetheria\\deploy A\\data\\wattswarm" --store wattswarm.db ui';

  assert.equal(commandLineMatchesIdentity(commandLine, [
    "--state-dir",
    "C:\\Users\\PVer\\.wattetheria\\deploy A\\data\\wattswarm"
  ]), true);
  assert.equal(commandLineMatchesIdentity(commandLine, [
    "--state-dir",
    "C:\\Users\\PVer\\.wattetheria\\deploy B\\data\\wattswarm"
  ]), false);
  assert.equal(commandLineMatchesIdentity(commandLine, [
    "--state-dir",
    "C:\\Users\\PVer\\.wattetheria\\deploy A"
  ]), false);
  if (process.platform !== "win32") {
    assert.equal(commandLineMatchesIdentity("wattswarm --state-dir /srv/Deploy/data", [
      "--state-dir",
      "/srv/deploy/data"
    ]), false);
  }
  assert.equal(commandLineMatchesIdentity(commandLine, ["--listen", "127.0.0.1:8788"]), false);
});

function freePort() {
  const net = require("node:net");
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

function writeFakeNativeBinaries(binDirectory, markerPath) {
  fs.mkdirSync(binDirectory, { recursive: true });
  const kernel = path.join(binDirectory, "wattetheria-kernel");
  fs.writeFileSync(kernel, "#!/bin/sh\nexec sleep 300\n");
  const wattswarm = path.join(binDirectory, "wattswarm");
  fs.writeFileSync(
    wattswarm,
    [
      "#!/bin/sh",
      `printf '%s\\n' "$*" >> '${markerPath}'`,
      'case "$*" in *" run init"*|*" executors add "*) exit 0 ;; esac',
      "exec sleep 300",
      "",
    ].join("\n")
  );
  const runtime = path.join(binDirectory, "wattswarm-runtime");
  fs.writeFileSync(
    runtime,
    [
      `#!${process.execPath}`,
      'const [host, port] = process.argv[process.argv.indexOf("--listen") + 1].split(":");',
      'require("node:http").createServer((_req, res) => res.end("ok")).listen(Number(port), host);',
      "",
    ].join("\n")
  );
  for (const binary of [kernel, wattswarm, runtime]) {
    fs.chmodSync(binary, 0o755);
  }
}

test(
  "native install supervises local binaries without Docker",
  { skip: process.platform === "win32" && "uses POSIX executable shims" },
  async (context) => {
    const testDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-test-"));
    const binDirectory = path.join(testDirectory, "bin");
    const deploymentDirectory = path.join(testDirectory, "deployment");
    const wattswarmMarker = path.join(testDirectory, "wattswarm-args");
    const dockerMarker = path.join(testDirectory, "docker");
    const commandDirectory = fakeCommandDirectory(
      require("../package.json").version,
      path.join(testDirectory, "npm"),
      dockerMarker
    );
    writeFakeNativeBinaries(binDirectory, wattswarmMarker);
    fs.mkdirSync(deploymentDirectory, { recursive: true });
    fs.writeFileSync(
      path.join(deploymentDirectory, ".env"),
      `WATTETHERIA_DEPLOYMENT_RUNTIME=native\nWATTSWARM_RUNTIME_PORT=${await freePort()}\n`
    );
    const env = {
      ...process.env,
      PATH: `${commandDirectory}${path.delimiter}${process.env.PATH || ""}`,
      WATTETHERIA_NATIVE_BIN_DIR: binDirectory,
      WATTETHERIA_NO_BANNER: "1",
    };
    const cli = (...args) => spawnSync(
      process.execPath,
      [CLI_PATH, ...args, "--dir", deploymentDirectory],
      { cwd: ROOT_DIR, encoding: "utf8", env }
    );
    context.after(() => {
      cli("stop");
      fs.rmSync(commandDirectory, { recursive: true, force: true });
      fs.rmSync(testDirectory, { recursive: true, force: true });
    });

    const install = cli("install", "--runtime", "native", "--no-health-checks");
    assert.equal(install.status, 0, install.stderr);
    assert.match(install.stdout, /Native supervisor started/);

    const deploymentEnv = fs.readFileSync(path.join(deploymentDirectory, ".env"), "utf8");
    assert.match(deploymentEnv, /^WATTETHERIA_DEPLOYMENT_RUNTIME=native$/m);
    assert.match(deploymentEnv, /^WATTSWARM_STORAGE_BACKEND=sqlite$/m);
    assert.doesNotMatch(deploymentEnv, /WATTSWARM_PG_/);

    let status;
    for (let attempt = 0; attempt < 50; attempt += 1) {
      status = cli("status");
      if ((status.stdout.match(/^\S+\s+running\s+\d+/gm) || []).length === 4) {
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    assert.equal(status.status, 0, status.stderr);
    for (const service of ["wattswarm-runtime", "wattswarm-kernel", "wattswarm-worker", "kernel"]) {
      assert.match(status.stdout, new RegExp(`^${service}\\s+running\\s+\\d+`, "m"));
    }
    const nativeState = JSON.parse(
      fs.readFileSync(path.join(deploymentDirectory, "run", "state.json"), "utf8")
    );
    assert.deepEqual(nativeState.services["wattswarm-kernel"].identityArgs, [
      "--state-dir",
      path.join(deploymentDirectory, "data", "wattswarm")
    ]);
    assert.deepEqual(nativeState.services.kernel.identityArgs, [
      "--data-dir",
      path.join(deploymentDirectory, "data", "wattetheria")
    ]);

    const wattswarmCalls = fs.readFileSync(wattswarmMarker, "utf8");
    assert.match(wattswarmCalls, /--store wattswarm\.db run init/);
    assert.match(wattswarmCalls, /executors add rt http:\/\/127\.0\.0\.1:\d+/);
    assert.match(wattswarmCalls, /run worker --concurrency 16/);
    assert.ok(fs.existsSync(path.join(deploymentDirectory, "data", "wattetheria", "control.token")));

    const mismatch = cli("install", "--runtime", "docker");
    assert.equal(mismatch.status, 1);
    assert.match(mismatch.stderr, /A native deployment already exists/);

    const stop = cli("stop");
    assert.equal(stop.status, 0, stop.stderr);
    assert.match(stop.stdout, /Native services stopped\./);
    assert.match(cli("status").stdout, /Native supervisor: stopped/);
    assert.equal(invocationCount(dockerMarker), 0);
  }
);

test(
  "native supervisor restarts a service whose binary fails to start",
  { skip: process.platform === "win32" && "uses POSIX executable shims" },
  async (context) => {
    const testDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-test-"));
    const binDirectory = path.join(testDirectory, "bin");
    const deploymentDirectory = path.join(testDirectory, "deployment");
    const commandDirectory = fakeCommandDirectory(
      require("../package.json").version,
      path.join(testDirectory, "npm"),
      path.join(testDirectory, "docker")
    );
    writeFakeNativeBinaries(binDirectory, path.join(testDirectory, "wattswarm-args"));
    fs.chmodSync(path.join(binDirectory, "wattetheria-kernel"), 0o644);
    fs.mkdirSync(deploymentDirectory, { recursive: true });
    fs.writeFileSync(
      path.join(deploymentDirectory, ".env"),
      `WATTETHERIA_DEPLOYMENT_RUNTIME=native\nWATTSWARM_RUNTIME_PORT=${await freePort()}\n`
    );
    const env = {
      ...process.env,
      PATH: `${commandDirectory}${path.delimiter}${process.env.PATH || ""}`,
      WATTETHERIA_NATIVE_BIN_DIR: binDirectory,
      WATTETHERIA_NO_BANNER: "1",
    };
    const cli = (...args) => spawnSync(
      process.execPath,
      [CLI_PATH, ...args, "--dir", deploymentDirectory],
      { cwd: ROOT_DIR, encoding: "utf8", env }
    );
    context.after(() => {
      cli("stop");
      fs.rmSync(commandDirectory, { recursive: true, force: true });
      fs.rmSync(testDirectory, { recursive: true, force: true });
    });

    const install = cli("install", "--runtime", "native", "--no-health-checks");
    assert.equal(install.status, 0, install.stderr);

    const kernelRow = () => cli("status").stdout.match(/^kernel\s+(\S+)\s+(\S+)\s+(\d+)/m);
    let row;
    for (let attempt = 0; attempt < 50; attempt += 1) {
      row = kernelRow();
      if (row && Number(row[3]) >= 2) {
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    assert.ok(row, "kernel status row is missing");
    assert.equal(row[1], "restarting");
    assert.equal(row[2], "-");
    assert.ok(Number(row[3]) >= 2, `expected repeated restart attempts, got ${row[3]}`);

    const state = JSON.parse(
      fs.readFileSync(path.join(deploymentDirectory, "run", "state.json"), "utf8")
    );
    assert.equal(state.services.kernel.lastExit, "EACCES");
    const daemonLog = fs.readFileSync(path.join(deploymentDirectory, "logs", "daemon.log"), "utf8");
    assert.match(daemonLog, /kernel failed to start: spawn .* EACCES/);
    assert.doesNotMatch(daemonLog, /kernel started \(pid undefined\)/);

    const stop = cli("stop");
    assert.equal(stop.status, 0, stop.stderr);
    assert.match(stop.stdout, /Native services stopped\./);
  }
);

function sandboxedCli(context) {
  const homeDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-home-test-"));
  const dockerMarker = path.join(homeDirectory, "docker-invocations");
  const commandDirectory = fakeCommandDirectory(
    require("../package.json").version,
    path.join(homeDirectory, "npm-invocations"),
    dockerMarker
  );
  const deploymentDirectory = path.join(homeDirectory, ".wattetheria", "deploy");
  context.after(() => {
    fs.rmSync(commandDirectory, { recursive: true, force: true });
    fs.rmSync(homeDirectory, { recursive: true, force: true });
  });
  const cli = (...args) => spawnSync(
    process.execPath,
    [CLI_PATH, ...args, "--dir", deploymentDirectory],
    {
      cwd: ROOT_DIR,
      encoding: "utf8",
      env: {
        ...process.env,
        HOME: homeDirectory,
        USERPROFILE: homeDirectory,
        PATH: `${commandDirectory}${path.delimiter}${process.env.PATH || ""}`,
        WATTETHERIA_NO_BANNER: "1",
      },
    }
  );
  return { cli, homeDirectory, deploymentDirectory, dockerMarker };
}

test(
  "uninstall --purge treats native leftovers without an env file as native",
  { skip: process.platform === "win32" && "requires an executable Docker test shim" },
  (context) => {
    const { cli, homeDirectory, deploymentDirectory, dockerMarker } = sandboxedCli(context);
    fs.mkdirSync(path.join(deploymentDirectory, "logs"), { recursive: true });
    fs.mkdirSync(path.join(deploymentDirectory, "run"), { recursive: true });
    fs.writeFileSync(path.join(deploymentDirectory, "logs", "kernel.log"), "log\n");

    const result = cli("uninstall", "--volumes", "--purge");

    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(`${result.stdout}${result.stderr}`, /PostgreSQL|compose/i);
    assert.match(result.stdout, /Native services are not running\./);
    assert.equal(fs.existsSync(path.join(homeDirectory, ".wattetheria")), false);
    assert.equal(invocationCount(dockerMarker), 0);
  }
);

test(
  "uninstall --purge without any deployment only removes the home directory",
  { skip: process.platform === "win32" && "requires an executable Docker test shim" },
  (context) => {
    const { cli, homeDirectory, dockerMarker } = sandboxedCli(context);
    fs.mkdirSync(path.join(homeDirectory, ".wattetheria"), { recursive: true });

    const result = cli("uninstall", "--purge");

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /No Wattetheria deployment found/);
    assert.doesNotMatch(`${result.stdout}${result.stderr}`, /PostgreSQL|compose/i);
    assert.equal(fs.existsSync(path.join(homeDirectory, ".wattetheria")), false);
    assert.equal(invocationCount(dockerMarker), 0);
  }
);

for (const command of ["start", "stop", "status", "logs", "restart"]) {
  test(
    `${command} without a deployment asks for install instead of checking Docker`,
    { skip: process.platform === "win32" && "requires an executable Docker test shim" },
    (context) => {
      const { cli, dockerMarker } = sandboxedCli(context);

      const result = cli(command);

      assert.equal(result.status, 1);
      assert.match(result.stderr, /No Wattetheria deployment found .* Run `wattetheria install` first\./);
      assert.equal(invocationCount(dockerMarker), 0);
    }
  );
}

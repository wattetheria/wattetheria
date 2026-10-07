"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");
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

function runAsyncCli(args, env = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [CLI_PATH, ...args], {
      cwd: ROOT_DIR, env: { ...process.env, WATTETHERIA_NO_BANNER: "1", ...env }
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("close", (status) => resolve({ status, stdout, stderr }));
  });
}

function publicMcpDeployment(context, runtime = "native") {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-public-mcp-"));
  const state = path.join(dir, "data", "wattetheria");
  fs.mkdirSync(state, { recursive: true });
  const envPath = path.join(dir, ".env");
  fs.writeFileSync(envPath, `WATTETHERIA_DEPLOYMENT_RUNTIME=${runtime}\nWATTETHERIA_HOST_STATE_DIR=./data/wattetheria\n`);
  const secret = require("node:crypto").randomBytes(32).toString("base64url");
  const secretPath = path.join(state, "mcp_url_secret");
  context.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return { dir, state, envPath, secret, secretPath };
}

test("public MCP CLI stays disabled without creating state or contacting Docker", (context) => {
  const { dir, secretPath } = publicMcpDeployment(context);
  for (const action of ["url", "rotate"]) {
    const result = runCli(["mcp", action, "--dir", dir]);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Public MCP is disabled/);
    assert.match(result.stdout, /WATTETHERIA_MCP_PUBLIC_BIND=0\.0\.0\.0:7778/);
    assert.equal(fs.existsSync(secretPath), false);
  }
});

test("public MCP CLI prints a full URL or the path with a configuration hint", (context) => {
  const { dir, envPath, secret, secretPath } = publicMcpDeployment(context);
  fs.writeFileSync(secretPath, secret, { mode: 0o600 });
  fs.appendFileSync(envPath, "WATTETHERIA_MCP_PUBLIC_BIND=0.0.0.0:7778\n");
  const withoutBase = runCli(["mcp", "url", "--dir", dir]);
  assert.equal(withoutBase.status, 0, withoutBase.stderr);
  assert.match(withoutBase.stdout, new RegExp(`^/mcp/${secret}\\n`));
  assert.match(withoutBase.stdout, /Configure WATTETHERIA_MCP_PUBLIC_BASE_URL/);
  fs.appendFileSync(envPath, "WATTETHERIA_MCP_PUBLIC_BASE_URL=https://node.example/prefix/\n");
  const complete = runCli(["mcp", "url", "--dir", dir]);
  assert.equal(complete.status, 0, complete.stderr);
  assert.equal(complete.stdout.trim(), `https://node.example/prefix/mcp/${secret}`);
});

test("public MCP rotation reads the existing token and uses the authenticated control endpoint", async (context) => {
  const { dir, state, envPath, secret, secretPath } = publicMcpDeployment(context);
  const token = "local-control-token";
  fs.writeFileSync(path.join(state, "control.token"), `${token}\n`, { mode: 0o600 });
  fs.appendFileSync(envPath, "WATTETHERIA_MCP_PUBLIC_BIND=127.0.0.1:7778\nWATTETHERIA_MCP_TOKEN_AUTH=false\nWATTETHERIA_MCP_PUBLIC_BASE_URL=https://node.example\n");
  let calls = 0;
  const server = require("node:http").createServer((request, response) => {
    calls += 1;
    assert.equal(request.method, "POST");
    assert.equal(request.url, "/v1/mcp/public-url/rotate");
    assert.equal(request.headers.authorization, `Bearer ${token}`);
    fs.writeFileSync(secretPath, secret, { mode: 0o600 });
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ path: `/mcp/${secret}`, revokedSubscriptions: 2 }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  context.after(() => new Promise((resolve) => server.close(resolve)));
  const result = await runAsyncCli(["mcp", "rotate", "--dir", dir, "--control-plane", `http://127.0.0.1:${server.address().port}`]);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(calls, 1);
  assert.match(result.stdout, new RegExp(`https://node.example/mcp/${secret}`));
  assert.match(result.stdout, /Update the MCP URL in your remote agent/);
  assert.match(result.stdout, /Subscribe again/);
  assert.doesNotMatch(result.stdout + result.stderr, new RegExp(token));
});

test("existing Compose stacks publish a configurable MCP port with an optional listener", () => {
  const port = "${WATTETHERIA_MCP_PUBLIC_BIND_HOST:-127.0.0.1}:${WATTETHERIA_MCP_PUBLIC_PORT:-7778}:7778";
  for (const file of ["docker-compose.yml", "docker-compose.full.yml", "docker-compose.release.yml"]) {
    const source = fs.readFileSync(path.join(ROOT_DIR, file), "utf8");
    assert.ok(source.includes(`- "${port}"`), `${file} must use the configurable MCP port mapping`);
    assert.doesNotMatch(source, /127\.0\.0\.1:7778:7778/);
    assert.match(source, /WATTETHERIA_MCP_PUBLIC_BIND:\s+\$\{WATTETHERIA_MCP_PUBLIC_BIND:-\}/);
  }
  assert.deepEqual(Object.keys(require("../lib/mcp-public")), ["run"]);
});

test("Compose MCP port mappings preserve loopback by default and honor explicit overrides", (context) => {
  if (spawnSync("docker", ["compose", "version"], { encoding: "utf8" }).status !== 0) {
    context.skip("requires Docker Compose; no container startup is needed");
    return;
  }
  const stacks = [
    ["main", ["docker-compose.yml"]],
    ["dev", ["docker-compose.yml", "docker-compose.dev.yml"]],
    ["full", ["docker-compose.full.yml"]],
    ["release", ["docker-compose.release.yml"]],
  ];
  for (const [name, files] of stacks) {
    for (const custom of [false, true]) {
      const env = { ...process.env, WATTSWARM_PG_PASSWORD: "compose-config-check", WATTETHERIA_COMPOSE_ENV_FILE: os.devNull };
      delete env.WATTETHERIA_MCP_PUBLIC_PORT;
      delete env.WATTETHERIA_MCP_PUBLIC_BIND_HOST;
      if (custom) {
        env.WATTETHERIA_MCP_PUBLIC_PORT = "18778";
        env.WATTETHERIA_MCP_PUBLIC_BIND_HOST = "0.0.0.0";
      }
      const result = spawnSync("docker", [
        "compose", "--env-file", os.devNull,
        ...files.flatMap((file) => ["-f", file]), "config", "--format", "json",
      ], { cwd: ROOT_DIR, encoding: "utf8", env });
      assert.equal(result.status, 0, result.stderr);
      const ports = JSON.parse(result.stdout).services.kernel.ports.filter((port) => port.target === 7778);
      assert.equal(ports.length, 1, `${name} must publish one MCP port`);
      assert.equal(ports[0].host_ip, custom ? "0.0.0.0" : "127.0.0.1", `${name} must preserve loopback unless explicitly overridden`);
      assert.equal(String(ports[0].published), custom ? "18778" : "7778", `${name} must honor the configured host port`);
    }
  }
});

test("native and Docker entrypoints forward the optional public MCP bind", () => {
  const { kernelArgs } = require("../lib/native");
  const config = { env: new Map(), wattetheriaDataDir: "/state", wattswarmStateDir: "/swarm" };
  assert.ok(!kernelArgs(config).includes("--mcp-public-bind"));
  config.env.set("WATTETHERIA_MCP_PUBLIC_BIND", "0.0.0.0:7778");
  config.env.set("WATTETHERIA_MCP_PUBLIC_BASE_URL", "https://node.example");
  const args = kernelArgs(config);
  assert.equal(args[args.indexOf("--mcp-public-bind") + 1], "0.0.0.0:7778");
  assert.match(fs.readFileSync(path.join(ROOT_DIR, "scripts/docker-kernel-entrypoint.sh"), "utf8"), /--mcp-public-bind "\$\{WATTETHERIA_MCP_PUBLIC_BIND\}"/);
  assert.ok(fs.readFileSync(path.join(ROOT_DIR, "docker-compose.dev.yml"), "utf8").includes('--mcp-public-bind "$$WATTETHERIA_MCP_PUBLIC_BIND"'));
});

test("Docker public MCP commands read private node files inside the kernel container",
  { skip: process.platform === "win32" && "requires an executable Docker test shim" }, async (context) => {
    const { dir, state, envPath, secret, secretPath } = publicMcpDeployment(context, "docker");
    fs.writeFileSync(secretPath, secret, { mode: 0o600 });
    fs.writeFileSync(path.join(state, "control.token"), "docker-local-token", { mode: 0o600 });
    fs.writeFileSync(path.join(dir, "docker-compose.yml"), "services: {}\n");
    fs.appendFileSync(envPath, "WATTETHERIA_MCP_PUBLIC_BIND=0.0.0.0:7778\nWATTETHERIA_MCP_PUBLIC_BASE_URL=https://node.example\n");
    const bin = path.join(dir, "bin");
    fs.mkdirSync(bin);
    const marker = path.join(dir, "docker-args.jsonl");
    fs.writeFileSync(path.join(bin, "docker"), `#!${process.execPath}\n` +
      `const fs = require('node:fs'); const args = process.argv.slice(2);\n` +
      `fs.appendFileSync(${JSON.stringify(marker)}, JSON.stringify(args) + '\\n');\n` +
      `const cat = args.indexOf('cat'); if (cat >= 0) process.stdout.write(fs.readFileSync(require('node:path').join(${JSON.stringify(state)}, require('node:path').basename(args[cat + 1])), 'utf8'));\n`);
    fs.chmodSync(path.join(bin, "docker"), 0o755);
    const env = { PATH: `${bin}${path.delimiter}${process.env.PATH || ""}` };
    const printed = await runAsyncCli(["mcp", "url", "--dir", dir], env);
    assert.equal(printed.status, 0, printed.stderr);
    assert.equal(printed.stdout.trim(), `https://node.example/mcp/${secret}`);
    const server = require("node:http").createServer((request, response) => {
      assert.equal(request.headers.authorization, "Bearer docker-local-token");
      assert.equal(request.url, "/v1/mcp/public-url/rotate");
      response.setHeader("content-type", "application/json");
      response.end(JSON.stringify({ path: `/mcp/${secret}`, revokedSubscriptions: 0 }));
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    context.after(() => new Promise((resolve) => server.close(resolve)));
    const rotated = await runAsyncCli(["mcp", "rotate", "--dir", dir, "--control-plane", `http://127.0.0.1:${server.address().port}`], env);
    assert.equal(rotated.status, 0, rotated.stderr);
    assert.doesNotMatch(rotated.stdout + rotated.stderr, /docker-local-token/);
    const calls = fs.readFileSync(marker, "utf8").trim().split("\n").map((row) => JSON.parse(row));
    for (const name of ["mcp_url_secret", "control.token"]) {
      const call = calls.find((args) => args.includes(`/var/lib/wattetheria/${name}`));
      assert.ok(call, `${name} was not read inside the container`);
      assert.ok(call.includes("exec") && call.includes("-T") && call.includes("kernel"));
      assert.deepEqual(call.filter((argument) => argument.endsWith(".yml")),
        [path.join(dir, "docker-compose.yml")]);
    }
  });

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

test(
  "network uses the cached native CLI from a GitHub Release",
  { skip: process.platform === "win32" && "uses a POSIX executable shim" },
  (context) => {
    const cacheRoot = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-cache-test-"));
    const platformKey = `${process.platform}-${process.arch}`;
    const binDirectory = path.join(cacheRoot, "1.2.3", platformKey);
    const marker = path.join(cacheRoot, "args.txt");
    fs.mkdirSync(binDirectory, { recursive: true });
    fs.writeFileSync(path.join(cacheRoot, "current.json"), JSON.stringify({
      tag: "v1.2.3",
      platformKey
    }));
    const binary = path.join(binDirectory, "wattetheria-client-cli");
    fs.writeFileSync(
      binary,
      `#!/bin/sh\nif [ "$1" = "--help" ]; then exit 0; fi\nprintf '%s\\n' "$@" > '${marker}'\n`
    );
    fs.chmodSync(binary, 0o755);
    for (const name of ["wattetheria-kernel", "wattswarm", "wattswarm-runtime"]) {
      fs.writeFileSync(path.join(binDirectory, name), "cached release binary\n");
    }
    context.after(() => fs.rmSync(cacheRoot, { recursive: true, force: true }));

    const result = spawnSync(process.execPath, [CLI_PATH, "network", "authority-show"], {
      cwd: ROOT_DIR,
      encoding: "utf8",
      env: {
        ...process.env,
        WATTETHERIA_NATIVE_CACHE_DIR: cacheRoot,
        WATTETHERIA_NO_BANNER: "1"
      }
    });

    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(fs.readFileSync(marker, "utf8").trim().split(/\r?\n/), [
      "network",
      "authority-show"
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
  test(`${command} works without updating the npm CLI first`, (context) => {
    const markerDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-markers-"));
    const npmMarker = path.join(markerDirectory, "npm");
    const dockerMarker = path.join(markerDirectory, "docker");
    const npmDirectory = fakeCommandDirectory("999.0.0", npmMarker, dockerMarker);
    const deploymentDirectory = path.join(markerDirectory, "deployment");
    fs.mkdirSync(deploymentDirectory, { recursive: true });
    if (command === "update") {
      fs.copyFileSync(path.join(ROOT_DIR, ".env.release"), path.join(deploymentDirectory, ".env"));
      fs.copyFileSync(
        path.join(ROOT_DIR, "docker-compose.release.yml"),
        path.join(deploymentDirectory, "docker-compose.yml")
      );
    }
    context.after(() => fs.rmSync(npmDirectory, { recursive: true, force: true }));
    context.after(() => fs.rmSync(markerDirectory, { recursive: true, force: true }));

    const result = spawnSync(process.execPath, [
      CLI_PATH,
      command,
      "--dir",
      deploymentDirectory,
      "--tag",
      "test",
      "--no-health-checks"
    ], {
      cwd: ROOT_DIR,
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: `${npmDirectory}${path.delimiter}${process.env.PATH || ""}`,
        WATTETHERIA_NO_BANNER: "1",
      },
    });

    assert.equal(result.status, 0, result.stderr);
    assert.equal(invocationCount(npmMarker), 0);
    assert.ok(invocationCount(dockerMarker) > 0);
  });
}

test("cli update is the explicit npm package update command", (context) => {
  const testDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-cli-update-test-"));
  const npmMarker = path.join(testDirectory, "npm");
  const dockerMarker = path.join(testDirectory, "docker");
  const commandDirectory = fakeCommandDirectory("1.2.3", npmMarker, dockerMarker);
  context.after(() => fs.rmSync(commandDirectory, { recursive: true, force: true }));
  context.after(() => fs.rmSync(testDirectory, { recursive: true, force: true }));

  const result = spawnSync(process.execPath, [CLI_PATH, "cli", "update"], {
    cwd: ROOT_DIR,
    encoding: "utf8",
    env: {
      ...process.env,
      PATH: `${commandDirectory}${path.delimiter}${process.env.PATH || ""}`,
      WATTETHERIA_NO_BANNER: "1"
    }
  });

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /npm install -g wattetheria@latest/);
  assert.equal(invocationCount(npmMarker), 1);
  assert.equal(invocationCount(dockerMarker), 0);
});

test(
  "setup delegates to install without invoking npm to check the CLI version",
  { skip: process.platform === "win32" && "requires an executable Docker test shim" },
  (context) => {
    const testDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-setup-test-"));
    const npmMarker = path.join(testDirectory, "npm");
    const dockerMarker = path.join(testDirectory, "docker");
    const commandDirectory = fakeCommandDirectory("999.0.0", npmMarker, dockerMarker);
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
    assert.equal(invocationCount(npmMarker), 0);
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

test("native kernel forwards the selected agent event mode", () => {
  const { kernelArgs } = require("../lib/native");
  const config = {
    env: new Map([["WATTETHERIA_AGENT_EVENT_MODE", "mcp_events"]]),
    wattetheriaDataDir: "/state/wattetheria",
    wattswarmStateDir: "/state/wattswarm",
  };
  const args = kernelArgs(config);
  assert.equal(args[args.indexOf("--agent-event-mode") + 1], "mcp_events");
  assert.ok(!kernelArgs({ ...config, env: new Map() }).includes("--agent-event-mode"));
});

test("Docker dev mode flags are evaluated in the container shell", () => {
  const source = fs.readFileSync(path.join(__dirname, "..", "docker-compose.dev.yml"), "utf8");
  assert.ok(source.includes('[ -n "$${WATTETHERIA_AGENT_EVENT_MODE:-}" ] && set -- "$$@" --agent-event-mode "$$WATTETHERIA_AGENT_EVENT_MODE"'));
  assert.ok(source.includes('[ -n "$${WATTETHERIA_BRAIN_PROVIDER_KIND:-}" ]'));
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
    const npmMarker = path.join(testDirectory, "npm");
    const dockerMarker = path.join(testDirectory, "docker");
    const commandDirectory = fakeCommandDirectory("999.0.0", npmMarker, dockerMarker);
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
    assert.equal(invocationCount(npmMarker), 0);

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

    const update = cli("update", "--no-health-checks");
    assert.equal(update.status, 0, update.stderr);
    assert.match(update.stdout, /Native services stopped\./);
    assert.match(update.stdout, /Native supervisor started/);

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

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
  for (const runtime of ["native", "docker"]) {
    test(`${command} rejects an outdated npm CLI before ${runtime} deployment work`, (context) => {
      const testDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-outdated-cli-"));
      const npmMarker = path.join(testDirectory, "npm");
      const dockerMarker = path.join(testDirectory, "docker");
      const commandDirectory = fakeCommandDirectory("999.0.0", npmMarker, dockerMarker);
      const deploymentDirectory = path.join(testDirectory, "deployment");
      const envPath = path.join(deploymentDirectory, ".env");
      const deploymentEnv = `WATTETHERIA_DEPLOYMENT_RUNTIME=${runtime}\n`;
      fs.mkdirSync(deploymentDirectory);
      fs.writeFileSync(envPath, deploymentEnv);
      context.after(() => fs.rmSync(commandDirectory, { recursive: true, force: true }));
      context.after(() => fs.rmSync(testDirectory, { recursive: true, force: true }));

      const result = spawnSync(process.execPath, [
        CLI_PATH, command, "--dir", deploymentDirectory, "--no-health-checks",
        ...(runtime === "docker" ? ["--tag", "test"] : [])
      ], {
        cwd: ROOT_DIR,
        encoding: "utf8",
        env: {
          ...process.env,
          PATH: `${commandDirectory}${path.delimiter}${process.env.PATH || ""}`,
          WATTETHERIA_NATIVE_BIN_DIR: path.join(testDirectory, "missing-binaries"),
          WATTETHERIA_NATIVE_CACHE_DIR: path.join(testDirectory, "cache"),
          WATTETHERIA_NO_BANNER: "1"
        }
      });

      assert.equal(result.status, 1);
      assert.match(result.stderr, /Wattetheria CLI is outdated\./);
      assert.ok(result.stderr.includes(`Current: ${require("../package.json").version}`));
      assert.match(result.stderr, /Latest:  999\.0\.0/);
      assert.match(result.stderr, /wattetheria cli update/);
      assert.ok(result.stderr.includes(`Then rerun:\n  wattetheria ${command}`));
      assert.equal(invocationCount(npmMarker), 1);
      assert.equal(invocationCount(dockerMarker), 0);
      assert.equal(fs.readFileSync(envPath, "utf8"), deploymentEnv);
      assert.deepEqual(fs.readdirSync(deploymentDirectory), [".env"]);
      assert.equal(fs.existsSync(path.join(testDirectory, "cache")), false);
      assert.doesNotMatch(result.stdout, /Checking native binaries|Pulling|Starting|Stopping/);
    });
  }

  test(`${command} accepts a current npm CLI`, (context) => {
    const markerDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-markers-"));
    const npmMarker = path.join(markerDirectory, "npm");
    const dockerMarker = path.join(markerDirectory, "docker");
    const npmDirectory = fakeCommandDirectory(require("../package.json").version, npmMarker, dockerMarker);
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
    assert.equal(invocationCount(npmMarker), 1);
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
  "setup checks the CLI version once when it delegates to install",
  { skip: process.platform === "win32" && "requires an executable Docker test shim" },
  (context) => {
    const testDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-setup-test-"));
    const npmMarker = path.join(testDirectory, "npm");
    const dockerMarker = path.join(testDirectory, "docker");
    const commandDirectory = fakeCommandDirectory(require("../package.json").version, npmMarker, dockerMarker);
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

async function setupFixture(context, runtime, settings = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-setup-mode-"));
  const packageRoot = path.join(root, "package");
  fs.mkdirSync(packageRoot);
  for (const file of ["bin", "lib", "package.json", ".env.native", ".env.release", "docker-compose.release.yml"]) {
    fs.cpSync(path.join(ROOT_DIR, file), path.join(packageRoot, file), { recursive: true });
  }
  if (settings.packageVersion) {
    const packagePath = path.join(packageRoot, "package.json");
    const metadata = JSON.parse(fs.readFileSync(packagePath, "utf8"));
    metadata.version = settings.packageVersion;
    fs.writeFileSync(packagePath, JSON.stringify(metadata));
  }
  const registryRequests = [];
  let certificate;
  if (settings.registryTags) {
    certificate = path.join(root, "registry-cert.pem");
    const key = path.join(root, "registry-key.pem");
    const generated = spawnSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes",
      "-keyout", key, "-out", certificate, "-days", "1", "-subj", "/CN=127.0.0.1",
      "-addext", "subjectAltName=IP:127.0.0.1"], { encoding: "utf8" });
    assert.equal(generated.status, 0, generated.stderr);
    const registry = require("node:https").createServer({ key: fs.readFileSync(key), cert: fs.readFileSync(certificate) }, (request, response) => {
      registryRequests.push(request.url);
      response.setHeader("Content-Type", "application/json");
      const expected = /^\/v2\/wattetheria\/(?:wattetheria-kernel|wattswarm-kernel|wattswarm-runtime|wattswarm-worker)\/tags\/list\?n=1000$/;
      response.statusCode = expected.test(request.url) ? 200 : 404;
      response.end(JSON.stringify({ tags: settings.registryTags }));
    });
    await new Promise(resolve => registry.listen(0, "127.0.0.1", resolve));
    context.after(() => new Promise(resolve => registry.close(resolve)));
    const releaseTemplate = path.join(packageRoot, ".env.release");
    fs.writeFileSync(releaseTemplate, fs.readFileSync(releaseTemplate, "utf8")
      .replaceAll("ghcr.io/wattetheria/", `127.0.0.1:${registry.address().port}/wattetheria/`));
  }
  const requests = [];
  const http = require("node:http");
  const health = http.createServer((request, response) => {
    requests.push(request.url);
    const running = runtime === "native"
      ? require("../lib/native").isNativeStackRunning(dir) : fs.existsSync(runningPath);
    response.statusCode = !["/v1/health", "/"].includes(request.url) ? 404
      : !running || requests.length <= (settings.healthFailures || 0) ? 503 : 200;
    response.end("test endpoint, not a Wattetheria service");
  });
  await new Promise(resolve => health.listen(0, "127.0.0.1", resolve));
  context.after(() => new Promise(resolve => health.close(resolve)));
  for (const name of [".env.native", ".env.release"]) {
    const template = path.join(packageRoot, name);
    fs.writeFileSync(template, fs.readFileSync(template, "utf8")
      .replace(/^WATTETHERIA_CONTROL_PLANE_PORT=.*$/m, `WATTETHERIA_CONTROL_PLANE_PORT=${health.address().port}`)
      .replace(/^WATTSWARM_UI_PORT=.*$/m, `WATTSWARM_UI_PORT=${health.address().port}`));
  }
  const template = path.join(packageRoot, ".env.native");
  fs.writeFileSync(template, fs.readFileSync(template, "utf8").replace(
    /^WATTSWARM_RUNTIME_PORT=.*$/m, `WATTSWARM_RUNTIME_PORT=${await freePort()}`
  ));
  const dir = settings.defaultDir ? path.join(root, ".wattetheria", "deploy") : path.join(root, "deployment");
  const envPath = path.join(dir, ".env");
  const npmMarker = path.join(root, "npm");
  const startsPath = path.join(root, "starts.jsonl");
  const runningPath = path.join(root, "docker-running");
  const callsPath = path.join(root, "docker-calls.jsonl");
  const commandDirectory = fakeCommandDirectory(settings.publishedVersion || require("../package.json").version, npmMarker, path.join(root, "docker"));
  const binDirectory = path.join(root, "native");
  writeFakeNativeBinaries(binDirectory, path.join(root, "wattswarm"));
  const recordStart = `fs.appendFileSync(${JSON.stringify(startsPath)}, JSON.stringify({ args, env: fs.readFileSync(${JSON.stringify(envPath)}, "utf8") }) + "\\n");`;
  fs.writeFileSync(path.join(binDirectory, "wattetheria-kernel"), [
    `#!${process.execPath}`,
    'const fs = require("node:fs"); const args = process.argv.slice(2);',
    recordStart,
    `fs.appendFileSync(${JSON.stringify(path.join(root, "kernel-env.jsonl"))}, JSON.stringify(process.env) + "\\n");`,
    'setInterval(() => {}, 1000);', ""
  ].join("\n"));
  fs.writeFileSync(path.join(commandDirectory, "docker"), [
    `#!${process.execPath}`,
    'const fs = require("node:fs"); const args = process.argv.slice(2);',
    `fs.appendFileSync(${JSON.stringify(callsPath)}, JSON.stringify(args) + "\\n");`,
    `if (args.includes(${JSON.stringify(settings.dockerFailure || "never-fail")}) && fs.readFileSync(${JSON.stringify(callsPath)}, "utf8").trim().split("\\n").map(JSON.parse).filter(call => call.includes(${JSON.stringify(settings.dockerFailure || "never-fail")})).length === ${settings.failureOccurrence || 1}) { console.error("injected Docker failure"); process.exit(23); }`,
    'if (!args.some(arg => ["--version", "version", "info", "config", "pull", "up", "down", "ps"].includes(arg))) { console.error("Unexpected Docker invocation", args); process.exit(24); }',
    `if (args.includes("up")) { ${recordStart} fs.writeFileSync(${JSON.stringify(runningPath)}, "running"); }`,
    `if (args.includes("down")) fs.rmSync(${JSON.stringify(runningPath)}, { force: true });`,
    `if (args.includes("ps") && fs.existsSync(${JSON.stringify(runningPath)})) console.log("kernel\\nwattswarm-postgres\\nwattswarm-runtime\\nwattswarm-kernel\\nwattswarm-worker");`, ""
  ].join("\n"));
  for (const command of ["open", "xdg-open"]) {
    fs.writeFileSync(path.join(commandDirectory, command), `#!/bin/sh\nexit ${settings.openFailure ? 1 : 0}\n`, { mode: 0o755 });
  }
  const env = {
    ...process.env,
    HOME: root,
    USERPROFILE: root,
    PATH: `${commandDirectory}${path.delimiter}${process.env.PATH || ""}`,
    WATTETHERIA_NATIVE_BIN_DIR: binDirectory,
    WATTETHERIA_NATIVE_CACHE_DIR: path.join(root, "cache"),
    ...(certificate ? { NODE_EXTRA_CA_CERTS: certificate } : {}),
    WATTETHERIA_NO_BANNER: "1"
  };
  const cliPath = path.join(packageRoot, "bin", "wattetheria.js");
  const cli = (...args) => new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [cliPath, ...args, ...(settings.defaultDir ? [] : ["--dir", dir])], { cwd: packageRoot, env });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", chunk => { stdout += chunk; });
    child.stderr.on("data", chunk => { stderr += chunk; });
    child.once("error", reject);
    child.once("close", status => resolve({ status, stdout, stderr }));
  });
  context.after(async () => {
    await cli("stop");
    fs.rmSync(commandDirectory, { recursive: true, force: true });
    fs.rmSync(root, { recursive: true, force: true });
  });
  const setupArgs = ["setup", ...(settings.defaultRuntime ? [] : ["--runtime", runtime]),
    ...(settings.defaultDir ? [] : ["--dir", dir]),
    ...(runtime === "docker" && !settings.registryTags ? ["--tag", "test"] : [])];
  const interactive = (mode, answer = mode === "mcp_events" ? "2" : "1", connection = "local", urlAnswers) => new Promise((resolve, reject) => {
    const remote = mode === "mcp_events" && connection === "remote";
    const responses = [
      ...(Array.isArray(answer) ? answer : [answer]).map(value => ["Select Agent Event Mode [1-2]", value]),
      ...(mode === "mcp_events" ? [
        ...(settings.connectionAnswers || [remote ? "2" : "1"]).map(value => ["Select MCP Connection [1-2]", value]),
        ...(urlAnswers || [remote ? "https://mcp.example.test/prefix" : ""])
          .map(value => [remote ? "Public MCP Base URL" : "Webhook receiver URL", value])
      ] : []),
      ...(mode === "api_runtime" ? [
        ["After starting the API server", ""], ["After saving runtime config", ""]
      ] : []),
      ...(remote ? [["Press Enter to restart Wattetheria and enable Remote MCP.", ""]] : []),
      ["After saving MCP config", ""], ["After restarting your agent runtime", ""],
      ["After verifying MCP access", ""]
    ];
    const child = spawn(process.execPath, ["-e", [
      "process.stdin.isTTY = true; process.stdout.isTTY = true;",
      `process.argv.splice(1, 0, ${JSON.stringify(cliPath)}); require(${JSON.stringify(cliPath)});`
    ].join("\n"), ...setupArgs], { cwd: packageRoot, env });
    let stdout = "";
    let stderr = "";
    let response = 0;
    let cursor = 0;
    const timeout = setTimeout(() => {
      child.kill();
      reject(new Error(`Setup prompt timed out: ${stdout}\n${stderr}`));
    }, 30000);
    child.stdout.on("data", chunk => {
      stdout += chunk;
      if (response < responses.length) {
        const [prompt, input] = responses[response];
        const index = stdout.indexOf(prompt, cursor);
        if (index >= 0) {
          cursor = index + prompt.length;
          response += 1;
          child.stdin.write(`${input}\n`);
        }
      }
    });
    child.stderr.on("data", chunk => { stderr += chunk; });
    child.once("error", error => { clearTimeout(timeout); reject(error); });
    child.once("close", status => {
      clearTimeout(timeout);
      resolve({ status, stdout: require("node:util").stripVTControlCharacters(stdout), stderr,
        answered: response, expectedAnswers: responses.length });
    });
  });
  const starts = () => fs.existsSync(startsPath)
    ? fs.readFileSync(startsPath, "utf8").trim().split("\n").map(line => JSON.parse(line))
    : [];
  const waitForStart = async (configPattern) => {
    const started = () => {
      const last = starts().at(-1);
      return Boolean(last && (!configPattern || configPattern.test(last.env))
        && (runtime !== "native" || require("../lib/native").isNativeStackRunning(dir)));
    };
    for (let attempt = 0; attempt < 50 && !started(); attempt += 1) {
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    assert.ok(started(), "deployment started with saved configuration");
  };
  const dockerCalls = () => fs.existsSync(callsPath)
    ? fs.readFileSync(callsPath, "utf8").trim().split("\n").map(JSON.parse) : [];
  const kernelEnvs = () => fs.existsSync(path.join(root, "kernel-env.jsonl"))
    ? fs.readFileSync(path.join(root, "kernel-env.jsonl"), "utf8").trim().split("\n").map(JSON.parse) : [];
  return { cli, setupArgs, interactive, envPath, npmMarker, starts, waitForStart, requests,
    dockerCalls, kernelEnvs, root, commandDirectory, packageRoot, env, registryRequests };
}

for (const runtime of ["native", "docker"]) {
  for (const mode of ["api_runtime", "mcp_events"]) {
    test(`interactive ${runtime} setup selects ${mode} before first startup`,
      { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
        const fixture = await setupFixture(context, runtime);
        const result = await fixture.interactive(mode);
        assert.equal(result.status, 0, result.stderr);
        assert.equal(result.answered, result.expectedAnswers);
        assert.ok(fixture.requests.includes("/v1/health"));
        assert.ok(fixture.requests.includes("/"));
        assert.match(result.stdout, /Agent Event Mode/);
        assert.doesNotMatch(result.stdout, /AGENT_EVENT_MODE/);
        assert.ok(result.stdout.indexOf("Select Agent Event Mode") < result.stdout.indexOf("Checking "));
        assert.match(result.stdout, /Install MCP in your agent runtime/);
        assert.match(result.stdout, /Restart Wattetheria/);
        assert.match(result.stdout, /Restart your agent runtime/);
        assert.match(result.stdout, /Setup complete\./);
        if (mode === "mcp_events") {
          assert.doesNotMatch(result.stdout, /Start an agent runtime API server|Configure runtime|OpenAI-compatible|Open:/);
          assert.match(result.stdout, /1\. Local MCP \(webhook\)/);
          assert.match(result.stdout, /2\. Remote MCP/);
          const host = runtime === "native" ? "127.0.0.1" : "host.docker.internal";
          assert.ok(result.stdout.includes(`default: http://${host}:3000/webhook`));
          const saved = fs.readFileSync(fixture.envPath, "utf8");
          assert.ok(saved.includes(`WATTETHERIA_EVENT_WEBHOOK_URL=http://${host}:3000/webhook`));
          const secret = saved.match(/^WATTETHERIA_EVENT_WEBHOOK_SECRET=(whsec_[A-Za-z0-9+/]+=*)$/m)?.[1];
          assert.ok(secret, "automatically generated webhook signing secret");
          assert.equal(Buffer.from(secret.slice(6), "base64").length, 32);
          assert.ok(!result.stdout.includes(secret), "secret is not printed");
          assert.doesNotMatch(result.stdout, /(?:Enter|Select|Type).*secret/i);
          await fixture.waitForStart(/^WATTETHERIA_EVENT_WEBHOOK_SECRET=whsec_/m);
          assert.ok(fixture.starts().at(-1).env.includes(`WATTETHERIA_EVENT_WEBHOOK_SECRET=${secret}`));
          assert.doesNotMatch(saved, /^WATTETHERIA_MCP_PUBLIC_BIND=0\.0\.0\.0/m);
        } else {
          assert.match(result.stdout, /Start an agent runtime API server/);
          assert.match(result.stdout, /Configure runtime/);
        }
        const count = mode === "mcp_events" ? 5 : 7;
        assert.deepEqual([...result.stdout.matchAll(/\[(\d+)\/(\d+)\]/g)].map(match => [Number(match[1]), Number(match[2])]),
          Array.from({ length: count }, (_, index) => [index + 1, count]));
        assert.equal(invocationCount(fixture.npmMarker), 1);
        await fixture.waitForStart();
        for (const start of fixture.starts()) {
          assert.match(start.env, new RegExp(`^WATTETHERIA_AGENT_EVENT_MODE=${mode}$`, "m"));
          if (mode === "mcp_events") {
            assert.match(start.env, /^WATTETHERIA_EVENT_WEBHOOK_SECRET=whsec_/m);
          }
          if (runtime === "native") {
            assert.equal(start.args[start.args.indexOf("--agent-event-mode") + 1], mode);
          }
        }
        if (runtime === "native") {
          assert.equal(fixture.kernelEnvs().at(-1).WATTETHERIA_AGENT_EVENT_MODE, mode);
          if (mode === "mcp_events") {
            assert.match(fixture.kernelEnvs().at(-1).WATTETHERIA_EVENT_WEBHOOK_SECRET, /^whsec_/);
          }
        } else {
          const operations = fixture.dockerCalls().flatMap(args => args.filter(arg => ["config", "pull", "up", "down"].includes(arg)));
          assert.deepEqual(operations, ["config", "pull", "up", "down", "up"]);
        }
      });
  }

  test(`interactive ${runtime} setup configures Remote MCP and applies it on restart`,
    { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
      const fixture = await setupFixture(context, runtime);
      const result = await fixture.interactive("mcp_events", "2", "remote", ["", "http://node.example", "https://mcp.example.test/prefix"]);
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.answered, result.expectedAnswers);
      assert.match(result.stdout, /example: https:\/\/node\.example/);
      assert.equal(result.stdout.split("Enter a public HTTPS base URL.").length - 1, 2);
      assert.match(result.stdout, /npx wattetheria mcp url --dir/);
      assert.doesNotMatch(result.stdout, /"command": "npx"|mcp-proxy|Webhook receiver URL|Configure runtime/);
      assert.match(result.stdout, /Setup complete/);
      const saved = fs.readFileSync(fixture.envPath, "utf8");
      assert.match(saved, /^WATTETHERIA_MCP_PUBLIC_BIND=0\.0\.0\.0:7778$/m);
      assert.match(saved, /^WATTETHERIA_MCP_PUBLIC_BASE_URL=https:\/\/mcp\.example\.test\/prefix$/m);
      assert.doesNotMatch(saved, /^WATTETHERIA_EVENT_WEBHOOK_SECRET=whsec_/m);
      await fixture.waitForStart(/^WATTETHERIA_MCP_PUBLIC_BIND=0\.0\.0\.0:7778$/m);
      for (const start of fixture.starts()) {
        assert.match(start.env, /^WATTETHERIA_MCP_PUBLIC_BIND=0\.0\.0\.0:7778$/m);
      }
      const noninteractive = await fixture.cli(...fixture.setupArgs);
      assert.equal(noninteractive.status, 0, noninteractive.stderr);
      assert.match(noninteractive.stdout, /npx wattetheria mcp url --dir/);
      assert.match(noninteractive.stdout, /1\. Restart Wattetheria/);
      assert.match(noninteractive.stdout, /2\. Add Wattetheria MCP/);
      assert.doesNotMatch(noninteractive.stdout, /Select MCP Connection|Public MCP Base URL \(|mcp-proxy/);
      assert.equal(fs.readFileSync(fixture.envPath, "utf8"), saved);
    });

  test(`${runtime} setup switches mode, preserves Brain config, and keeps the saved default`,
    { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
      const fixture = await setupFixture(context, runtime);
      const initial = await fixture.cli(...fixture.setupArgs);
      assert.equal(initial.status, 0, initial.stderr);
      assert.match(initial.stdout, /Using api_runtime\./);
      assert.match(initial.stdout, /Start an agent runtime API server/);
      assert.doesNotMatch(initial.stdout, /Select Agent Event Mode/);
      await fixture.waitForStart();
      const brainEnv = fs.readFileSync(fixture.envPath, "utf8")
        .replace(/^WATTETHERIA_BRAIN_BASE_URL=.*$/m, "WATTETHERIA_BRAIN_BASE_URL=http://brain.example/v1")
        .replace(/^WATTETHERIA_BRAIN_MODEL=.*$/m, "WATTETHERIA_BRAIN_MODEL=existing-model")
        .replace(/^WATTETHERIA_BRAIN_API_KEY=.*$/m, "WATTETHERIA_BRAIN_API_KEY=existing-key");
      fs.writeFileSync(fixture.envPath, brainEnv);

      for (const mode of ["mcp_events", "api_runtime"]) {
        const switched = await fixture.interactive(mode);
        assert.equal(switched.status, 0, switched.stderr);
        assert.equal(switched.answered, switched.expectedAnswers);
        assert.match(switched.stdout, /Restart Wattetheria/);
        assert.doesNotMatch(switched.stdout, /Agent Event Mode changed/);
        assert.equal(switched.stdout.split(runtime === "native" ? "Native services stopped." : "Stopping release stack...").length - 1, 1);
        const saved = fs.readFileSync(fixture.envPath, "utf8");
        assert.match(saved, new RegExp(`^WATTETHERIA_AGENT_EVENT_MODE=${mode}$`, "m"));
        for (const line of brainEnv.split("\n").filter(line => line.startsWith("WATTETHERIA_BRAIN_"))) {
          assert.ok(saved.split("\n").includes(line), `preserved ${line.split("=")[0]}`);
        }
        const repeat = await fixture.interactive(mode, "");
        assert.equal(repeat.status, 0, repeat.stderr);
        assert.equal(repeat.answered, repeat.expectedAnswers);
        assert.ok(repeat.stdout.includes(`default: ${mode}`));
        assert.doesNotMatch(repeat.stdout, /Agent Event Mode changed/);
        assert.equal(fs.readFileSync(fixture.envPath, "utf8"), saved, "repeated setup preserves the webhook secret and config");
        const noninteractive = await fixture.cli(...fixture.setupArgs);
        assert.equal(noninteractive.status, 0, noninteractive.stderr);
        assert.ok(noninteractive.stdout.includes(`Using ${mode}.`));
        assert.doesNotMatch(noninteractive.stdout, /Select Agent Event Mode/);
        assert.match(noninteractive.stdout, /Add Wattetheria MCP|Verify MCP access/);
        if (mode === "mcp_events") {
          assert.doesNotMatch(noninteractive.stdout, /Start an agent runtime API server|configure the runtime|OpenAI-compatible/);
          assert.match(noninteractive.stdout, /1\. Add Wattetheria MCP/);
          assert.match(noninteractive.stdout, /4\. Verify MCP access/);
        } else {
          assert.match(noninteractive.stdout, /Start an agent runtime API server/);
          assert.match(noninteractive.stdout, /6\. Verify MCP access/);
        }
      }
    });
}

for (const runtime of ["native", "docker"]) {
  for (const connection of ["api", "local", "remote"]) {
    test(`${runtime} setup ${connection}: default entry, running and stopped deployments, interactive and noninteractive`,
      { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
      const fixture = await setupFixture(context, runtime, {
        defaultRuntime: runtime === "docker", defaultDir: true,
        connectionAnswers: connection === "remote" ? ["invalid", "2"] : ["invalid", ""],
        openFailure: true, healthFailures: 1
      });
      const mode = connection === "api" ? "api_runtime" : "mcp_events";
      const choice = connection === "api" ? "" : "2";
      const invalidUrls = connection === "remote"
        ? ["not a URL", "ftp://node.example", "https://user:pass@node.example", "https://:pass@node.example", "https://node.example/#fragment", "https://node.example/?query=1", "https://node.example/prefix"]
        : ["not a URL", "ftp://localhost", "http://user:pass@localhost", "http://:pass@localhost", "http://localhost/#fragment", "https://receiver.example/events?agent=1"];
      const first = await fixture.interactive(mode, ["invalid", choice], connection, invalidUrls);
      assert.equal(first.status, 0, first.stderr);
      assert.equal(first.answered, first.expectedAnswers);
      assert.match(first.stdout, /Please choose 1 or 2/);
      assert.match(first.stdout, /Still waiting for kernel health.*HTTP 503/);
      assert.match(first.stdout, /\[ok\] kernel health/);
      assert.match(first.stdout, /\[ok\] wattswarm ui/);
      assert.ok(!fixture.setupArgs.includes("--no-health-checks"));
      assert.equal(fixture.setupArgs.includes("--runtime"), runtime === "native");
      const env = fs.readFileSync(fixture.envPath, "utf8");
      assert.ok(env.includes(`WATTETHERIA_DEPLOYMENT_RUNTIME=${runtime}`));
      if (connection === "api") assert.match(first.stdout, /Open the URL above in your browser/);
      if (connection === "remote") assert.match(first.stdout, /npx wattetheria mcp url\r?\n/);
      if (connection === "local") assert.match(env, /^WATTETHERIA_EVENT_WEBHOOK_URL=https:\/\/receiver\.example\/events\?agent=1$/m);
      await fixture.waitForStart();
      for (const state of ["running", "stopped"]) {
        if (state === "stopped") assert.equal((await fixture.cli("stop")).status, 0);
        const automatic = await fixture.cli("setup");
        assert.equal(automatic.status, 0, automatic.stderr);
        assert.match(automatic.stdout, state === "running" ? /Skipping start/ : /Starting existing stack/);
        assert.match(automatic.stdout, /\[ok\] kernel health/);
        assert.doesNotMatch(automatic.stdout, /Select Agent Event Mode|Select MCP Connection/);
        assert.equal(fs.readFileSync(fixture.envPath, "utf8"), env);
        await fixture.waitForStart();
        if (state === "stopped") assert.equal((await fixture.cli("stop")).status, 0);
        const manual = await fixture.interactive(mode, "", connection, [""]);
        assert.equal(manual.status, 0, manual.stderr);
        assert.equal(manual.answered, manual.expectedAnswers);
        assert.match(manual.stdout, state === "running" ? /Skipping start/ : /Starting existing stack/);
        assert.equal(fs.readFileSync(fixture.envPath, "utf8"), env);
        await fixture.waitForStart();
      }
    });
  }
}

for (const failure of ["--version", "version", "info", "config", "pull", "up", "down"]) {
  test(`setup stops on Docker ${failure} failure instead of claiming completion`,
    { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
    const fixture = await setupFixture(context, "docker", { dockerFailure: failure, defaultRuntime: true });
    const result = await fixture.interactive("mcp_events");
    assert.equal(result.status, 1, result.stderr);
    assert.doesNotMatch(result.stdout, /Setup complete|Verify MCP access/);
    const calls = fixture.dockerCalls();
    assert.ok(calls.some(args => args.includes(failure)));
    if (["--version", "version", "info", "config", "pull"].includes(failure)) {
      assert.equal(fixture.starts().length, 0);
      assert.equal(fixture.requests.length, 0);
    }
    if (["--version", "version", "info"].includes(failure)) assert.equal(fs.existsSync(fixture.envPath), false);
    if (failure === "down") assert.equal(calls.filter(args => args.includes("up")).length, 1);
  });
}

test("setup does not continue after a failed Docker restart",
  { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
  const fixture = await setupFixture(context, "docker", { dockerFailure: "up", failureOccurrence: 2 });
  const result = await fixture.interactive("mcp_events", "2", "remote");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /injected Docker failure|Command failed/);
  assert.match(result.stdout, /Restart Wattetheria/);
  assert.doesNotMatch(result.stdout, /After saving MCP config|Restart your agent runtime|Setup complete/);
  assert.equal(fixture.dockerCalls().filter(args => args.includes("up")).length, 2);
});

test("existing Docker setup stops when service status cannot be read",
  { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
  const fixture = await setupFixture(context, "docker", { dockerFailure: "ps" });
  assert.equal((await fixture.cli(...fixture.setupArgs)).status, 0);
  const before = fixture.dockerCalls().filter(args => args.includes("up")).length;
  const result = await fixture.interactive("mcp_events");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /injected Docker failure/);
  assert.doesNotMatch(result.stdout, /Restart Wattetheria|Setup complete/);
  assert.equal(fixture.dockerCalls().filter(args => args.includes("up")).length, before);
});

for (const connection of ["api", "local", "remote"]) {
  test(`plain setup resolves latest Docker images for ${connection}, then update checks health`,
    { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
    const fixture = await setupFixture(context, "docker", {
      defaultRuntime: true, defaultDir: true, registryTags: ["latest", "v1.2.3", "v1.3.0"]
    });
    assert.deepEqual(fixture.setupArgs, ["setup"]);
    const mode = connection === "api" ? "api_runtime" : "mcp_events";
    const result = await fixture.interactive(mode, connection === "api" ? "1" : "2", connection);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.answered, result.expectedAnswers);
    assert.match(result.stdout, /Resolved latest published release tag v1.3.0/);
    assert.equal(new Set(fixture.registryRequests).size, 4);
    assert.match(fs.readFileSync(fixture.envPath, "utf8"), /^RELEASE_TAG=v1\.3\.0$/m);
    assert.match(result.stdout, /Setup complete/);
    const update = await fixture.cli("update");
    assert.equal(update.status, 0, update.stderr);
    assert.match(update.stdout, /already pinned to latest published tag v1.3.0/);
    assert.match(update.stdout, /\[ok\] kernel health/);
    assert.equal(fixture.registryRequests.length, 8);
  });
}

test("plain setup stops when the registry has no published releases",
  { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
  const fixture = await setupFixture(context, "docker", { defaultRuntime: true, registryTags: [] });
  const result = await fixture.interactive("mcp_events");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /No published tags found/);
  assert.doesNotMatch(result.stdout, /Setup complete/);
  assert.equal(fixture.starts().length, 0);
  assert.equal(fixture.requests.length, 0);
});

test("native setup rejects missing binaries without downloading or saving selected config",
  { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
  const fixture = await setupFixture(context, "native");
  fs.rmSync(path.join(fixture.root, "native"), { recursive: true });
  const result = await fixture.interactive("mcp_events");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Native Wattetheria binaries were not found/);
  assert.equal(fs.existsSync(fixture.envPath), false);
  assert.equal(fs.existsSync(path.join(fixture.root, "cache")), false);
  assert.equal(fixture.requests.length, 0);
  assert.equal(fixture.starts().length, 0);
});

test("native setup and update reject Docker image tags without restarting",
  { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
  const fixture = await setupFixture(context, "native");
  const first = await fixture.cli(...fixture.setupArgs, "--tag", "test");
  assert.equal(first.status, 1);
  assert.match(first.stderr, /--tag selects Docker image tags/);
  assert.equal(fs.existsSync(fixture.envPath), false);
  assert.equal((await fixture.cli(...fixture.setupArgs)).status, 0);
  const before = fixture.starts().length;
  const update = await fixture.cli("update", "--tag", "test");
  assert.equal(update.status, 1);
  assert.match(update.stderr, /--tag selects Docker image tags/);
  assert.equal(fixture.starts().length, before);
});

test("Docker update rejects an incomplete deployment without starting it",
  { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
  const fixture = await setupFixture(context, "docker");
  fs.mkdirSync(path.dirname(fixture.envPath), { recursive: true });
  fs.writeFileSync(fixture.envPath, "WATTETHERIA_DEPLOYMENT_RUNTIME=docker\n");
  const result = await fixture.cli("update");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Deployment is not initialized/);
  assert.equal(fixture.starts().length, 0);
});

for (const runtime of ["native", "docker"]) {
  test(`${runtime} setup rejects a conflicting runtime before changing existing config`,
    { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
    const fixture = await setupFixture(context, runtime);
    assert.equal((await fixture.cli(...fixture.setupArgs)).status, 0);
    const before = fs.readFileSync(fixture.envPath, "utf8");
    const result = await fixture.cli("setup", "--runtime", runtime === "native" ? "docker" : "native");
    assert.equal(result.status, 1);
    assert.match(result.stderr, /deployment already exists/);
    assert.doesNotMatch(result.stdout, /Agent Event Mode/);
    assert.equal(fs.readFileSync(fixture.envPath, "utf8"), before);
  });
}

for (const runtime of ["native", "docker"]) {
  for (const scenario of ["missing-npm", "npm-error", "npm-error-no-detail", "invalid-version"]) {
    test(`${runtime} CLI version check fails closed on ${scenario}`,
      { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
      const fixture = await setupFixture(context, runtime);
      const npmPath = path.join(fixture.commandDirectory, "npm");
      const errors = {
        "missing-npm": /Failed to query latest Wattetheria CLI version:/,
        "npm-error": /Failed to query latest Wattetheria CLI version from npm: registry unavailable/,
        "npm-error-no-detail": /Failed to query latest Wattetheria CLI version from npm\./,
        "invalid-version": /Invalid Wattetheria CLI version: not-semver/
      };
      if (scenario === "missing-npm") {
        fs.rmSync(npmPath);
        fixture.env.PATH = fixture.commandDirectory;
      } else {
        fs.writeFileSync(npmPath, `#!${process.execPath}\n${scenario === "invalid-version"
          ? 'console.log("not-semver");' : `${scenario === "npm-error" ? 'console.error("registry unavailable");' : ""} process.exit(1);`}\n`);
      }
      for (const command of ["setup", "install", "update"]) {
        const result = await fixture.cli(command, "--runtime", runtime);
        assert.equal(result.status, 1);
        assert.match(result.stderr, errors[scenario]);
        assert.equal(fs.existsSync(fixture.envPath), false);
        assert.equal(fixture.dockerCalls().length, 0);
        assert.equal(fixture.starts().length, 0);
      }
    });
  }
  for (const [current, latest, outdated] of [
    ["1.2.3", "2.0.0", true], ["1.2.3", "1.3.0", true], ["1.2.3", "1.2.4", true],
    ["2.0.0", "1.2.3", false], ["1.3.0", "1.2.3", false], ["1.2.4", "1.2.3", false]
  ]) {
    test(`${runtime} setup compares ${current} with published ${latest}`,
      { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
      const fixture = await setupFixture(context, runtime, { packageVersion: current, publishedVersion: latest });
      const result = await fixture.cli(...fixture.setupArgs);
      assert.equal(result.status, outdated ? 1 : 0, result.stderr);
      assert.equal(invocationCount(fixture.npmMarker), 1);
      if (outdated) {
        assert.match(result.stderr, /Wattetheria CLI is outdated/);
        assert.equal(fs.existsSync(fixture.envPath), false);
        assert.equal(fixture.starts().length, 0);
      } else {
        assert.match(result.stdout, /Finish setup/);
        assert.match(result.stdout, /\[ok\] kernel health/);
      }
    });
  }
  test(`${runtime} setup retains an existing custom Remote MCP bind and quoted URL`,
    { skip: process.platform === "win32" && "uses POSIX executable shims" }, async context => {
    const fixture = await setupFixture(context, runtime, { connectionAnswers: [""] });
    assert.equal((await fixture.cli(...fixture.setupArgs)).status, 0);
    fs.appendFileSync(fixture.envPath, '\nWATTETHERIA_AGENT_EVENT_MODE=mcp_events\nWATTETHERIA_MCP_PUBLIC_BIND=127.0.0.1:17778\nWATTETHERIA_MCP_PUBLIC_BASE_URL="https://node.example/prefix"\n');
    const result = await fixture.interactive("mcp_events", "", "remote", [""]);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.answered, result.expectedAnswers);
    assert.match(result.stdout, /Select MCP Connection \[1-2\] \(default: 2\)/);
    assert.match(result.stdout, /default: https:\/\/node.example\/prefix/);
    assert.match(fs.readFileSync(fixture.envPath, "utf8"), /^WATTETHERIA_MCP_PUBLIC_BIND=127\.0\.0\.1:17778$/m);
    await fixture.waitForStart();
    if (runtime === "native") assert.equal(fixture.kernelEnvs().at(-1).WATTETHERIA_MCP_PUBLIC_BIND, "127.0.0.1:17778");
  });
}

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

for (const command of ["install", "setup"]) {
test(
  `native ${command} supervises local binaries without Docker`,
  { skip: process.platform === "win32" && "uses POSIX executable shims" },
  async (context) => {
    const testDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "wattetheria-native-test-"));
    const binDirectory = path.join(testDirectory, "bin");
    const deploymentDirectory = path.join(testDirectory, "deployment");
    const wattswarmMarker = path.join(testDirectory, "wattswarm-args");
    const npmMarker = path.join(testDirectory, "npm");
    const dockerMarker = path.join(testDirectory, "docker");
    const commandDirectory = fakeCommandDirectory(require("../package.json").version, npmMarker, dockerMarker);
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

    const install = cli(command, "--runtime", "native", "--no-health-checks");
    assert.equal(install.status, 0, install.stderr);
    assert.match(install.stdout, /Native supervisor started/);

    const deploymentEnv = fs.readFileSync(path.join(deploymentDirectory, ".env"), "utf8");
    assert.match(deploymentEnv, /^WATTETHERIA_DEPLOYMENT_RUNTIME=native$/m);
    if (command === "install") {
      assert.match(deploymentEnv, /^WATTSWARM_STORAGE_BACKEND=sqlite$/m);
    } else {
      assert.match(install.stdout, /Start existing Wattetheria deployment/);
    }
    assert.doesNotMatch(deploymentEnv, /WATTSWARM_PG_/);
    assert.equal(invocationCount(npmMarker), 1);

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
    assert.equal(invocationCount(npmMarker), 2);

    const stop = cli("stop");
    assert.equal(stop.status, 0, stop.stderr);
    assert.match(stop.stdout, /Native services stopped\./);
    assert.match(cli("status").stdout, /Native supervisor: stopped/);
    assert.equal(invocationCount(dockerMarker), 0);
  }
);
}

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

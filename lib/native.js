"use strict";

// Native (no Docker) deployment runtime: a detached supervisor process runs the
// wattetheria and wattswarm binaries directly with Wattswarm on SQLite.

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");

const WATTSWARM_STORE_NAME = "wattswarm.db";
const BOOTSTRAP_EXECUTOR_NAME = "rt";
const NATIVE_BINARY_BASE_NAMES = {
  kernel: "wattetheria-kernel",
  wattswarm: "wattswarm",
  runtime: "wattswarm-runtime"
};
const NATIVE_BINARY_NAMES = new Set(Object.values(NATIVE_BINARY_BASE_NAMES));
const SERVICE_NAMES = ["wattswarm-runtime", "wattswarm-kernel", "wattswarm-worker", "kernel"];
// Linux truncates `ps -o comm` to 15 characters.
const TRUNCATED_COMM_LENGTH = 15;
const LOG_NAMES = ["daemon", ...SERVICE_NAMES];
const STOP_POLL_MS = 500;
const CHILD_STOP_TIMEOUT_MS = 10000;
const DAEMON_STOP_TIMEOUT_MS = 45000;
const DAEMON_START_TIMEOUT_MS = 10000;
const RESTART_BASE_DELAY_MS = 1000;
const RESTART_MAX_DELAY_MS = 30000;
const STABLE_RUN_MS = 60000;
const RUNTIME_READY_TIMEOUT_MS = 60000;
const LOG_ROTATE_BYTES = 20 * 1024 * 1024;
const DEFAULT_LOG_TAIL_LINES = 100;

function nativeBinaryName(baseName, platform = process.platform) {
  return platform === "win32" ? `${baseName}.exe` : baseName;
}

function resolveNativeBinaries(binDirCandidates) {
  const checked = [];
  for (const dir of binDirCandidates.filter(Boolean)) {
    const binaries = {};
    const missing = [];
    for (const [key, baseName] of Object.entries(NATIVE_BINARY_BASE_NAMES)) {
      const binaryPath = path.join(dir, nativeBinaryName(baseName));
      if (fs.existsSync(binaryPath)) {
        binaries[key] = binaryPath;
      } else {
        missing.push(nativeBinaryName(baseName));
      }
    }
    if (missing.length === 0) {
      return binaries;
    }
    checked.push(`${dir} (missing ${missing.join(", ")})`);
  }
  throw new Error([
    `Native Wattetheria binaries were not found for ${process.platform}-${process.arch}.`,
    checked.length > 0 ? `Checked:\n${checked.map((entry) => `  - ${entry}`).join("\n")}` : "",
    "Run `wattetheria install --runtime native` to download the matching native release,",
    "or set WATTETHERIA_NATIVE_BIN_DIR to a directory containing:",
    `  ${Object.values(NATIVE_BINARY_BASE_NAMES).map((name) => nativeBinaryName(name)).join(", ")}`
  ].filter(Boolean).join("\n"));
}

function runDir(dir) {
  return path.join(dir, "run");
}

function logDir(dir) {
  return path.join(dir, "logs");
}

function pidFilePath(dir) {
  return path.join(runDir(dir), "daemon.pid");
}

function stateFilePath(dir) {
  return path.join(runDir(dir), "state.json");
}

function stopRequestPath(dir) {
  return path.join(runDir(dir), "stop.request");
}

function logFilePath(dir, name) {
  return path.join(logDir(dir), `${name}.log`);
}

// A native deployment whose env file is gone still leaves its supervisor
// state and logs behind.
function hasNativeArtifacts(dir) {
  return fs.existsSync(runDir(dir)) || fs.existsSync(logDir(dir));
}

function envValue(env, key, fallback) {
  const value = env.get(key);
  return value && value.trim() ? value.trim() : fallback;
}

function connectHost(host) {
  return host === "0.0.0.0" || host === "::" || host === "[::]" ? "127.0.0.1" : host;
}

function endpoint(env, hostKey, portKey, defaultPort) {
  const host = envValue(env, hostKey, "127.0.0.1");
  const port = envValue(env, portKey, defaultPort);
  return {
    listen: `${host}:${port}`,
    url: `http://${connectHost(host)}:${port}`
  };
}

function nativeEndpoints(env) {
  return {
    controlPlane: endpoint(env, "WATTETHERIA_CONTROL_PLANE_BIND_HOST", "WATTETHERIA_CONTROL_PLANE_PORT", "7777"),
    wattswarmUi: endpoint(env, "WATTSWARM_UI_BIND_HOST", "WATTSWARM_UI_PORT", "7788"),
    syncGrpc: endpoint(env, "WATTSWARM_SYNC_GRPC_BIND_HOST", "WATTSWARM_SYNC_GRPC_PORT", "7791"),
    runtime: endpoint(env, "WATTSWARM_RUNTIME_BIND_HOST", "WATTSWARM_RUNTIME_PORT", "8787")
  };
}

// Mirrors scripts/docker-kernel-entrypoint.sh so native and Docker kernels
// receive the same flags from the same deployment env keys.
function kernelArgs(config) {
  const { env, wattetheriaDataDir, wattswarmStateDir } = config;
  const endpoints = nativeEndpoints(env);
  const args = [
    "--data-dir", wattetheriaDataDir,
    "--control-plane-bind", endpoints.controlPlane.listen,
    "--autonomy-interval-sec", envValue(env, "WATTETHERIA_AUTONOMY_INTERVAL_SEC", "30")
  ];
  if (envValue(env, "WATTETHERIA_AUTONOMY_ENABLED", "false") === "true") {
    args.push("--autonomy-enabled");
  }
  const optionalFlags = [
    ["WATTETHERIA_BRAIN_PROVIDER_KIND", "--brain-provider-kind"],
    ["WATTETHERIA_BRAIN_BASE_URL", "--brain-base-url"],
    ["WATTETHERIA_BRAIN_MODEL", "--brain-model"],
    ["WATTETHERIA_BRAIN_API_KEY_ENV", "--brain-api-key-env"],
    ["WATTETHERIA_BRAIN_RUNTIME_ADAPTER", "--brain-runtime-adapter"],
    ["WATTETHERIA_BRAIN_SESSION_HEADER_NAME", "--brain-session-header-name"],
    ["WATTETHERIA_BRAIN_SESSION_MODE", "--brain-runtime-session-mode"]
  ];
  for (const [key, flag] of optionalFlags) {
    const value = envValue(env, key, "");
    if (value) {
      args.push(flag, value);
    }
  }
  args.push(
    "--wattswarm-ui-base-url", endpoints.wattswarmUi.url,
    "--wattswarm-sync-grpc-endpoint", endpoints.syncGrpc.url,
    "--wattswarm-agent-event-callback-base-url", endpoints.controlPlane.url,
    "--agent-control-plane-endpoint", endpoints.controlPlane.url,
    "--agent-wattswarm-ui-base-url", endpoints.wattswarmUi.url,
    "--agent-wattswarm-sync-grpc-endpoint", endpoints.syncGrpc.url,
    "--agent-host-data-dir", wattetheriaDataDir,
    "--gateway-config-path", path.join(wattswarmStateDir, "startup_config.json")
  );
  if (envValue(env, "WATTETHERIA_MCP_TOKEN_AUTH", "false") === "true") {
    args.push("--mcp-token-auth-required");
  }
  for (const gatewayUrl of envValue(env, "WATTETHERIA_GATEWAY_URLS", "").split(",")) {
    if (gatewayUrl.trim()) {
      args.push("--gateway-url", gatewayUrl.trim());
    }
  }
  return args;
}

function wattswarmArgs(config, ...args) {
  return ["--state-dir", config.wattswarmStateDir, "--store", WATTSWARM_STORE_NAME, ...args];
}

function processEnv(config, controlToken) {
  const endpoints = nativeEndpoints(config.env);
  return {
    ...process.env,
    ...Object.fromEntries(config.env),
    WATTSWARM_STORAGE_BACKEND: "sqlite",
    WATTSWARM_STATE_DIR: config.wattswarmStateDir,
    WATTSWARM_STORE_NAME,
    WATTSWARM_UI_LISTEN: endpoints.wattswarmUi.listen,
    WATTSWARM_WATTETHERIA_SYNC_GRPC_LISTEN: endpoints.syncGrpc.listen,
    WATTSWARM_DISCOVERY_AGENT_CARD_URL: endpoints.controlPlane.url,
    // Docker mounts the Wattetheria state at /var/lib/wattetheria so Wattswarm
    // can read control.token; natively the token is handed over explicitly.
    WATTSWARM_DISCOVERY_AGENT_CARD_TOKEN: controlToken,
    WATTETHERIA_RUNTIME_ENV_FILE: config.envFilePath
  };
}

function nativeServices(config) {
  const endpoints = nativeEndpoints(config.env);
  const env = config.env;
  const identityArgs = nativeServiceIdentityArgs(config);
  return [
    {
      name: "wattswarm-runtime",
      command: config.binaries.runtime,
      args: ["--listen", endpoints.runtime.listen],
      identityArgs: identityArgs["wattswarm-runtime"],
      readyUrl: `${endpoints.runtime.url}/health`
    },
    {
      name: "wattswarm-kernel",
      command: config.binaries.wattswarm,
      preSteps: [
        wattswarmArgs(config, "run", "init"),
        wattswarmArgs(config, "executors", "add", BOOTSTRAP_EXECUTOR_NAME, endpoints.runtime.url)
      ],
      args: wattswarmArgs(config, "ui", "--listen", endpoints.wattswarmUi.listen),
      identityArgs: identityArgs["wattswarm-kernel"]
    },
    {
      name: "wattswarm-worker",
      command: config.binaries.wattswarm,
      args: wattswarmArgs(
        config,
        "run",
        "worker",
        "--concurrency", envValue(env, "WATTSWARM_WORKER_CONCURRENCY", "16"),
        "--poll-ms", envValue(env, "WATTSWARM_WORKER_POLL_MS", "250"),
        "--lease-ms", envValue(env, "WATTSWARM_WORKER_LEASE_MS", "30000")
      ),
      identityArgs: identityArgs["wattswarm-worker"],
      // The kernel owns the iroh data plane; a second stack would contend on
      // the shared state directory's exclusive locks.
      envOverrides: { WATTSWARM_P2P_ENABLED: "false" }
    },
    {
      name: "kernel",
      command: config.binaries.kernel,
      args: kernelArgs(config),
      identityArgs: identityArgs.kernel
    }
  ];
}

function nativeServiceIdentityArgs(config) {
  const endpoints = nativeEndpoints(config.env);
  return {
    "wattswarm-runtime": ["--listen", endpoints.runtime.listen],
    "wattswarm-kernel": ["--state-dir", config.wattswarmStateDir],
    "wattswarm-worker": ["--state-dir", config.wattswarmStateDir],
    kernel: ["--data-dir", config.wattetheriaDataDir]
  };
}

function nativeServiceIdentities(config) {
  return Object.entries(nativeServiceIdentityArgs(config))
    .map(([name, args]) => ({ name, identityArgs: args }));
}

function ensureControlToken(wattetheriaDataDir) {
  const tokenPath = path.join(wattetheriaDataDir, "control.token");
  if (fs.existsSync(tokenPath)) {
    const existing = fs.readFileSync(tokenPath, "utf8").trim();
    if (existing) {
      return existing;
    }
  }
  const token = crypto.randomUUID();
  fs.mkdirSync(wattetheriaDataDir, { recursive: true });
  fs.writeFileSync(tokenPath, token, { mode: 0o600 });
  return token;
}

function isPidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) {
    return false;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

function readJsonFile(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, "utf8"));
  } catch (_error) {
    return null;
  }
}

function readDaemonPid(dir) {
  try {
    return Number.parseInt(fs.readFileSync(pidFilePath(dir), "utf8").trim(), 10);
  } catch (_error) {
    return 0;
  }
}

function runningDaemonPid(dir) {
  const pid = readDaemonPid(dir);
  return isPidAlive(pid) ? pid : 0;
}

function rotateLog(filePath) {
  try {
    if (fs.statSync(filePath).size > LOG_ROTATE_BYTES) {
      fs.renameSync(filePath, `${filePath}.1`);
    }
  } catch (_error) {
    // Missing log files need no rotation.
  }
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForUrl(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(2000) });
      if (response.ok) {
        return true;
      }
    } catch (_error) {
      // Not listening yet.
    }
    await delay(STOP_POLL_MS);
  }
  return false;
}

class NativeSupervisor {
  constructor(config) {
    this.config = config;
    this.dir = config.dir;
    this.stopping = false;
    this.services = new Map();
    this.state = {
      daemonPid: process.pid,
      startedAt: new Date().toISOString(),
      services: {}
    };
  }

  log(message) {
    console.log(`[${new Date().toISOString()}] ${message}`);
  }

  writeState() {
    fs.writeFileSync(stateFilePath(this.dir), `${JSON.stringify(this.state, null, 2)}\n`);
  }

  updateService(name, patch) {
    this.state.services[name] = { ...(this.state.services[name] || {}), ...patch };
    this.writeState();
  }

  spawnLogged(service, args) {
    const logPath = logFilePath(this.dir, service.name);
    const fd = fs.openSync(logPath, "a");
    try {
      return spawn(service.command, args, {
        env: { ...this.env, ...(service.envOverrides || {}) },
        stdio: ["ignore", fd, fd],
        windowsHide: true
      });
    } finally {
      fs.closeSync(fd);
    }
  }

  runStep(service, args) {
    return new Promise((resolve) => {
      const child = this.spawnLogged(service, args);
      child.once("error", (error) => {
        this.log(`${service.name} step failed to spawn: ${error.message}`);
        resolve(1);
      });
      child.once("exit", (code) => resolve(code ?? 1));
    });
  }

  async launch(service, entry) {
    for (const step of service.preSteps || []) {
      const code = await this.runStep(service, step);
      if (this.stopping) {
        return;
      }
      if (code !== 0) {
        this.log(`${service.name} setup step '${step.slice(4).join(" ")}' exited with ${code}`);
        this.scheduleRestart(service, entry, code);
        return;
      }
    }

    const child = this.spawnLogged(service, service.args);
    entry.child = child;
    entry.startedAt = 0;
    entry.exited = new Promise((resolve) => {
      let finished = false;
      const finish = (exitStatus, reason) => {
        if (finished) {
          return;
        }
        finished = true;
        entry.child = null;
        resolve();
        if (this.stopping) {
          return;
        }
        if (entry.startedAt && Date.now() - entry.startedAt >= STABLE_RUN_MS) {
          entry.failures = 0;
        }
        this.log(`${service.name} ${reason}`);
        this.scheduleRestart(service, entry, exitStatus);
      };

      child.once("spawn", () => {
        entry.startedAt = Date.now();
        this.updateService(service.name, {
          pid: child.pid,
          status: "running",
          restarts: entry.restarts,
          startedAt: new Date(entry.startedAt).toISOString(),
          identityArgs: service.identityArgs
        });
        this.log(`${service.name} started (pid ${child.pid})`);
      });
      child.once("error", (error) => {
        // A process that never started emits only error and close, never exit.
        if (child.pid === undefined) {
          finish(error.code || "spawn_error", `failed to start: ${error.message}`);
        } else {
          this.log(`${service.name} process error: ${error.message}`);
        }
      });
      child.once("exit", (code, signal) => {
        const ranFor = Math.round((Date.now() - entry.startedAt) / 1000);
        finish(code ?? signal, `exited (${signal || `code ${code}`}) after ${ranFor}s`);
      });
    });
  }

  scheduleRestart(service, entry, exitStatus) {
    const waitMs = Math.min(RESTART_MAX_DELAY_MS, RESTART_BASE_DELAY_MS * 2 ** entry.failures);
    entry.failures += 1;
    entry.restarts += 1;
    this.updateService(service.name, {
      pid: null,
      status: "restarting",
      restarts: entry.restarts,
      lastExit: exitStatus
    });
    entry.timer = setTimeout(() => {
      entry.timer = null;
      if (!this.stopping) {
        this.launch(service, entry);
      }
    }, waitMs);
  }

  async start() {
    fs.mkdirSync(runDir(this.dir), { recursive: true });
    fs.mkdirSync(logDir(this.dir), { recursive: true });
    fs.mkdirSync(this.config.wattswarmStateDir, { recursive: true });
    fs.rmSync(stopRequestPath(this.dir), { force: true });
    const orphans = stopOrphanServices(this.dir, nativeServiceIdentities(this.config));
    if (orphans > 0) {
      this.log(`stopped ${orphans} orphaned service process(es) from a previous supervisor`);
    }
    for (const name of SERVICE_NAMES) {
      rotateLog(logFilePath(this.dir, name));
    }
    fs.writeFileSync(pidFilePath(this.dir), `${process.pid}\n`);
    this.env = processEnv(this.config, ensureControlToken(this.config.wattetheriaDataDir));
    this.writeState();
    this.log(`native supervisor started (pid ${process.pid}) for ${this.dir}`);

    for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
      process.on(signal, () => this.shutdown(signal));
    }
    this.stopPoll = setInterval(() => {
      if (fs.existsSync(stopRequestPath(this.dir))) {
        this.shutdown("stop request");
      }
    }, STOP_POLL_MS);

    for (const service of nativeServices(this.config)) {
      if (this.stopping) {
        return;
      }
      const entry = { child: null, timer: null, restarts: 0, failures: 0, startedAt: 0 };
      this.services.set(service.name, entry);
      this.updateService(service.name, {
        pid: null,
        status: "starting",
        restarts: 0,
        identityArgs: service.identityArgs
      });
      await this.launch(service, entry);
      if (service.readyUrl && !(await waitForUrl(service.readyUrl, RUNTIME_READY_TIMEOUT_MS))) {
        this.log(`${service.name} is not ready at ${service.readyUrl}; continuing startup`);
      }
    }
  }

  stopChild(name, entry) {
    if (entry.timer) {
      clearTimeout(entry.timer);
      entry.timer = null;
    }
    const child = entry.child;
    if (!child) {
      return Promise.resolve();
    }
    const forceKill = setTimeout(() => {
      this.log(`${name} did not exit in time; killing`);
      child.kill("SIGKILL");
    }, CHILD_STOP_TIMEOUT_MS);
    child.kill("SIGTERM");
    return entry.exited.finally(() => clearTimeout(forceKill));
  }

  async shutdown(reason) {
    if (this.stopping) {
      return;
    }
    this.stopping = true;
    clearInterval(this.stopPoll);
    this.log(`stopping native services (${reason})`);
    for (const name of [...SERVICE_NAMES].reverse()) {
      const entry = this.services.get(name);
      if (entry) {
        await this.stopChild(name, entry);
        this.updateService(name, { pid: null, status: "stopped" });
      }
    }
    fs.rmSync(pidFilePath(this.dir), { force: true });
    fs.rmSync(stopRequestPath(this.dir), { force: true });
    this.log("native supervisor stopped");
    process.exit(0);
  }
}

async function runDaemon(config) {
  const existing = runningDaemonPid(config.dir);
  if (existing && existing !== process.pid) {
    throw new Error(`Native supervisor is already running (pid ${existing}).`);
  }
  await new NativeSupervisor(config).start();
}

async function spawnDaemon(dir, daemonCommand) {
  fs.mkdirSync(logDir(dir), { recursive: true });
  const daemonLog = logFilePath(dir, "daemon");
  rotateLog(daemonLog);
  const fd = fs.openSync(daemonLog, "a");
  try {
    const child = spawn(daemonCommand.command, daemonCommand.args, {
      detached: true,
      stdio: ["ignore", fd, fd],
      windowsHide: true
    });
    child.unref();
  } finally {
    fs.closeSync(fd);
  }

  const deadline = Date.now() + DAEMON_START_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const pid = runningDaemonPid(dir);
    if (pid) {
      return pid;
    }
    await delay(200);
  }
  throw new Error(
    `Native supervisor did not start within ${DAEMON_START_TIMEOUT_MS / 1000}s. See ${daemonLog}`
  );
}

function processImageName(pid) {
  const result = process.platform === "win32"
    ? spawnSync("tasklist", ["/FI", `PID eq ${pid}`, "/FO", "CSV", "/NH"], {
        encoding: "utf8",
        windowsHide: true
      })
    : spawnSync("ps", ["-p", String(pid), "-o", "comm="], { encoding: "utf8" });
  if (result.status !== 0 || !result.stdout) {
    return "";
  }
  const output = result.stdout.trim();
  const image = process.platform === "win32" ? (/^"([^"]+)"/.exec(output) || [])[1] || "" : output;
  return path.basename(image).replace(/\.exe$/i, "");
}

// Guards against killing an unrelated process that reused a recorded pid.
function processCommandLine(pid) {
  const result = process.platform === "win32"
    ? spawnSync("powershell.exe", [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        `[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding; $process = Get-CimInstance Win32_Process -Filter 'ProcessId = ${pid}'; if ($process) { $process.CommandLine }`
      ], { encoding: "utf8", windowsHide: true })
    : spawnSync("ps", ["-ww", "-p", String(pid), "-o", "args="], { encoding: "utf8" });
  return result.status === 0 ? String(result.stdout || "").trim() : "";
}

function commandLineMatchesIdentity(commandLine, identityArgs) {
  if (!commandLine || !Array.isArray(identityArgs) || identityArgs.length < 2 || identityArgs.length % 2 !== 0) {
    return false;
  }
  const normalize = (value) => process.platform === "win32" ? value.toLowerCase() : value;
  const normalizedCommandLine = ` ${normalize(commandLine.replace(/["']/g, ""))} `;
  for (let index = 0; index < identityArgs.length; index += 2) {
    const flag = normalize(String(identityArgs[index]));
    const value = normalize(String(identityArgs[index + 1]));
    if (!normalizedCommandLine.includes(` ${flag} ${value} `)) {
      return false;
    }
  }
  return true;
}

function isNativeServiceProcess(pid, identityArgs) {
  const name = processImageName(pid);
  const isNativeBinary = [...NATIVE_BINARY_NAMES].some((binary) => name === binary
    || (name.length >= TRUNCATED_COMM_LENGTH && binary.startsWith(name)));
  return isNativeBinary && commandLineMatchesIdentity(processCommandLine(pid), identityArgs);
}

// Service processes outlive a supervisor that exits abnormally, because
// Windows does not terminate children with their parent.
function stopOrphanServices(dir, expectedServices = []) {
  const state = readJsonFile(stateFilePath(dir));
  const expectedIdentities = new Map(
    expectedServices.map((service) => [service.name, service.identityArgs])
  );
  let stopped = 0;
  for (const [name, service] of Object.entries(state?.services || {})) {
    const identityArgs = service.identityArgs || expectedIdentities.get(name);
    if (identityArgs && forceKill(service.pid, identityArgs)) {
      stopped += 1;
    }
  }
  return stopped;
}

function forceKill(pid, identityArgs) {
  if (!isPidAlive(pid) || (identityArgs && !isNativeServiceProcess(pid, identityArgs))) {
    return false;
  }
  try {
    process.kill(pid, "SIGKILL");
    return true;
  } catch (_error) {
    return false;
  }
}

async function stopDaemon(dir, expectedServices = []) {
  const pid = runningDaemonPid(dir);
  if (!pid) {
    const orphans = stopOrphanServices(dir, expectedServices);
    fs.rmSync(pidFilePath(dir), { force: true });
    return orphans > 0;
  }
  fs.mkdirSync(runDir(dir), { recursive: true });
  fs.writeFileSync(stopRequestPath(dir), `${new Date().toISOString()}\n`);
  const deadline = Date.now() + DAEMON_STOP_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (!isPidAlive(pid)) {
      stopOrphanServices(dir, expectedServices);
      return true;
    }
    await delay(STOP_POLL_MS);
  }

  forceKill(pid);
  stopOrphanServices(dir, expectedServices);
  fs.rmSync(pidFilePath(dir), { force: true });
  fs.rmSync(stopRequestPath(dir), { force: true });
  return true;
}

function formatDuration(isoTime) {
  const seconds = Math.max(0, Math.round((Date.now() - Date.parse(isoTime)) / 1000));
  if (seconds < 60) {
    return `${seconds}s`;
  }
  if (seconds < 3600) {
    return `${Math.floor(seconds / 60)}m`;
  }
  return `${Math.floor(seconds / 3600)}h${Math.floor((seconds % 3600) / 60)}m`;
}

function nativeStatusRows(dir) {
  const daemonPid = runningDaemonPid(dir);
  const state = daemonPid ? readJsonFile(stateFilePath(dir)) : null;
  return {
    daemonPid,
    rows: SERVICE_NAMES.map((name) => {
      const service = state?.services?.[name] || {};
      const alive = daemonPid && isPidAlive(service.pid);
      return {
        name,
        status: alive ? "running" : (daemonPid ? service.status || "starting" : "stopped"),
        pid: alive ? String(service.pid) : "-",
        restarts: String(service.restarts || 0),
        uptime: alive && service.startedAt ? formatDuration(service.startedAt) : "-"
      };
    })
  };
}

function isNativeStackRunning(dir) {
  const { daemonPid, rows } = nativeStatusRows(dir);
  return daemonPid !== 0 && rows.every((row) => row.status === "running");
}

function printNativeStatus(dir) {
  const { daemonPid, rows } = nativeStatusRows(dir);
  console.log(daemonPid ? `Native supervisor: running (pid ${daemonPid})` : "Native supervisor: stopped");
  const header = { name: "SERVICE", status: "STATUS", pid: "PID", restarts: "RESTARTS", uptime: "UPTIME" };
  const widths = Object.fromEntries(
    Object.keys(header).map((key) => [key, Math.max(...[header, ...rows].map((row) => row[key].length))])
  );
  for (const row of [header, ...rows]) {
    console.log(Object.keys(header).map((key) => row[key].padEnd(widths[key])).join("  ").trimEnd());
  }
}

function parseLogArgs(args) {
  const selection = { follow: false, tail: DEFAULT_LOG_TAIL_LINES, names: [] };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "-f" || arg === "--follow") {
      selection.follow = true;
    } else if (arg === "-n" || arg === "--tail") {
      const value = Number.parseInt(args[++index], 10);
      if (!Number.isInteger(value) || value < 0) {
        throw new Error(`Invalid value for ${arg}`);
      }
      selection.tail = value;
    } else if (LOG_NAMES.includes(arg)) {
      selection.names.push(arg);
    } else {
      throw new Error(`Unknown logs argument: ${arg}. Services: ${LOG_NAMES.join(", ")}`);
    }
  }
  if (selection.names.length === 0) {
    selection.names = [...LOG_NAMES];
  }
  return selection;
}

function printLogLines(name, text, prefix) {
  for (const line of text.split(/\r?\n/)) {
    if (line) {
      console.log(prefix ? `${name.padEnd(16)} | ${line}` : line);
    }
  }
}

async function showNativeLogs(dir, args) {
  const selection = parseLogArgs(args);
  const prefix = selection.names.length > 1;
  const offsets = new Map();
  for (const name of selection.names) {
    const filePath = logFilePath(dir, name);
    if (!fs.existsSync(filePath)) {
      offsets.set(name, 0);
      continue;
    }
    const content = fs.readFileSync(filePath, "utf8");
    offsets.set(name, Buffer.byteLength(content));
    if (selection.tail > 0) {
      printLogLines(name, content.split(/\r?\n/).filter(Boolean).slice(-selection.tail).join("\n"), prefix);
    }
  }
  while (selection.follow) {
    await delay(1000);
    for (const name of selection.names) {
      const filePath = logFilePath(dir, name);
      if (!fs.existsSync(filePath)) {
        continue;
      }
      const size = fs.statSync(filePath).size;
      const offset = size < offsets.get(name) ? 0 : offsets.get(name);
      if (size > offset) {
        const fd = fs.openSync(filePath, "r");
        const buffer = Buffer.alloc(size - offset);
        fs.readSync(fd, buffer, 0, buffer.length, offset);
        fs.closeSync(fd);
        printLogLines(name, buffer.toString("utf8"), prefix);
      }
      offsets.set(name, size);
    }
  }
}

module.exports = {
  commandLineMatchesIdentity,
  kernelArgs,
  nativeEndpoints,
  nativeServiceIdentities,
  nativeServices,
  hasNativeArtifacts,
  isNativeStackRunning,
  logDir,
  printNativeStatus,
  resolveNativeBinaries,
  runDaemon,
  runningDaemonPid,
  showNativeLogs,
  spawnDaemon,
  stopDaemon
};

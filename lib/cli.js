const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { createInterface } = require("node:readline/promises");

const PACKAGE_ROOT = path.resolve(__dirname, "..");
const PACKAGE_JSON = require(path.join(PACKAGE_ROOT, "package.json"));
const RELEASE_ENV_TEMPLATE = path.join(PACKAGE_ROOT, ".env.release.example");
const DEFAULT_DEPLOY_DIR = path.join(os.homedir(), ".wattetheria", "deploy");
const DEFAULT_PROJECT_NAME = "wattetheria";
const DEFAULT_COMMAND = "install";
const IMAGE_KEYS = [
  "WATTETHERIA_KERNEL_IMAGE",
  "WATTETHERIA_OBSERVATORY_IMAGE",
  "WATTSWARM_KERNEL_IMAGE",
  "WATTSWARM_RUNTIME_IMAGE",
  "WATTSWARM_WORKER_IMAGE"
];
const HOST_STATE_DIR_KEYS = ["WATTETHERIA_HOST_STATE_DIR", "WATTSWARM_HOST_STATE_DIR"];
const DOCKER_INSTALL_URLS = {
  darwin: "https://www.docker.com/products/docker-desktop/",
  win32: "https://www.docker.com/products/docker-desktop/",
  linux: "https://docs.docker.com/engine/install/"
};
const WINDOWS_DOCKER_CANDIDATES = [
  "C:\\Program Files\\Docker\\Docker\\resources\\bin\\docker.exe",
  "C:\\Program Files\\Docker\\cli-plugins\\docker.exe"
];

function printHelp() {
  console.log(`Wattetheria CLI ${PACKAGE_JSON.version}

Usage:
  npx wattetheria [command] [options]
  npx wattetheria install

Commands:
  install     Prepare deployment, pull images, and start the stack
  start       Start an existing deployment
  status      Show docker compose status
  update      Update image tags, pull, and restart
  stop        Stop the deployment
  uninstall   Stop the deployment and optionally remove volumes
  logs        Show docker compose logs
  doctor      Check local prerequisites
  help        Show this help

Options:
  --version              Show CLI version
  --dir <path>           Deployment directory (default: ${DEFAULT_DEPLOY_DIR})
  --project-name <name>  Docker compose project name (default: ${DEFAULT_PROJECT_NAME})
  --tag <tag>            Override all release image tags
  --force                Refresh deployment templates from package assets
  --no-health-checks     Skip HTTP health checks
  --volumes              With uninstall, remove named docker volumes
  --purge                With uninstall, remove the deployment directory
`);
}

function parseArgs(argv) {
  if (argv[0] === "--version" || argv[0] === "-v") {
    return {
      command: "version",
      options: {
        dir: DEFAULT_DEPLOY_DIR,
        projectName: DEFAULT_PROJECT_NAME,
        tag: null,
        force: false,
        healthChecks: true,
        volumes: false,
        purge: false,
        composeArgs: []
      }
    };
  }

  let command = DEFAULT_COMMAND;
  let index = 0;
  if (argv[0] && !argv[0].startsWith("-")) {
    command = argv[0];
    index = 1;
  }

  const options = {
    dir: DEFAULT_DEPLOY_DIR,
    projectName: DEFAULT_PROJECT_NAME,
    tag: null,
    force: false,
    healthChecks: true,
    volumes: false,
    purge: false,
    composeArgs: []
  };

  while (index < argv.length) {
    const arg = argv[index];
    if (arg === "--dir") {
      options.dir = requireValue(arg, argv[++index]);
    } else if (arg === "--project-name") {
      options.projectName = requireValue(arg, argv[++index]);
    } else if (arg === "--tag") {
      options.tag = requireValue(arg, argv[++index]);
    } else if (arg === "--force") {
      options.force = true;
    } else if (arg === "--no-health-checks") {
      options.healthChecks = false;
    } else if (arg === "--volumes") {
      options.volumes = true;
    } else if (arg === "--purge") {
      options.purge = true;
    } else if (arg === "--") {
      options.composeArgs = argv.slice(index + 1);
      break;
    } else if (command === "logs") {
      options.composeArgs.push(arg);
    } else if (command === "help") {
      break;
    } else {
      throw new Error(`Unknown option: ${arg}`);
    }
    index += 1;
  }

  return { command, options };
}

function requireValue(flag, value) {
  if (!value || value.startsWith("-")) {
    throw new Error(`Missing value for ${flag}`);
  }
  return value;
}

function getDockerInstallUrl() {
  return DOCKER_INSTALL_URLS[process.platform] || DOCKER_INSTALL_URLS.linux;
}

function getDockerCandidates() {
  if (process.platform === "win32") {
    return ["docker", ...WINDOWS_DOCKER_CANDIDATES];
  }
  return ["docker"];
}

function resolveDockerCommand() {
  for (const candidate of getDockerCandidates()) {
    const result = spawnSync(candidate, ["--version"], { stdio: "ignore" });
    if (!result.error && result.status === 0) {
      return candidate;
    }
  }
  return "";
}

function getGitRevision() {
  if (typeof PACKAGE_JSON.gitHead === "string" && PACKAGE_JSON.gitHead.trim()) {
    return PACKAGE_JSON.gitHead.trim();
  }

  const result = spawnSync("git", ["rev-parse", "--short=7", "HEAD"], {
    cwd: PACKAGE_ROOT,
    stdio: "pipe",
    encoding: "utf8"
  });
  if (result.error || result.status !== 0) {
    return "";
  }
  return (result.stdout || "").trim();
}

function extractImageTag(imageRef) {
  if (!imageRef) {
    return "";
  }
  const lastColon = imageRef.lastIndexOf(":");
  const lastSlash = imageRef.lastIndexOf("/");
  if (lastColon <= lastSlash) {
    return "";
  }
  return imageRef.slice(lastColon + 1).trim();
}

function getDefaultReleaseVersion() {
  try {
    const envMap = readEnvFile(RELEASE_ENV_TEMPLATE);
    const tags = IMAGE_KEYS
      .map((key) => extractImageTag(envMap.get(key)))
      .filter(Boolean);
    if (tags.length === IMAGE_KEYS.length && new Set(tags).size === 1) {
      return tags[0];
    }
  } catch (error) {
    // fall through to package version
  }
  return PACKAGE_JSON.version;
}

function formatVersionString() {
  const revision = getGitRevision();
  const releaseVersion = getDefaultReleaseVersion();
  if (revision) {
    return `Wattetheria ${releaseVersion} (${revision})`;
  }
  return `Wattetheria ${releaseVersion}`;
}

function formatBanner() {
  return `${formatVersionString()} — Local agent runtime with swarm sync and external agent reach.`;
}

function formatDockerStatusMessage(status) {
  const installUrl = status.installUrl || getDockerInstallUrl();
  switch (status.code) {
    case "missing-docker":
      if (process.platform === "linux") {
        return [
          "Docker runtime not found.",
          "Install Docker Engine or another Docker-compatible runtime, then run the command again.",
          `Install guide: ${installUrl}`
        ].join("\n");
      }
      return [
        "Docker runtime not found.",
        "Install Docker Desktop or another Docker-compatible runtime, then run the command again.",
        process.platform === "win32"
          ? "If you just installed Docker Desktop, open a new PowerShell window and retry."
          : "",
        `Download: ${installUrl}`
      ].filter(Boolean).join("\n");
    case "missing-compose":
      return [
        "Docker Compose v2 is required.",
        process.platform === "linux"
          ? "Install or upgrade Docker Engine/Compose so `docker compose` is available."
          : "Install or upgrade Docker Desktop so `docker compose` is available.",
        `Help: ${installUrl}`
      ].join("\n");
    case "daemon-unreachable":
      return [
        "Docker is installed but the daemon is not reachable.",
        "Start Docker Desktop or your Docker service, wait until it is ready, then retry."
      ].join("\n");
    default:
      return "Docker runtime check failed.";
  }
}

function getDockerStatus() {
  const installUrl = getDockerInstallUrl();
  const dockerCommand = resolveDockerCommand();
  if (!dockerCommand) {
    return {
      ready: false,
      code: "missing-docker",
      installUrl
    };
  }

  const compose = spawnSync(dockerCommand, ["compose", "version"], {
    stdio: "ignore"
  });
  if (compose.error || compose.status !== 0) {
    return {
      ready: false,
      code: "missing-compose",
      installUrl,
      dockerCommand
    };
  }

  const info = spawnSync(dockerCommand, ["info"], { stdio: "ignore" });
  if (info.error || info.status !== 0) {
    return {
      ready: false,
      code: "daemon-unreachable",
      installUrl,
      dockerCommand
    };
  }

  return {
    ready: true,
    dockerCommand
  };
}

function isInteractiveTerminal() {
  return Boolean(process.stdin.isTTY && process.stdout.isTTY);
}

function openUrl(url) {
  let command;
  let args;
  if (process.platform === "darwin") {
    command = "open";
    args = [url];
  } else if (process.platform === "win32") {
    command = "cmd";
    args = ["/c", "start", "", url];
  } else {
    command = "xdg-open";
    args = [url];
  }

  const result = spawnSync(command, args, { stdio: "ignore" });
  return !result.error && result.status === 0;
}

async function promptForDockerSetup(status) {
  const installUrl = status.installUrl || getDockerInstallUrl();
  const rl = createInterface({
    input: process.stdin,
    output: process.stdout
  });

  try {
    while (true) {
      console.log("");
      console.log("Wattetheria needs Docker before it can install the local stack.");
      console.log(formatDockerStatusMessage(status));
      console.log("");
      console.log("1. Open runtime install guide");
      console.log("2. Retry Docker check");
      console.log("3. Cancel");

      const answer = (await rl.question("Select an option [1-3]: ")).trim();
      if (answer === "1") {
        const opened = openUrl(installUrl);
        console.log(opened ? "Opened Docker install page." : `Open this URL in your browser: ${installUrl}`);
        continue;
      }
      if (answer === "2") {
        return;
      }
      if (answer === "3" || answer === "") {
        throw new Error(formatDockerStatusMessage(status));
      }
      console.log("Please choose 1, 2, or 3.");
    }
  } finally {
    rl.close();
  }
}

async function ensureDockerAvailable(options = {}) {
  const interactive = Boolean(options.interactive);
  while (true) {
    const status = getDockerStatus();
    if (status.ready) {
      return status;
    }
    if (!interactive || !isInteractiveTerminal()) {
      throw new Error(formatDockerStatusMessage(status));
    }
    await promptForDockerSetup(status);
  }
}

function ensureDeploymentAssets(options) {
  fs.mkdirSync(options.dir, { recursive: true });

  const templateEnvPath = path.join(PACKAGE_ROOT, ".env.release.example");
  const templateComposePath = path.join(PACKAGE_ROOT, "docker-compose.release.yml");
  const targetEnvPath = envFilePath(options);
  const targetComposePath = composeFilePath(options);

  if (options.force || !fs.existsSync(targetEnvPath)) {
    fs.copyFileSync(templateEnvPath, targetEnvPath);
  }
  if (options.force || !fs.existsSync(targetComposePath)) {
    fs.copyFileSync(templateComposePath, targetComposePath);
  }

  const envMap = readEnvFile(targetEnvPath);
  envMap.set("WATTETHERIA_RUNTIME_ENV_FILE", path.basename(targetEnvPath));
  ensureDatabasePassword(envMap);
  if (options.tag) {
    pinImageTags(envMap, options.tag);
  }
  ensureHostStateDirectories(options.dir, envMap);
  writeEnvFile(targetEnvPath, envMap);
}

function envFilePath(options) {
  return path.join(options.dir, ".env");
}

function composeFilePath(options) {
  return path.join(options.dir, "docker-compose.yml");
}

function readEnvFile(filePath) {
  const envMap = new Map();
  const content = fs.readFileSync(filePath, "utf8");
  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) {
      continue;
    }
    const separator = rawLine.indexOf("=");
    if (separator < 0) {
      continue;
    }
    const key = rawLine.slice(0, separator).trim();
    const value = rawLine.slice(separator + 1);
    envMap.set(key, value);
  }
  return envMap;
}

function writeEnvFile(filePath, envMap) {
  const lines = [];
  for (const [key, value] of envMap.entries()) {
    lines.push(`${key}=${value}`);
  }
  fs.writeFileSync(filePath, `${lines.join("\n")}\n`);
}

function ensureDatabasePassword(envMap) {
  const placeholder = "replace-with-strong-password";
  const current = envMap.get("WATTSWARM_PG_PASSWORD");
  if (!current || current === placeholder) {
    envMap.set("WATTSWARM_PG_PASSWORD", randomPassword());
  }
}

function ensureHostStateDirectories(baseDir, envMap) {
  for (const key of HOST_STATE_DIR_KEYS) {
    const configured = envMap.get(key);
    if (!configured || !configured.trim()) {
      continue;
    }
    const resolved = path.isAbsolute(configured)
      ? configured
      : path.resolve(baseDir, configured);
    fs.mkdirSync(resolved, { recursive: true });
  }
}

function pinImageTags(envMap, tag) {
  for (const key of IMAGE_KEYS) {
    if (!envMap.has(key)) {
      continue;
    }
    const current = envMap.get(key);
    const index = current.lastIndexOf(":");
    if (index <= current.lastIndexOf("/")) {
      envMap.set(key, `${current}:${tag}`);
    } else {
      envMap.set(key, `${current.slice(0, index + 1)}${tag}`);
    }
  }
}

function randomPassword() {
  return crypto
    .randomBytes(24)
    .toString("base64")
    .replace(/\+/g, "A")
    .replace(/\//g, "B")
    .replace(/=/g, "");
}

function runCompose(options, args, capture = false) {
  const dockerCommand = resolveDockerCommand() || "docker";
  const result = spawnSync(
    dockerCommand,
    [
      "compose",
      "--project-name",
      options.projectName,
      "--env-file",
      envFilePath(options),
      "-f",
      composeFilePath(options),
      ...args
    ],
    {
      stdio: capture ? "pipe" : "inherit",
      encoding: capture ? "utf8" : undefined
    }
  );
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    const stderr = capture ? (result.stderr || "").trim() : "";
    if (stderr.includes("failed to resolve reference") && stderr.includes(": not found")) {
      throw new Error(
        [
          "One or more release images were not found in the container registry.",
          "This usually means the requested image tag has not been published yet.",
          "Publish the matching GHCR images first, or run the command with --tag <published-tag>."
        ].join("\n")
      );
    }
    if (capture && result.stderr) {
      throw new Error(result.stderr.trim() || `docker compose ${args.join(" ")} failed`);
    }
    throw new Error(`docker compose ${args.join(" ")} failed`);
  }
  return result;
}

async function waitForHttp(name, url, maxAttempts = 60, delayMs = 2000) {
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      const response = await fetch(url, { method: "GET" });
      if (response.ok) {
        console.log(`[ok] ${name}: ${url}`);
        return;
      }
    } catch (error) {
      // retry
    }
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  }
  throw new Error(`Timed out waiting for ${name}: ${url}`);
}

function getEnvValue(envMap, key, defaultValue) {
  const value = envMap.get(key);
  return value && value.trim() ? value : defaultValue;
}

async function runHealthChecks(options) {
  const envMap = readEnvFile(envFilePath(options));
  const kernelHost = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_BIND_HOST", "127.0.0.1");
  const kernelPort = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_PORT", "7777");
  const uiHost = getEnvValue(envMap, "WATTSWARM_UI_BIND_HOST", "127.0.0.1");
  const uiPort = getEnvValue(envMap, "WATTSWARM_UI_PORT", "7788");
  const observatoryHost = getEnvValue(envMap, "WATTETHERIA_OBSERVATORY_BIND_HOST", "127.0.0.1");
  const observatoryPort = getEnvValue(envMap, "WATTETHERIA_OBSERVATORY_PORT", "8780");

  await waitForHttp("kernel health", `http://${kernelHost}:${kernelPort}/v1/health`);
  await waitForHttp("wattswarm ui", `http://${uiHost}:${uiPort}/`);
  await waitForHttp("observatory health", `http://${observatoryHost}:${observatoryPort}/healthz`);
}

function printSummary(options) {
  const envMap = readEnvFile(envFilePath(options));
  const kernelHost = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_BIND_HOST", "127.0.0.1");
  const kernelPort = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_PORT", "7777");
  const uiHost = getEnvValue(envMap, "WATTSWARM_UI_BIND_HOST", "127.0.0.1");
  const uiPort = getEnvValue(envMap, "WATTSWARM_UI_PORT", "7788");
  const observatoryHost = getEnvValue(envMap, "WATTETHERIA_OBSERVATORY_BIND_HOST", "127.0.0.1");
  const observatoryPort = getEnvValue(envMap, "WATTETHERIA_OBSERVATORY_PORT", "8780");

  console.log("");
  console.log("Deployment complete.");
  console.log(`Kernel:       http://${kernelHost}:${kernelPort}`);
  console.log(`Wattswarm UI: http://${uiHost}:${uiPort}`);
  console.log(`Observatory:  http://${observatoryHost}:${observatoryPort}`);
  console.log(`Deploy dir:   ${options.dir}`);
}

async function install(options) {
  await ensureDockerAvailable({ interactive: true });
  ensureDeploymentAssets(options);
  runCompose(options, ["config"], true);
  console.log("Pulling release images...");
  runCompose(options, ["pull"]);
  console.log("Starting release stack...");
  runCompose(options, ["up", "-d"]);
  if (options.healthChecks) {
    await runHealthChecks(options);
  }
  printSummary(options);
}

async function start(options) {
  await ensureDockerAvailable();
  if (!fs.existsSync(composeFilePath(options)) || !fs.existsSync(envFilePath(options))) {
    throw new Error("Deployment is not initialized. Run install first.");
  }
  runCompose(options, ["up", "-d"]);
  if (options.healthChecks) {
    await runHealthChecks(options);
  }
  printSummary(options);
}

async function status(options) {
  await ensureDockerAvailable();
  runCompose(options, ["ps"]);
}

async function update(options) {
  await ensureDockerAvailable();
  if (!fs.existsSync(composeFilePath(options)) || !fs.existsSync(envFilePath(options))) {
    throw new Error("Deployment is not initialized. Run install first.");
  }
  ensureDeploymentAssets(options);
  console.log("Pulling updated images...");
  runCompose(options, ["pull"]);
  console.log("Restarting release stack...");
  runCompose(options, ["up", "-d"]);
  if (options.healthChecks) {
    await runHealthChecks(options);
  }
  printSummary(options);
}

async function stop(options) {
  await ensureDockerAvailable();
  runCompose(options, ["down"]);
}

async function uninstall(options) {
  await ensureDockerAvailable();
  const args = ["down"];
  if (options.volumes) {
    args.push("-v");
  }
  runCompose(options, args);
  if (options.purge && fs.existsSync(options.dir)) {
    fs.rmSync(options.dir, { recursive: true, force: true });
    console.log(`Removed deployment directory: ${options.dir}`);
  }
}

async function logs(options) {
  await ensureDockerAvailable();
  runCompose(options, ["logs", ...options.composeArgs]);
}

function doctor() {
  const status = getDockerStatus();
  if (!status.ready) {
    throw new Error(formatDockerStatusMessage(status));
  }
  console.log("Docker runtime is available.");
  console.log(`Node.js ${process.version} is available.`);
}

function printVersion() {
  console.log(formatVersionString());
}

function shouldPrintBanner(command) {
  return !["help", "--help", "-h", "version"].includes(command);
}

function printBanner() {
  console.log(formatBanner());
  console.log("");
}

async function run(argv) {
  const { command, options } = parseArgs(argv);

  if (shouldPrintBanner(command)) {
    printBanner();
  }

  switch (command) {
    case "install":
      await install(options);
      return;
    case "start":
    case "up":
      await start(options);
      return;
    case "status":
      await status(options);
      return;
    case "update":
      await update(options);
      return;
    case "stop":
    case "down":
      await stop(options);
      return;
    case "uninstall":
      await uninstall(options);
      return;
    case "logs":
      await logs(options);
      return;
    case "doctor":
      doctor();
      return;
    case "version":
      printVersion();
      return;
    case "help":
    case "--help":
    case "-h":
      printHelp();
      return;
    default:
      throw new Error(`Unknown command: ${command}`);
  }
}

module.exports = {
  run
};

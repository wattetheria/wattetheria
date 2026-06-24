const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { createInterface } = require("node:readline/promises");
const readline = require("node:readline");

const PACKAGE_ROOT = path.resolve(__dirname, "..");
const PACKAGE_JSON = require(path.join(PACKAGE_ROOT, "package.json"));
const WATTETHERIA_HOME_DIR = path.join(os.homedir(), ".wattetheria");
const DEFAULT_DEPLOY_DIR = path.join(os.homedir(), ".wattetheria", "deploy");
const DEFAULT_PROJECT_NAME = "wattetheria";
const POSTGRES_VOLUME_SERVICE_NAME = "wattswarm_pg_data";
const DEFAULT_COMMAND = "help";
const RELEASE_ENV_TEMPLATE_PATH = path.join(PACKAGE_ROOT, ".env.release");
const IMAGE_KEYS = [
  "WATTETHERIA_KERNEL_IMAGE",
  "WATTSWARM_KERNEL_IMAGE",
  "WATTSWARM_RUNTIME_IMAGE",
  "WATTSWARM_WORKER_IMAGE"
];
const HOST_STATE_DIR_KEYS = ["WATTETHERIA_HOST_STATE_DIR", "WATTSWARM_HOST_STATE_DIR"];
const REGISTRY_TAG_PAGE_SIZE = 1000;
const DOCKER_INSTALL_URLS = {
  darwin: "https://www.docker.com/products/docker-desktop/",
  win32: "https://www.docker.com/products/docker-desktop/",
  linux: "https://docs.docker.com/engine/install/"
};
const WINDOWS_DOCKER_CANDIDATES = [
  "C:\\Program Files\\Docker\\Docker\\resources\\bin\\docker.exe",
  "C:\\Program Files\\Docker\\cli-plugins\\docker.exe"
];
const RUST_CLI_BASE_NAME = "wattetheria-client-cli";
const NATIVE_CLI_PACKAGE_PREFIX = "@wattetheria/cli";
const NATIVE_ARCH_ALIASES = new Map([
  ["x64", "x64"],
  ["arm64", "arm64"]
]);
const BANNER_COMMANDS = new Set([
  "help",
  "setup",
  "install",
  "start",
  "up",
  "update",
  "restart",
  "stop",
  "down",
  "uninstall"
]);
const ANSI_ORANGE = "\x1b[38;5;166m";
const ANSI_MUTED = "\x1b[38;5;244m";
const ANSI_RESET = "\x1b[0m";

function printHelp() {
  console.log(`Wattetheria CLI ${PACKAGE_JSON.version}

Usage:
  npx wattetheria [command] [options]
  npx wattetheria setup
  npx wattetheria install

Commands:
  version     Show Wattetheria release version
  images      Show configured release images
  cli update  Update the Wattetheria CLI package via npm install -g wattetheria@latest
  setup       Install the local stack and finish first-time runtime/MCP setup
  install     Prepare deployment, pull images, and start the stack
  start       Start an existing deployment
  status      Show docker compose status
  update      Resolve latest published release, pull, and restart
  restart     Recreate and restart the deployment
  stop        Stop the deployment
  uninstall   Stop the deployment and optionally remove volumes
  logs        Show docker compose logs
  mcp-proxy   Run stdio MCP proxy for the local Wattetheria node
  doctor      Run native node diagnostics
  help        Show this help

Options:
  --version, -v          Alias for \`version\`
  --cli                  With \`version\`, show deployment CLI version instead
  --images               With \`version\`, print configured image refs
  --dir <path>           Deployment directory (default: ${DEFAULT_DEPLOY_DIR})
  --project-name <name>  Docker compose project name (default: ${DEFAULT_PROJECT_NAME})
  --tag <tag>            Override all release image tags
  --force                Refresh deployment defaults and compose assets
  --no-health-checks     Skip HTTP health checks
  --volumes              With uninstall, remove named docker volumes
  --purge                With uninstall, remove ~/.wattetheria and PostgreSQL data
  --data-dir <path>      With mcp-proxy or doctor, override Wattetheria host state directory
  --control-plane <url>  With mcp-proxy or doctor, override local control-plane endpoint

Agent subcommands:
  identity
    init                         Initialize a lightweight local identity for ServiceNet publishing or wallet binding
    show                         Show the local identity public DID and public key
    export-seed                  Export the local identity seed; treat it like a password

  servicenet
    agent-card init              Generate an editable agent-card.json template in the current directory
    register --card <path-to-agent-card.json>
                                 Register the agent card and local identity with ServiceNet; returns agent_id and provider_id
    publish <agent-id>           Publish a registered ServiceNet agent using the agent_id returned by register
`);
}

function throwCommandSuggestion(argv) {
  if (argv[0] === "wallet") {
    throw new Error(
      "Unknown command: wallet. Use the local node console Wallet page or wallet control-plane routes for payment account setup."
    );
  }
  if (argv[0] === "provider" && argv[1] === "register") {
    throw new Error("Unknown command: provider register. Use `wattetheria servicenet register`.");
  }
  if (argv[0] === "register") {
    throw new Error("Unknown command: register. Use `wattetheria servicenet register`.");
  }
}

function parseArgs(argv) {
  let command = DEFAULT_COMMAND;
  let index = 0;
  if (argv[0] === "--version" || argv[0] === "-v") {
    command = "version";
    index = 1;
  } else if (argv[0] && !argv[0].startsWith("-")) {
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
    dataDir: null,
    controlPlane: null,
    composeArgs: [],
    versionTarget: "release",
    includeImages: false
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
    } else if (arg === "--cli") {
      options.versionTarget = "cli";
    } else if (arg === "--images") {
      options.includeImages = true;
    } else if (arg === "--volumes") {
      options.volumes = true;
    } else if (arg === "--purge") {
      options.purge = true;
    } else if (arg === "--data-dir") {
      options.dataDir = requireValue(arg, argv[++index]);
    } else if (arg === "--control-plane") {
      options.controlPlane = requireValue(arg, argv[++index]);
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

function stripImageTag(imageRef) {
  if (!imageRef) {
    return "";
  }
  const lastColon = imageRef.lastIndexOf(":");
  const lastSlash = imageRef.lastIndexOf("/");
  if (lastColon <= lastSlash) {
    return imageRef.trim();
  }
  return imageRef.slice(0, lastColon).trim();
}

function parseImageReference(imageRef) {
  const normalized = stripImageTag(imageRef);
  if (!normalized) {
    throw new Error(`Invalid image reference: ${imageRef}`);
  }

  const segments = normalized.split("/");
  const first = segments[0];
  const hasExplicitRegistry = first.includes(".") || first.includes(":") || first === "localhost";
  const registry = hasExplicitRegistry ? first : "registry-1.docker.io";
  const repositorySegments = hasExplicitRegistry ? segments.slice(1) : segments;
  if (repositorySegments.length === 0) {
    throw new Error(`Invalid image repository: ${imageRef}`);
  }

  if (!hasExplicitRegistry && repositorySegments.length === 1) {
    repositorySegments.unshift("library");
  }

  return {
    registry,
    repository: repositorySegments.join("/")
  };
}

function parseReleaseTag(tag) {
  const match = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/.exec((tag || "").trim());
  if (!match) {
    return null;
  }
  return {
    raw: tag,
    major: Number.parseInt(match[1], 10),
    minor: Number.parseInt(match[2], 10),
    patch: Number.parseInt(match[3], 10),
    prerelease: match[4] || ""
  };
}

function compareReleaseTags(left, right) {
  const a = parseReleaseTag(left);
  const b = parseReleaseTag(right);
  if (!a && !b) {
    return String(left).localeCompare(String(right));
  }
  if (!a) {
    return -1;
  }
  if (!b) {
    return 1;
  }
  for (const key of ["major", "minor", "patch"]) {
    if (a[key] !== b[key]) {
      return a[key] - b[key];
    }
  }
  if (!a.prerelease && b.prerelease) {
    return 1;
  }
  if (a.prerelease && !b.prerelease) {
    return -1;
  }
  return a.prerelease.localeCompare(b.prerelease);
}

function parseAuthChallenge(header) {
  if (!header) {
    return null;
  }
  const match = /^Bearer\s+(.*)$/i.exec(header.trim());
  if (!match) {
    return null;
  }
  const params = new Map();
  for (const [, key, value] of match[1].matchAll(/([a-zA-Z_]+)="([^"]*)"/g)) {
    params.set(key, value);
  }
  const realm = params.get("realm");
  if (!realm) {
    return null;
  }
  return {
    realm,
    service: params.get("service") || "",
    scope: params.get("scope") || ""
  };
}

async function fetchRegistryAccessToken(challenge) {
  const url = new URL(challenge.realm);
  if (challenge.service) {
    url.searchParams.set("service", challenge.service);
  }
  if (challenge.scope) {
    url.searchParams.set("scope", challenge.scope);
  }

  const response = await fetch(url, { method: "GET" });
  if (!response.ok) {
    throw new Error(`Failed to resolve registry token: ${response.status} ${response.statusText}`);
  }
  const payload = await response.json();
  const token = payload.token || payload.access_token;
  if (!token) {
    throw new Error("Registry token response did not contain an access token.");
  }
  return token;
}

async function fetchRegistryResponse(url, token = "") {
  const headers = {
    Accept: "application/json"
  };
  if (token) {
    headers.Authorization = `Bearer ${token}`;
  }
  const response = await fetch(url, { method: "GET", headers });
  if (response.status === 401 && !token) {
    const challenge = parseAuthChallenge(response.headers.get("www-authenticate"));
    if (!challenge) {
      throw new Error(`Registry authentication challenge is missing for ${url}`);
    }
    const resolvedToken = await fetchRegistryAccessToken(challenge);
    return fetchRegistryResponse(url, resolvedToken);
  }
  if (!response.ok) {
    throw new Error(`Registry request failed for ${url}: ${response.status} ${response.statusText}`);
  }
  return response;
}

function resolveRegistryNextPage(linkHeader, currentUrl) {
  if (!linkHeader) {
    return "";
  }
  const match = /<([^>]+)>;\s*rel="next"/i.exec(linkHeader);
  if (!match) {
    return "";
  }
  return new URL(match[1], currentUrl).toString();
}

async function listPublishedImageTags(imageRef) {
  const { registry, repository } = parseImageReference(imageRef);
  const collected = new Set();
  let nextUrl = `https://${registry}/v2/${repository}/tags/list?n=${REGISTRY_TAG_PAGE_SIZE}`;
  while (nextUrl) {
    const response = await fetchRegistryResponse(nextUrl);
    const payload = await response.json();
    for (const tag of payload.tags || []) {
      if (tag && tag.trim()) {
        collected.add(tag.trim());
      }
    }
    nextUrl = resolveRegistryNextPage(response.headers.get("link"), nextUrl);
  }
  return [...collected];
}

function findLatestCommonReleaseTag(tagLists) {
  if (tagLists.length === 0) {
    return "";
  }
  const [first, ...rest] = tagLists.map((tags) => new Set(tags));
  const common = [...first].filter((tag) => rest.every((tags) => tags.has(tag)));
  const releaseCandidates = common
    .map((tag) => ({ tag, parsed: parseReleaseTag(tag) }))
    .filter((entry) => entry.parsed);
  const stableCandidates = releaseCandidates.filter((entry) => !entry.parsed.prerelease);
  const ranked = (stableCandidates.length > 0 ? stableCandidates : releaseCandidates)
    .map((entry) => entry.tag)
    .sort(compareReleaseTags);
  return ranked.at(-1) || "";
}

function resolveEnvReference(value, envMap, seen = new Set()) {
  if (!value) {
    return "";
  }
  return value.replace(/\$\{([^}:]+)(?::-([^}]*))?\}/g, (_match, key, fallback = "") => {
    if (seen.has(key)) {
      return fallback;
    }
    const resolved = envMap.get(key);
    if (!resolved || !resolved.trim()) {
      return fallback;
    }
    const nextSeen = new Set(seen);
    nextSeen.add(key);
    return resolveEnvReference(resolved, envMap, nextSeen);
  });
}

function getReleaseImageMap(filePath) {
  const envMap = readEnvFile(filePath);
  const images = new Map();
  for (const key of IMAGE_KEYS) {
    const rawValue = envMap.get(key);
    if (!rawValue) {
      continue;
    }
    images.set(key, resolveEnvReference(rawValue.trim(), envMap));
  }
  return images;
}

function getReleaseImageMapFromEnv(envMap) {
  const images = new Map();
  for (const key of IMAGE_KEYS) {
    const rawValue = envMap.get(key);
    if (!rawValue) {
      continue;
    }
    images.set(key, resolveEnvReference(rawValue.trim(), envMap));
  }
  return images;
}

function defaultEnvMap() {
  if (!fs.existsSync(RELEASE_ENV_TEMPLATE_PATH)) {
    throw new Error(`Release env template is missing: ${RELEASE_ENV_TEMPLATE_PATH}`);
  }
  return readEnvFile(RELEASE_ENV_TEMPLATE_PATH);
}

function getReleaseSource(options) {
  const deployEnvPath = envFilePath(options);
  if (fs.existsSync(deployEnvPath)) {
    return {
      kind: "deployment",
      path: deployEnvPath
    };
  }
  return {
    kind: "uninitialized",
    path: deployEnvPath
  };
}

function getReleaseVersionInfo(options) {
  const source = getReleaseSource(options);
  const images = source.kind === "deployment"
    ? getReleaseImageMap(source.path)
    : getReleaseImageMapFromEnv(defaultEnvMap());
  const tags = IMAGE_KEYS
    .map((key) => extractImageTag(images.get(key)))
    .filter(Boolean);
  const version = source.kind === "uninitialized"
    ? "uninitialized"
    : (tags.length === IMAGE_KEYS.length && new Set(tags).size === 1
        ? tags[0]
        : "custom");
  return {
    version,
    images,
    source
  };
}

function formatReleaseVersionString(options) {
  const revision = getGitRevision();
  const releaseVersion = getReleaseVersionInfo(options).version;
  if (revision) {
    return `Wattetheria ${releaseVersion} (${revision})`;
  }
  return `Wattetheria ${releaseVersion}`;
}

function formatCliVersionString() {
  const revision = getGitRevision();
  if (revision) {
    return `Wattetheria CLI ${PACKAGE_JSON.version} (${revision})`;
  }
  return `Wattetheria CLI ${PACKAGE_JSON.version}`;
}

function npmCommandName() {
  return process.platform === "win32" ? "npm.cmd" : "npm";
}

function formatCliUpdateCommand() {
  return "npm install -g wattetheria@latest";
}

function parseVersion(version) {
  const match = String(version).trim().match(/^(\d+)\.(\d+)\.(\d+)(?:[-+].*)?$/);
  if (!match) {
    throw new Error(`Invalid Wattetheria CLI version: ${version}`);
  }
  return match.slice(1).map((part) => Number.parseInt(part, 10));
}

function compareVersions(left, right) {
  const leftParts = parseVersion(left);
  const rightParts = parseVersion(right);
  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] < rightParts[index]) {
      return -1;
    }
    if (leftParts[index] > rightParts[index]) {
      return 1;
    }
  }
  return 0;
}

function latestPublishedCliVersion() {
  const result = spawnSync(npmCommandName(), ["view", "wattetheria", "version"], {
    encoding: "utf8"
  });
  if (result.error) {
    throw new Error(`Failed to query latest Wattetheria CLI version: ${result.error.message}`);
  }
  if (result.status !== 0) {
    const detail = String(result.stderr || "").trim();
    throw new Error(
      detail
        ? `Failed to query latest Wattetheria CLI version from npm: ${detail}`
        : "Failed to query latest Wattetheria CLI version from npm."
    );
  }
  const version = result.stdout.trim();
  parseVersion(version);
  return version;
}

function ensureCliVersionIsLatest() {
  const current = PACKAGE_JSON.version;
  const latest = latestPublishedCliVersion();
  if (compareVersions(current, latest) >= 0) {
    return;
  }
  throw new Error([
    "Wattetheria CLI is outdated.",
    `Current: ${current}`,
    `Latest:  ${latest}`,
    "",
    "Update the CLI first:",
    "  wattetheria cli update",
    "",
    "Then rerun:",
    "  wattetheria install"
  ].join("\n"));
}

function formatBanner(options) {
  const wordmark = [
    " __        __    _   _      _   _               _       ",
    " \\ \\      / /_ _| |_| |_ ___| |_| |__   ___ _ __(_) __ _ ",
    "  \\ \\ /\\ / / _` | __| __/ _ \\ __| '_ \\ / _ \\ '__| |/ _` |",
    "   \\ V  V / (_| | |_| ||  __/ |_| | | |  __/ |  | | (_| |",
    "    \\_/\\_/ \\__,_|\\__|\\__\\___|\\__|_| |_|\\___|_|  |_|\\__,_|"
  ].join("\n");
  const subtitle = `${formatReleaseVersionString(options)} - local agent runtime, swarm sync, external agent reach`;

  if (!supportsBannerColor()) {
    return `${wordmark}\n${subtitle}`;
  }
  return `${ANSI_ORANGE}${wordmark}${ANSI_RESET}\n${ANSI_MUTED}${subtitle}${ANSI_RESET}`;
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

function refreshDeploymentComposeAsset(templatePath, targetPath, options) {
  if (!fs.existsSync(targetPath)) {
    fs.copyFileSync(templatePath, targetPath);
    return;
  }

  const template = fs.readFileSync(templatePath, "utf8");
  const current = fs.readFileSync(targetPath, "utf8");
  if (!options.force && current === template) {
    return;
  }

  if (current !== template) {
    const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
    const backupPath = `${targetPath}.bak-${timestamp}`;
    fs.copyFileSync(targetPath, backupPath);
    console.log(`Backed up previous deployment compose: ${backupPath}`);
  }

  fs.copyFileSync(templatePath, targetPath);
  console.log("Refreshed deployment compose from current CLI package.");
}

function ensureDeploymentAssets(options) {
  fs.mkdirSync(options.dir, { recursive: true });

  const templateComposePath = path.join(PACKAGE_ROOT, "docker-compose.release.yml");
  const targetEnvPath = envFilePath(options);
  const targetComposePath = composeFilePath(options);

  if (options.force || !fs.existsSync(targetEnvPath)) {
    writeEnvFile(targetEnvPath, defaultEnvMap());
  } else {
    mergeNewDefaultKeys(targetEnvPath);
  }
  refreshDeploymentComposeAsset(templateComposePath, targetComposePath, options);

  const envMap = readEnvFile(targetEnvPath);
  const envFileName = path.basename(targetEnvPath);
  envMap.set("WATTETHERIA_COMPOSE_ENV_FILE", envFileName);
  envMap.set("WATTETHERIA_DEPLOY_DIR", ".");
  envMap.set("WATTETHERIA_RUNTIME_ENV_FILE", `/var/lib/wattetheria-deploy/${envFileName}`);
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

function deploymentState(dir = DEFAULT_DEPLOY_DIR) {
  const envPath = path.join(dir, ".env");
  const composePath = path.join(dir, "docker-compose.yml");
  const stateDir = fs.existsSync(envPath)
    ? resolveConfiguredPath(
        dir,
        getEnvValue(readEnvFile(envPath), "WATTETHERIA_HOST_STATE_DIR", "./data/wattetheria")
      )
    : path.join(dir, "data", "wattetheria");
  const tokenPath = path.join(stateDir, "control.token");
  return {
    dir,
    envPath,
    composePath,
    stateDir,
    tokenPath,
    installed: fs.existsSync(envPath) || fs.existsSync(composePath) || fs.existsSync(tokenPath),
    runnable: fs.existsSync(envPath) && fs.existsSync(composePath)
  };
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

function mergeNewDefaultKeys(targetPath) {
  const templateMap = defaultEnvMap();
  const targetMap = readEnvFile(targetPath);
  let added = 0;
  for (const [key, value] of templateMap.entries()) {
    if (!targetMap.has(key)) {
      targetMap.set(key, value);
      added += 1;
    }
  }
  if (added > 0) {
    writeEnvFile(targetPath, targetMap);
    console.log(`Merged ${added} new config key(s) from updated template.`);
  }
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

async function syncImageTagsToLatestPublishedRelease(options) {
  if (options.tag) {
    return options.tag;
  }

  const targetPath = envFilePath(options);
  const targetMap = readEnvFile(targetPath);
  const images = getReleaseImageMapFromEnv(targetMap);
  const missing = IMAGE_KEYS.filter((key) => !images.get(key));
  if (missing.length > 0) {
    throw new Error(`Release image refs are missing from deployment env: ${missing.join(", ")}`);
  }

  const tagLists = [];
  for (const key of IMAGE_KEYS) {
    const imageRef = images.get(key);
    const tags = await listPublishedImageTags(imageRef);
    if (tags.length === 0) {
      throw new Error(`No published tags found for ${stripImageTag(imageRef)}`);
    }
    tagLists.push(tags);
  }

  const latestTag = findLatestCommonReleaseTag(tagLists);
  if (!latestTag) {
    throw new Error(
      "Could not find a shared published release tag across all configured images."
    );
  }

  const currentTag = targetMap.get("RELEASE_TAG") || "";
  if (currentTag.trim() !== latestTag) {
    targetMap.set("RELEASE_TAG", latestTag);
  }

  let changed = false;
  for (const key of IMAGE_KEYS) {
    const current = targetMap.get(key);
    if (!current) {
      continue;
    }
    const next = `${stripImageTag(resolveEnvReference(current.trim(), targetMap))}:${latestTag}`;
    if (resolveEnvReference(current.trim(), targetMap) !== next) {
      targetMap.set(key, next);
      changed = true;
    }
  }
  if (changed || currentTag.trim() !== latestTag) {
    writeEnvFile(targetPath, targetMap);
    console.log(`Resolved latest published release tag ${latestTag}.`);
  } else {
    console.log(`Release images are already pinned to latest published tag ${latestTag}.`);
  }
  return latestTag;
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

function runDocker(args, capture = false) {
  const dockerCommand = resolveDockerCommand() || "docker";
  const result = spawnSync(dockerCommand, args, {
    stdio: capture ? "pipe" : "inherit",
    encoding: capture ? "utf8" : undefined
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    const stderr = capture ? (result.stderr || "").trim() : "";
    throw new Error(stderr || `docker ${args.join(" ")} failed`);
  }
  return result;
}

function postgresVolumeName(options) {
  return `${options.projectName}_${POSTGRES_VOLUME_SERVICE_NAME}`;
}

function dockerVolumeExists(volumeName) {
  const dockerCommand = resolveDockerCommand() || "docker";
  const result = spawnSync(dockerCommand, ["volume", "inspect", volumeName], {
    stdio: "ignore"
  });
  return !result.error && result.status === 0;
}

function removeDockerVolumeIfExists(volumeName) {
  if (!dockerVolumeExists(volumeName)) {
    console.log(`PostgreSQL volume not found: ${volumeName}`);
    return;
  }
  runDocker(["volume", "rm", volumeName]);
  if (dockerVolumeExists(volumeName)) {
    throw new Error(`PostgreSQL volume still exists after removal: ${volumeName}`);
  }
  console.log(`Removed PostgreSQL volume: ${volumeName}`);
}

function comparablePath(filePath) {
  const resolved = path.resolve(filePath);
  return process.platform === "win32" ? resolved.toLowerCase() : resolved;
}

function pathIsInsideOrEqual(childPath, parentPath) {
  const child = comparablePath(childPath);
  const parent = comparablePath(parentPath);
  const relative = path.relative(parent, child);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}

function removeWattetheriaHomeDir() {
  const homeDir = WATTETHERIA_HOME_DIR;
  if (!fs.existsSync(homeDir)) {
    console.log(`Wattetheria home directory not found: ${homeDir}`);
    return;
  }
  if (pathIsInsideOrEqual(process.cwd(), homeDir)) {
    process.chdir(os.homedir());
  }
  fs.rmSync(homeDir, { recursive: true, force: true });
  if (fs.existsSync(homeDir)) {
    throw new Error(`Wattetheria home directory still exists after removal: ${homeDir}`);
  }
  console.log(`Removed Wattetheria home directory: ${homeDir}`);
}

const RELEASE_SERVICES = [
  "kernel",
  "wattswarm-postgres",
  "wattswarm-runtime",
  "wattswarm-kernel",
  "wattswarm-worker"
];

function runningComposeServices(options) {
  const result = runCompose(options, ["ps", "--status", "running", "--services"], true);
  return new Set(
    (result.stdout || "")
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean)
  );
}

function composeStackIsRunning(options) {
  const runningServices = runningComposeServices(options);
  return RELEASE_SERVICES.every((service) => runningServices.has(service));
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

function resolveConfiguredPath(baseDir, configured) {
  if (!configured || !configured.trim()) {
    return "";
  }
  return path.isAbsolute(configured) ? configured : path.resolve(baseDir, configured);
}

function resolveMcpProxyConfig(options) {
  const envPath = envFilePath(options);
  const envMap = fs.existsSync(envPath) ? readEnvFile(envPath) : defaultEnvMap();
  const dataDir = options.dataDir
    ? path.resolve(options.dataDir)
    : resolveConfiguredPath(
        options.dir,
        getEnvValue(envMap, "WATTETHERIA_HOST_STATE_DIR", "./data/wattetheria")
      );
  const tokenPath = path.join(dataDir, "control.token");
  const host = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_BIND_HOST", "127.0.0.1");
  const port = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_PORT", "7777");
  const tokenAuth = getEnvValue(envMap, "WATTETHERIA_MCP_TOKEN_AUTH", "false") === "true";
  return {
    endpoint: (options.controlPlane || `http://${host}:${port}`).replace(/\/+$/, ""),
    tokenPath,
    tokenAuth
  };
}

async function runHealthChecks(options) {
  const envMap = readEnvFile(envFilePath(options));
  const kernelHost = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_BIND_HOST", "127.0.0.1");
  const kernelPort = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_PORT", "7777");
  const uiHost = getEnvValue(envMap, "WATTSWARM_UI_BIND_HOST", "127.0.0.1");
  const uiPort = getEnvValue(envMap, "WATTSWARM_UI_PORT", "7788");

  await waitForHttp("kernel health", `http://${kernelHost}:${kernelPort}/v1/health`);
  await waitForHttp("wattswarm ui", `http://${uiHost}:${uiPort}/`);
}

function printSummary(options) {
  const envMap = readEnvFile(envFilePath(options));
  const kernelHost = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_BIND_HOST", "127.0.0.1");
  const kernelPort = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_PORT", "7777");
  const uiHost = getEnvValue(envMap, "WATTSWARM_UI_BIND_HOST", "127.0.0.1");
  const uiPort = getEnvValue(envMap, "WATTSWARM_UI_PORT", "7788");

  console.log("");
  console.log("Deployment complete.");
  console.log(`Kernel:       http://${kernelHost}:${kernelPort}`);
  console.log(`Wattswarm UI: http://${uiHost}:${uiPort}`);
  console.log(`Deploy dir:   ${options.dir}`);
}

function supervisionUrl(options) {
  const envMap = readEnvFile(envFilePath(options));
  const kernelHost = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_BIND_HOST", "127.0.0.1");
  const kernelPort = getEnvValue(envMap, "WATTETHERIA_CONTROL_PLANE_PORT", "7777");
  return `http://${kernelHost}:${kernelPort}/supervision`;
}

function mcpServerConfigSnippet(options) {
  const args = ["wattetheria", "mcp-proxy"];
  if (path.resolve(options.dir) !== path.resolve(DEFAULT_DEPLOY_DIR)) {
    args.push("--dir", options.dir);
  }
  return JSON.stringify({
    mcpServers: {
      wattetheria: {
        command: "npx",
        args
      }
    }
  }, null, 2);
}

function printSetupHint() {
  console.log("");
  console.log("To finish first-time setup, run:");
  console.log("  npx wattetheria setup");
}

function printManualSetupChecklist(options) {
  console.log("");
  console.log("Finish setup:");
  console.log("1. Open Supervision and configure the runtime:");
  console.log(`   ${supervisionUrl(options)}`);
  console.log("");
  console.log("2. Add Wattetheria MCP to your agent runtime:");
  console.log(mcpServerConfigSnippet(options));
  console.log("");
  console.log("3. Restart Wattetheria so runtime config takes effect:");
  console.log("   npx wattetheria restart");
  console.log("");
  console.log("4. Restart your agent runtime so MCP config takes effect.");
  console.log("");
  console.log("5. Verify MCP access from the agent runtime:");
  console.log("   - list Wattetheria MCP tools");
  console.log("   - call one read-only Wattetheria MCP tool");
}

async function waitForEnter(message) {
  const rl = createInterface({
    input: process.stdin,
    output: process.stdout
  });
  try {
    await rl.question(message);
  } finally {
    rl.close();
  }
}

async function install(options, behavior = {}) {
  if (!behavior.cliVersionAlreadyChecked) {
    ensureCliVersionIsLatest();
  }
  if (!behavior.dockerAlreadyChecked) {
    await ensureDockerAvailable({ interactive: true });
  }
  ensureDeploymentAssets(options);
  if (options.tag) {
    console.log(`Pinning release images to requested tag ${options.tag}.`);
  } else {
    await syncImageTagsToLatestPublishedRelease(options);
  }
  runCompose(options, ["config"], true);
  console.log("Pulling release images...");
  runCompose(options, ["pull"]);
  console.log("Starting release stack...");
  runCompose(options, ["up", "-d"]);
  if (options.healthChecks) {
    await runHealthChecks(options);
  }
  printSummary(options);
  if (!behavior.suppressSetupHint) {
    printSetupHint();
  }
}

async function setup(options) {
  console.log("Checking Docker runtime...");
  const dockerStatus = getDockerStatus();
  if (!dockerStatus.ready) {
    throw new Error([
      formatDockerStatusMessage(dockerStatus),
      "",
      "Install Docker, start it, then rerun:",
      "  npx wattetheria setup"
    ].join("\n"));
  }
  console.log("[ok] Docker runtime is ready.");

  const deployment = deploymentState(options.dir);
  console.log("");
  if (deployment.runnable) {
    console.log("[1/6] Start existing Wattetheria deployment");
    if (composeStackIsRunning(options)) {
      console.log("Deployment already initialized and running. Skipping docker compose up.");
      if (options.healthChecks) {
        await runHealthChecks(options);
      }
      printSummary(options);
    } else {
      console.log("Deployment already initialized. Starting existing stack...");
      await start(options);
    }
  } else {
    console.log("[1/6] Install Wattetheria");
    await install(options, {
      dockerAlreadyChecked: true,
      suppressSetupHint: true
    });
  }

  if (!isInteractiveTerminal()) {
    printManualSetupChecklist(options);
    return;
  }

  const url = supervisionUrl(options);
  console.log("");
  console.log("[2/6] Configure runtime");
  console.log(`Open: ${url}`);
  const opened = openUrl(url);
  if (!opened) {
    console.log("Open the URL above in your browser.");
  }
  await waitForEnter("After saving runtime config, press Enter to continue.");

  console.log("");
  console.log("[3/6] Install MCP in your agent runtime");
  console.log(mcpServerConfigSnippet(options));
  await waitForEnter("After saving MCP config, press Enter to continue.");

  console.log("");
  console.log("[4/6] Restart Wattetheria");
  await restart(options);

  console.log("");
  console.log("[5/6] Restart your agent runtime");
  await waitForEnter("After restarting your agent runtime, press Enter to continue.");

  console.log("");
  console.log("[6/6] Verify MCP access");
  console.log("From the agent runtime:");
  console.log("- list Wattetheria MCP tools");
  console.log("- call one read-only Wattetheria MCP tool");
  await waitForEnter("After verifying MCP access, press Enter to finish.");

  console.log("");
  console.log("Setup complete.");
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

async function restart(options) {
  await ensureDockerAvailable();
  if (!fs.existsSync(composeFilePath(options)) || !fs.existsSync(envFilePath(options))) {
    throw new Error("Deployment is not initialized. Run install first.");
  }
  console.log("Stopping release stack...");
  runCompose(options, ["down"]);
  console.log("Starting release stack...");
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
  if (options.tag) {
    console.log(`Pinning release images to requested tag ${options.tag}.`);
  } else {
    await syncImageTagsToLatestPublishedRelease(options);
  }
  console.log("Pulling updated images...");
  runCompose(options, ["pull"]);
  console.log("Restarting release stack...");
  runCompose(options, ["up", "-d"]);
  if (options.healthChecks) {
    await runHealthChecks(options);
  }
  printSummary(options);
}

function updateCliPackage() {
  const command = formatCliUpdateCommand();
  console.log(formatCliVersionString());

  console.log("");
  console.log(`Running: ${command}`);
  const result = spawnSync(npmCommandName(), ["install", "-g", "wattetheria@latest"], {
    stdio: "inherit"
  });
  if (typeof result.status === "number") {
    if (result.status === 0) {
      return;
    }
    throw new Error(`CLI update failed with exit code ${result.status}`);
  }
  throw result.error ?? new Error("CLI update failed");
}

async function stop(options) {
  await ensureDockerAvailable();
  runCompose(options, ["down"]);
}

async function uninstall(options) {
  await ensureDockerAvailable();
  const hasDeploymentFiles = fs.existsSync(composeFilePath(options)) && fs.existsSync(envFilePath(options));
  if (hasDeploymentFiles) {
    const args = ["down"];
    if (options.purge) {
      args.push("--remove-orphans");
    } else if (options.volumes) {
      args.push("-v");
    }
    runCompose(options, args);
  } else {
    console.log(`Deployment compose/env files not found in: ${options.dir}`);
  }
  if (options.purge) {
    removeDockerVolumeIfExists(postgresVolumeName(options));
    removeWattetheriaHomeDir();
  }
}

async function logs(options) {
  await ensureDockerAvailable();
  runCompose(options, ["logs", ...options.composeArgs]);
}

async function mcpProxy(options) {
  const { endpoint, tokenPath, tokenAuth } = resolveMcpProxyConfig(options);
  if (tokenAuth && !fs.existsSync(tokenPath)) {
    throw new Error(
      [
        `Wattetheria control token not found: ${tokenPath}`,
        "Start or initialize the local node first, or pass --data-dir <path>."
      ].join("\n")
    );
  }
  const token = fs.existsSync(tokenPath) ? fs.readFileSync(tokenPath, "utf8").trim() : "";
  if (tokenAuth && !token) {
    throw new Error(`Wattetheria control token is empty: ${tokenPath}`);
  }

  const input = readline.createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
    terminal: false
  });

  for await (const line of input) {
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }
    let request;
    try {
      request = JSON.parse(trimmed);
    } catch (error) {
      writeMcpResponse({
        jsonrpc: "2.0",
        id: null,
        error: {
          code: -32700,
          message: `parse error: ${error.message}`
        }
      });
      continue;
    }

    const hasId = Object.prototype.hasOwnProperty.call(request, "id");
    const response = await forwardMcpRequest(endpoint, token, request);
    if (hasId) {
      writeMcpResponse(response);
    }
  }
}

async function forwardMcpRequest(endpoint, token, request) {
  const headers = {
    "content-type": "application/json"
  };
  if (token) {
    headers.authorization = `Bearer ${token}`;
  }
  try {
    const response = await fetch(`${endpoint}/mcp`, {
      method: "POST",
      headers,
      body: JSON.stringify(request)
    });
    const payload = await response.json().catch(() => null);
    if (response.ok) {
      return payload;
    }
    return {
      jsonrpc: "2.0",
      id: request.id ?? null,
      error: {
        code: -32000,
        message: `local Wattetheria MCP returned HTTP ${response.status}`,
        data: payload
      }
    };
  } catch (error) {
    return {
      jsonrpc: "2.0",
      id: request.id ?? null,
      error: {
        code: -32000,
        message: error.message
      }
    };
  }
}

function writeMcpResponse(response) {
  process.stdout.write(`${JSON.stringify(response)}\n`);
}

function doctor(rawArgv) {
  const wantsHelp = rawArgv.some((arg) => arg === "--help" || arg === "-h");
  if (!wantsHelp) {
    const status = getDockerStatus();
    if (status.ready) {
      console.error("Docker runtime is available.");
    } else {
      console.error(formatDockerStatusMessage(status));
    }
    console.error(`Node.js ${process.version} is available.`);
  }
  forwardToRustBinary("doctor", rawArgv);
}

function printReleaseImages(options) {
  const release = getReleaseVersionInfo(options);
  console.log(`Source:      ${release.source.path}`);
  for (const key of IMAGE_KEYS) {
    const imageRef = release.images.get(key) || "(not configured)";
    console.log(`${key}: ${imageRef}`);
  }
}

function printVersion(options) {
  if (options.versionTarget === "cli") {
    console.log(formatCliVersionString());
    return;
  }

  console.log(formatReleaseVersionString(options));
  if (options.includeImages) {
    printReleaseImages(options);
  }
}

function printImages(options) {
  const release = getReleaseVersionInfo(options);
  console.log(formatReleaseVersionString(options));
  printReleaseImages(options);
}

function shouldPrintBanner(command) {
  return isInteractiveTerminal()
    && !process.env.CI
    && !process.env.WATTETHERIA_NO_BANNER
    && BANNER_COMMANDS.has(command);
}

function printBanner(options) {
  console.log(formatBanner(options));
  console.log("");
}

function supportsBannerColor() {
  return process.stdout.isTTY
    && !process.env.NO_COLOR
    && process.env.TERM !== "dumb";
}

// Forwarded subcommands delegate verbatim to the local Rust binary
// `wattetheria-client-cli`. Keep the JS side stupid — no flag parsing here.
const FORWARDED_SUBCOMMANDS = new Set([
  "identity",
  "servicenet",
]);

function nativePlatformKey(platform = process.platform, arch = process.arch) {
  const normalizedArch = NATIVE_ARCH_ALIASES.get(arch);
  if (!normalizedArch || !["darwin", "linux", "win32"].includes(platform)) {
    return "";
  }
  return `${platform}-${normalizedArch}`;
}

function rustCliBinaryName(platform = process.platform) {
  return platform === "win32" ? `${RUST_CLI_BASE_NAME}.exe` : RUST_CLI_BASE_NAME;
}

function nativePackageName(platform = process.platform, arch = process.arch) {
  const platformKey = nativePlatformKey(platform, arch);
  return platformKey ? `${NATIVE_CLI_PACKAGE_PREFIX}-${platformKey}` : "";
}

function nativePackageRustBinaryPath() {
  const packageName = nativePackageName();
  if (!packageName) {
    return "";
  }

  try {
    const manifestPath = require.resolve(`${packageName}/package.json`, {
      paths: [PACKAGE_ROOT]
    });
    return path.join(path.dirname(manifestPath), "bin", rustCliBinaryName());
  } catch (_error) {
    return "";
  }
}

function bundledRustBinaryPath() {
  const platformKey = nativePlatformKey();
  if (!platformKey) {
    return "";
  }
  return path.join(PACKAGE_ROOT, "bin", "native", platformKey, rustCliBinaryName());
}

function missingNativeCliError(commandName) {
  const platformKey = nativePlatformKey() || `${process.platform}-${process.arch}`;
  if (commandName === "identity") {
    return new Error(
      [
        `Cannot run '${commandName}' because the Wattetheria native CLI for ${platformKey} was not found.`,
        "",
        "`wattetheria identity` is a lightweight local setup command",
        "for ServiceNet publishing and wallet binding. It does not require a local Wattetheria",
        "node, but they do require the native CLI package for this system.",
        "",
        "Install the matching Wattetheria native CLI package, then retry."
      ].join("\n")
    );
  }

  const deployment = deploymentState();
  if (deployment.installed) {
    return new Error(
      `Wattetheria node deployment was found at ${deployment.dir}, but this npm package does not include ` +
        `the native CLI for ${platformKey}. ` +
        `Run 'npx wattetheria status' to check the installed node, or update the local node image.`
    );
  }
  return new Error(
    `Could not run '${commandName}' because no Wattetheria node deployment or native CLI was found. ` +
      `Run 'npx wattetheria install' to install the local node, or set WATTETHERIA_CLI_BIN to ` +
      `the full path of ${rustCliBinaryName()}.`
  );
}

function installedNodeArtifacts(deployment = deploymentState()) {
  return {
    identityPath: path.join(deployment.stateDir, "identity.json"),
    walletMetadataPath: path.join(deployment.stateDir, ".watt-wallet", "metadata.json"),
    walletKeystorePath: path.join(deployment.stateDir, ".watt-wallet", "keystore.json")
  };
}

function isLightweightLocalCommand(commandName) {
  return commandName === "identity";
}

function ensureLightweightCommandAllowed(commandName) {
  if (!isLightweightLocalCommand(commandName)) {
    return;
  }
  const deployment = deploymentState();
  if (!deployment.installed) {
    return;
  }
  const artifacts = installedNodeArtifacts(deployment);
  if (commandName === "identity") {
    throw new Error(
      [
        "Refusing to run a separate local identity command.",
        "",
        fs.existsSync(artifacts.identityPath)
          ? "A local Wattetheria node is already installed and has an identity at:"
          : "A local Wattetheria node is already installed at:",
        deployment.stateDir,
        "",
        "`wattetheria identity` is only for lightweight local setup when no Wattetheria",
        "node is installed. Use the installed node's identity instead."
      ].join("\n")
    );
  }
}

function forwardedArgsForInstalledNode(commandName, rawArgv) {
  const args = [commandName];
  if (!rawArgv.some((arg) => arg === "--data-dir" || arg.startsWith("--data-dir="))) {
    args.push("--data-dir", "/var/lib/wattetheria");
  }
  args.push(...rawArgv.slice(1));
  return args;
}

function isServicenetAgentCardInit(commandName, rawArgv) {
  return commandName === "servicenet"
    && rawArgv[1] === "agent-card"
    && rawArgv[2] === "init";
}

function isServicenetRegister(commandName, rawArgv) {
  return commandName === "servicenet"
    && rawArgv[1] === "register";
}

function isServicenetPublish(commandName, rawArgv) {
  return commandName === "servicenet"
    && rawArgv[1] === "publish";
}

function usesWorkspaceMount(commandName, rawArgv) {
  return isServicenetAgentCardInit(commandName, rawArgv)
    || isServicenetRegister(commandName, rawArgv)
    || isServicenetPublish(commandName, rawArgv);
}

function findExistingAncestor(targetPath) {
  let cursor = path.resolve(targetPath);
  while (!fs.existsSync(cursor)) {
    const parent = path.dirname(cursor);
    if (parent === cursor) {
      throw new Error(`No existing parent directory found for ${targetPath}`);
    }
    cursor = parent;
  }
  return cursor;
}

function toContainerPath(relativePath) {
  if (!relativePath || relativePath === ".") {
    return "/workspace";
  }
  return `/workspace/${relativePath.split(path.sep).join("/")}`;
}

function parsePathOption(rawArgv, optionName, startIndex) {
  const equalsPrefix = `${optionName}=`;
  for (let index = startIndex; index < rawArgv.length; index += 1) {
    const arg = rawArgv[index];
    if (arg === optionName) {
      return { index, value: rawArgv[index + 1], style: "separate" };
    }
    if (arg.startsWith(equalsPrefix)) {
      return { index, value: arg.slice(equalsPrefix.length), style: "equals" };
    }
  }
  return null;
}

function pathMountFor(targetPath) {
  const requestedPath = path.resolve(targetPath);
  const existingPath = fs.existsSync(requestedPath) ? requestedPath : "";
  const mountSource = existingPath
    ? (
      fs.statSync(existingPath).isDirectory()
        ? existingPath
        : path.dirname(existingPath)
    )
    : findExistingAncestor(requestedPath);
  return {
    mountSource,
    containerPath: toContainerPath(path.relative(mountSource, requestedPath))
  };
}

function rewritePathOption(rawArgv, parsedOption, containerPath) {
  const rewrittenArgv = [...rawArgv];
  if (parsedOption.style === "separate") {
    rewrittenArgv[parsedOption.index + 1] = containerPath;
  } else {
    rewrittenArgv[parsedOption.index] = `${rewrittenArgv[parsedOption.index].split("=")[0]}=${containerPath}`;
  }
  return rewrittenArgv;
}

function dockerWorkspaceInvocation(commandName, rawArgv) {
  if (isServicenetAgentCardInit(commandName, rawArgv)) {
    const outputArg = parsePathOption(rawArgv, "--out", 3);
    const outputPath = outputArg?.value || process.cwd();
    if (outputArg && !outputArg.value) {
      throw new Error("Missing value for --out");
    }
    const outputMount = pathMountFor(outputPath);
    const rewrittenArgv = outputArg
      ? rewritePathOption(rawArgv, outputArg, outputMount.containerPath)
      : rawArgv;
    return {
      args: outputArg
        ? forwardedArgsForInstalledNode(commandName, rewrittenArgv)
        : [...forwardedArgsForInstalledNode(commandName, rawArgv), "--out", outputMount.containerPath],
      mountSource: outputMount.mountSource
    };
  }

  if (isServicenetRegister(commandName, rawArgv)) {
    const cardArg = parsePathOption(rawArgv, "--card", 2);
    if (!cardArg) {
      return {
        args: forwardedArgsForInstalledNode(commandName, rawArgv),
        mountSource: process.cwd()
      };
    }
    if (!cardArg.value) {
      throw new Error("Missing value for --card");
    }
    const cardMount = pathMountFor(cardArg.value);
    return {
      args: forwardedArgsForInstalledNode(
        commandName,
        rewritePathOption(rawArgv, cardArg, cardMount.containerPath)
      ),
      mountSource: cardMount.mountSource
    };
  }

  return {
    args: forwardedArgsForInstalledNode(commandName, rawArgv),
    mountSource: process.cwd()
  };
}

function hostPathFromWorkspace(containerPath, mountSource) {
  if (containerPath === "/workspace") {
    return mountSource;
  }
  if (!containerPath.startsWith("/workspace/")) {
    return containerPath;
  }
  return path.join(mountSource, containerPath.slice("/workspace/".length));
}

function rewriteWorkspaceJsonStdout(stdout, mountSource) {
  if (!stdout.trim()) {
    return stdout;
  }
  try {
    const payload = JSON.parse(stdout);
    if (typeof payload.card === "string") {
      payload.card = hostPathFromWorkspace(payload.card, mountSource);
    }
    return `${JSON.stringify(payload, null, 2)}\n`;
  } catch (_error) {
    return stdout;
  }
}

function runInstalledNodeCli(commandName, rawArgv, deployment) {
  const dockerCommand = resolveDockerCommand();
  if (!dockerCommand) {
    throw new Error(
      `Wattetheria node deployment was found at ${deployment.dir}, but Docker is not available ` +
        `to run '${commandName}' inside the installed node. Start Docker Desktop and retry.`
    );
  }
  if (usesWorkspaceMount(commandName, rawArgv)) {
    const invocation = dockerWorkspaceInvocation(commandName, rawArgv);
    const result = spawnSync(
      dockerCommand,
      [
        "compose",
        "--project-name",
        DEFAULT_PROJECT_NAME,
        "--env-file",
        deployment.envPath,
        "-f",
        deployment.composePath,
        "run",
        "--rm",
        "--no-deps",
        "-T",
        "--entrypoint",
        "/app/target/release/wattetheria-client-cli",
        "-v",
        `${invocation.mountSource}:/workspace`,
        "-w",
        "/workspace",
        "kernel",
        ...invocation.args
      ],
      { stdio: ["inherit", "pipe", "inherit"], encoding: "utf8" }
    );
    if (result.stdout) {
      process.stdout.write(rewriteWorkspaceJsonStdout(result.stdout, invocation.mountSource));
    }
    if (typeof result.status === "number") {
      process.exit(result.status);
    }
    throw result.error ?? new Error(`Failed to run '${commandName}' inside the installed node`);
  }
  const result = spawnSync(
    dockerCommand,
    [
      "compose",
      "--project-name",
      DEFAULT_PROJECT_NAME,
      "--env-file",
      deployment.envPath,
      "-f",
      deployment.composePath,
      "exec",
      "-T",
      "kernel",
      "/app/target/release/wattetheria-client-cli",
      ...forwardedArgsForInstalledNode(commandName, rawArgv)
    ],
    { stdio: "inherit" }
  );
  if (typeof result.status === "number") {
    process.exit(result.status);
  }
  throw result.error ?? new Error(`Failed to run '${commandName}' inside the installed node`);
}

function forwardToRustBinary(commandName, rawArgv) {
  ensureLightweightCommandAllowed(commandName);

  // Only the Rust binary name is allowed here. The bare `wattetheria` name
  // resolves to this JS shim on most user PATHs (via the npm bin link), so
  // including it would create an infinite spawn loop.
  const candidates = [
    process.env.WATTETHERIA_CLI_BIN,
    nativePackageRustBinaryPath(),
    bundledRustBinaryPath(),
    RUST_CLI_BASE_NAME,
  ].filter(Boolean);

  for (const candidate of candidates) {
    const probe = spawnSync(candidate, ["--help"], { stdio: "ignore" });
    if (probe.status === 0 || probe.status === 2) {
      // Drop the first arg (the subcommand name itself) — node bin/wattetheria
      // already routed on it, but the Rust binary still wants it at argv[1].
      const args = [commandName, ...rawArgv.slice(1)];
      const result = spawnSync(candidate, args, { stdio: "inherit" });
      if (typeof result.status === "number") {
        process.exit(result.status);
      }
      throw result.error
        ?? new Error(`Failed to spawn ${candidate}`);
    }
  }

  const deployment = deploymentState();
  if (commandName === "identity") {
    throw missingNativeCliError(commandName);
  }
  if (deployment.runnable) {
    runInstalledNodeCli(commandName, rawArgv, deployment);
    return;
  }

  throw missingNativeCliError(commandName);
}

async function run(argv) {
  throwCommandSuggestion(argv);

  if (argv[0] === "doctor") {
    doctor(argv);
    return;
  }

  if (argv[0] === "cli") {
    if (argv[1] === "update" && argv.length === 2) {
      updateCliPackage();
      return;
    }
    if (argv[1] === "help" || argv[1] === "--help" || argv[1] === "-h") {
      printHelp();
      return;
    }
    throw new Error("Unknown command: cli. Use `wattetheria cli update`.");
  }

  if (argv[0] && FORWARDED_SUBCOMMANDS.has(argv[0])) {
    forwardToRustBinary(argv[0], argv);
    return;
  }

  const { command, options } = parseArgs(argv);

  if (shouldPrintBanner(command)) {
    printBanner(options);
  }

  switch (command) {
    case "version":
      printVersion(options);
      return;
    case "images":
      printImages(options);
      return;
    case "setup":
      await setup(options);
      return;
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
    case "restart":
      await restart(options);
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
    case "mcp-proxy":
      await mcpProxy(options);
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

const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { createInterface } = require("node:readline/promises");
const readline = require("node:readline");

const PACKAGE_ROOT = path.resolve(__dirname, "..");
const PACKAGE_JSON = require(path.join(PACKAGE_ROOT, "package.json"));
const DEFAULT_DEPLOY_DIR = path.join(os.homedir(), ".wattetheria", "deploy");
const DEFAULT_PROJECT_NAME = "wattetheria";
const DEFAULT_COMMAND = "help";
const DEFAULT_IMAGE_REFS = new Map([
  ["WATTETHERIA_KERNEL_IMAGE", "ghcr.io/wattetheria/wattetheria-kernel:latest"],
  ["WATTETHERIA_OBSERVATORY_IMAGE", "ghcr.io/wattetheria/wattetheria-observatory:latest"],
  ["WATTSWARM_KERNEL_IMAGE", "ghcr.io/wattetheria/wattswarm-kernel:latest"],
  ["WATTSWARM_RUNTIME_IMAGE", "ghcr.io/wattetheria/wattswarm-runtime:latest"],
  ["WATTSWARM_WORKER_IMAGE", "ghcr.io/wattetheria/wattswarm-worker:latest"]
]);
const IMAGE_KEYS = [
  "WATTETHERIA_KERNEL_IMAGE",
  "WATTETHERIA_OBSERVATORY_IMAGE",
  "WATTSWARM_KERNEL_IMAGE",
  "WATTSWARM_RUNTIME_IMAGE",
  "WATTSWARM_WORKER_IMAGE"
];
const HOST_STATE_DIR_KEYS = ["WATTETHERIA_HOST_STATE_DIR", "WATTSWARM_HOST_STATE_DIR"];
const REGISTRY_TAG_PAGE_SIZE = 1000;
const DEFAULT_ENV_ENTRIES = [
  ...DEFAULT_IMAGE_REFS.entries(),
  ["WATTETHERIA_CONTROL_PLANE_BIND_HOST", "127.0.0.1"],
  ["WATTETHERIA_CONTROL_PLANE_PORT", "7777"],
  ["WATTETHERIA_OBSERVATORY_BIND_HOST", "127.0.0.1"],
  ["WATTETHERIA_OBSERVATORY_PORT", "8780"],
  ["WATTSWARM_UI_BIND_HOST", "127.0.0.1"],
  ["WATTSWARM_UI_PORT", "7788"],
  ["WATTSWARM_SYNC_GRPC_BIND_HOST", "127.0.0.1"],
  ["WATTSWARM_SYNC_GRPC_PORT", "7791"],
  ["WATTSWARM_P2P_HOST_PORT", "4001"],
  ["WATTSWARM_UDP_ANNOUNCE_HOST_PORT", "37931"],
  ["WATTETHERIA_HOST_STATE_DIR", "./data/wattetheria"],
  ["WATTSWARM_HOST_STATE_DIR", "./data/wattswarm"],
  ["WATTETHERIA_RUNTIME_ENV_FILE", ".env.release.local"],
  ["WATTETHERIA_AGENT_CONTROL_PLANE_ENDPOINT", "http://127.0.0.1:7777"],
  ["WATTETHERIA_AGENT_WATTSWARM_UI_BASE_URL", "http://127.0.0.1:7788"],
  ["WATTETHERIA_AGENT_WATTSWARM_SYNC_GRPC_ENDPOINT", "http://127.0.0.1:7791"],
  ["WATTETHERIA_AGENT_HOST_DATA_DIR", "./data/wattetheria"],
  ["WATTETHERIA_BRAIN_PROVIDER_KIND", "rules"],
  ["WATTETHERIA_BRAIN_BASE_URL", ""],
  ["WATTETHERIA_BRAIN_MODEL", ""],
  ["WATTETHERIA_BRAIN_API_KEY_ENV", ""],
  ["WATTETHERIA_SERVICENET_BASE_URL", ""],
  ["WATTETHERIA_AUTONOMY_ENABLED", "false"],
  ["WATTETHERIA_AUTONOMY_INTERVAL_SEC", "30"],
  ["OPENCLAW_API_KEY", ""],
  ["WATTSWARM_PG_DB", "wattswarm"],
  ["WATTSWARM_PG_USER", "postgres"],
  ["WATTSWARM_PG_PASSWORD", "replace-with-strong-password"],
  ["WATTSWARM_P2P_ENABLED", "true"],
  ["WATTSWARM_P2P_MDNS", "true"],
  ["WATTSWARM_P2P_PORT", "4001"],
  ["WATTSWARM_WORKER_CONCURRENCY", "16"],
  ["WATTSWARM_WORKER_POLL_MS", "250"],
  ["WATTSWARM_WORKER_LEASE_MS", "30000"],
  ["WATTSWARM_UDP_ANNOUNCE_ENABLED", "false"],
  ["WATTSWARM_UDP_ANNOUNCE_MODE", "multicast"],
  ["WATTSWARM_UDP_ANNOUNCE_ADDR", "239.255.42.99"],
  ["WATTSWARM_UDP_ANNOUNCE_PORT", "37931"]
];
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
  version     Show Wattetheria release version
  images      Show configured release images
  install     Prepare deployment, pull images, and start the stack
  start       Start an existing deployment
  status      Show docker compose status
  update      Resolve latest published release, pull, and restart
  restart     Recreate and restart the deployment
  stop        Stop the deployment
  uninstall   Stop the deployment and optionally remove volumes
  logs        Show docker compose logs
  mcp-proxy   Run stdio MCP proxy for the local Wattetheria node
  doctor      Check local prerequisites
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
  --purge                With uninstall, remove the deployment directory
  --data-dir <path>      With mcp-proxy, override Wattetheria host state directory
  --control-plane <url>  With mcp-proxy, override local control-plane endpoint
`);
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
  return new Map(DEFAULT_ENV_ENTRIES);
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

function formatBanner(options) {
  return `${formatReleaseVersionString(options)} — Local agent runtime with swarm sync and external agent reach.`;
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
  return {
    endpoint: (options.controlPlane || `http://${host}:${port}`).replace(/\/+$/, ""),
    tokenPath
  };
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

async function mcpProxy(options) {
  const { endpoint, tokenPath } = resolveMcpProxyConfig(options);
  if (!fs.existsSync(tokenPath)) {
    throw new Error(
      [
        `Wattetheria control token not found: ${tokenPath}`,
        "Start or initialize the local node first, or pass --data-dir <path>."
      ].join("\n")
    );
  }
  const token = fs.readFileSync(tokenPath, "utf8").trim();
  if (!token) {
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
  try {
    const response = await fetch(`${endpoint}/mcp`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${token}`,
        "content-type": "application/json"
      },
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

function doctor() {
  const status = getDockerStatus();
  if (!status.ready) {
    throw new Error(formatDockerStatusMessage(status));
  }
  console.log("Docker runtime is available.");
  console.log(`Node.js ${process.version} is available.`);
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
  return !["help", "--help", "-h", "version", "images", "mcp-proxy"].includes(command);
}

function printBanner(options) {
  console.log(formatBanner(options));
  console.log("");
}

async function run(argv) {
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
    case "doctor":
      doctor();
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

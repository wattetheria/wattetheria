const fs = require("node:fs");
const path = require("node:path");

const BIND_ENV = "WATTETHERIA_MCP_PUBLIC_BIND";
const BASE_URL_ENV = "WATTETHERIA_MCP_PUBLIC_BASE_URL";

function configuredBind(env) {
  return (env.get(BIND_ENV) ?? process.env[BIND_ENV] ?? "").trim();
}

function printUrl(secretPath, env) {
  const base = (env.get(BASE_URL_ENV) ?? process.env[BASE_URL_ENV] ?? "").trim().replace(/\/+$/, "");
  console.log(`${base}${secretPath}`);
  if (!base) {
    console.log(`Configure ${BASE_URL_ENV} with your public HTTPS prefix to print the complete URL.`);
  }
}

function readNodeFile(name, config) {
  return config.runtime === "docker"
    ? config.readDockerFile(name).trim()
    : fs.readFileSync(path.join(path.dirname(config.tokenPath), name), "utf8").trim();
}

async function run(action, config, env) {
  if (!configuredBind(env)) {
    console.log(`Public MCP is disabled. Set ${BIND_ENV}=0.0.0.0:7778 in the deployment .env and restart the node.`);
    return;
  }
  if (action === "url") {
    printUrl(`/mcp/${readNodeFile("mcp_url_secret", config)}`, env);
    return;
  }
  const token = readNodeFile("control.token", config);
  const response = await fetch(`${config.endpoint}/v1/mcp/public-url/rotate`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` }
  });
  const result = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(`MCP URL rotation failed (HTTP ${response.status}): ${result.error || "request rejected"}`);
  }
  printUrl(result.path, env);
  console.log("Update the MCP URL in your remote agent. Subscribe again if you need event delivery.");
}

module.exports = { run };

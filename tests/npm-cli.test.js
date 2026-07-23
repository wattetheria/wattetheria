"use strict";

const assert = require("node:assert/strict");
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

test("help omits removed agent subcommands", () => {
  const result = runCli(["help"]);

  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.stdout, /Agent subcommands:/);
  assert.doesNotMatch(result.stdout, /^\s+identity\s*$/m);
  assert.doesNotMatch(result.stdout, /^\s+servicenet\s*$/m);
});

const removedCommands = [
  { args: ["identity"], error: /Unknown command: identity/ },
  { args: ["servicenet"], error: /Unknown command: servicenet/ },
  { args: ["register"], error: /Unknown command: register/ },
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

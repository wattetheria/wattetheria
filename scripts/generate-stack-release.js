#!/usr/bin/env node
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync } = require("child_process");

const STACK_RELEASE_REPO = process.env.STACK_RELEASE_REPO || "wattetheria/wattetheria";
const WORKSPACE = process.env.STACK_RELEASE_WORKSPACE || process.cwd();
const OUT_DIR =
  process.env.STACK_RELEASE_OUT_DIR ||
  path.join(WORKSPACE, "wattetheria", "dist", "stack-release");
const SKIP_GITHUB = process.env.STACK_RELEASE_SKIP_GITHUB === "1";
const INITIAL_RELEASE_PR_LIMIT = Number(process.env.STACK_INITIAL_RELEASE_PR_LIMIT || "1000");

const COMPONENTS = [
  {
    name: "wattetheria",
    repo: "wattetheria/wattetheria",
    path: "wattetheria",
    role: "user-local application, authorization, and semantic host",
    artifacts: ["ghcr.io/wattetheria/wattetheria-kernel:${release}"],
  },
  {
    name: "wattswarm",
    repo: "wattetheria/wattswarm",
    path: "wattswarm",
    role: "transport and network substrate",
    artifacts: [
      "ghcr.io/wattetheria/wattswarm-kernel:${release}",
      "ghcr.io/wattetheria/wattswarm-runtime:${release}",
      "ghcr.io/wattetheria/wattswarm-worker:${release}",
    ],
  },
  {
    name: "watt-did",
    repo: "wattetheria/watt-did",
    path: "watt-did",
    role: "shared identity and proof library",
    artifacts: [],
  },
  {
    name: "watt-wallet",
    repo: "wattetheria/watt-wallet",
    path: "watt-wallet",
    role: "local key-custody and signing layer",
    artifacts: [],
  },
  {
    name: "wattswarm-servicenet",
    repo: "wattetheria/watt-servicenet",
    path: "watt-servicenet",
    role: "public-agent registry and invocation boundary",
    artifacts: [],
  },
  {
    name: "wattetheria-gateway",
    repo: "wattetheria/wattetheria-gateway",
    path: "wattetheria-gateway",
    role: "global read-only aggregation surface",
    artifacts: [],
  },
];

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd || WORKSPACE,
    env: process.env,
    encoding: "utf8",
  });

  if (result.status !== 0 && !options.allowFailure) {
    const details = [result.stdout, result.stderr].filter(Boolean).join("\n").trim();
    throw new Error(`${command} ${args.join(" ")} failed${details ? `\n${details}` : ""}`);
  }

  return {
    ok: result.status === 0,
    stdout: result.stdout || "",
    stderr: result.stderr || "",
  };
}

function git(repoPath, args, options = {}) {
  return run("git", args, { ...options, cwd: repoPath }).stdout.trim();
}

function gh(args, options = {}) {
  return run("gh", args, options).stdout.trim();
}

function parseJson(value, context) {
  try {
    return JSON.parse(value);
  } catch (error) {
    throw new Error(`failed to parse ${context}: ${error.message}`);
  }
}

function validateRelease(value) {
  if (!value) {
    throw new Error("RELEASE is required");
  }
  if (value === "latest") {
    throw new Error("RELEASE must be a version tag, not latest");
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value)) {
    throw new Error("RELEASE must contain only letters, numbers, dots, underscores, and hyphens");
  }
}

function ensureRepoPath(component) {
  const repoPath = path.join(WORKSPACE, component.path);
  if (!fs.existsSync(path.join(repoPath, ".git"))) {
    throw new Error(`missing checkout for ${component.repo} at ${repoPath}`);
  }
  return repoPath;
}

function shortSha(value) {
  return value ? value.slice(0, 12) : "";
}

function markdownEscape(value) {
  return String(value || "")
    .replace(/\\/g, "\\\\")
    .replace(/\[/g, "\\[")
    .replace(/\]/g, "\\]")
    .replace(/\|/g, "\\|")
    .replace(/\r?\n/g, " ");
}

function resolveArtifacts(component, release) {
  return component.artifacts.map((artifact) => artifact.replace("${release}", release));
}

function commitUrl(component, commit) {
  return `https://github.com/${component.repo}/commit/${commit.sha}`;
}

function loadManifestFromPath(manifestPath) {
  return parseJson(fs.readFileSync(manifestPath, "utf8"), manifestPath);
}

function findPreviousManifest(release) {
  const explicitPath = process.env.PREVIOUS_STACK_MANIFEST;
  if (explicitPath) {
    return {
      manifest: loadManifestFromPath(explicitPath),
      source: explicitPath,
    };
  }

  if (SKIP_GITHUB) {
    return {
      manifest: null,
      source: "github lookup skipped by STACK_RELEASE_SKIP_GITHUB=1",
    };
  }

  const releasesOutput = gh([
    "release",
    "list",
    "--repo",
    STACK_RELEASE_REPO,
    "--exclude-drafts",
    "--exclude-pre-releases",
    "--limit",
    "50",
    "--json",
    "tagName,publishedAt",
  ]);
  const releases = parseJson(releasesOutput, "GitHub release list");
  const previousRelease = releases.find((entry) => entry.tagName !== release);

  if (!previousRelease) {
    return {
      manifest: null,
      source: "no previous GitHub release found",
    };
  }

  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "watt-stack-release-"));
  const download = run(
    "gh",
    [
      "release",
      "download",
      previousRelease.tagName,
      "--repo",
      STACK_RELEASE_REPO,
      "--pattern",
      "release-manifest*.json",
      "--dir",
      tempDir,
    ],
    { allowFailure: true },
  );

  if (!download.ok) {
    return {
      manifest: null,
      source: `no release manifest asset found on ${previousRelease.tagName}`,
    };
  }

  const manifestFiles = fs
    .readdirSync(tempDir)
    .filter((fileName) => /^release-manifest.*\.json$/.test(fileName))
    .sort();

  if (manifestFiles.length === 0) {
    return {
      manifest: null,
      source: `no release manifest asset found on ${previousRelease.tagName}`,
    };
  }

  const manifestPath = path.join(tempDir, manifestFiles[0]);
  return {
    manifest: loadManifestFromPath(manifestPath),
    source: `${STACK_RELEASE_REPO}@${previousRelease.tagName}/${manifestFiles[0]}`,
  };
}

function previousComponentFor(manifest, component) {
  if (!manifest || !Array.isArray(manifest.components)) {
    return null;
  }
  return manifest.components.find(
    (entry) => entry.repo === component.repo || entry.name === component.name,
  );
}

function collectCommitRange(repoPath, previousCommit, currentCommit) {
  if (!previousCommit) {
    return {
      commits: [],
      boundary: "missing_previous_commit",
      warning: "previous component commit is unavailable",
    };
  }

  const ancestor = run("git", ["merge-base", "--is-ancestor", previousCommit, currentCommit], {
    cwd: repoPath,
    allowFailure: true,
  });
  if (!ancestor.ok) {
    return {
      commits: [],
      boundary: "previous_commit_not_ancestor",
      warning: `${previousCommit} is not an ancestor of ${currentCommit}`,
    };
  }

  const output = git(repoPath, [
    "log",
    "--format=%H%x1f%an%x1f%ae%x1f%cI%x1f%s",
    `${previousCommit}..${currentCommit}`,
  ]);
  const commits = output
    ? output
        .split(/\r?\n/)
        .filter(Boolean)
        .map((line) => {
          const [sha, authorName, authorEmail, committedAt, ...subjectParts] = line.split("\x1f");
          return {
            sha,
            short_sha: shortSha(sha),
            title: subjectParts.join("\x1f"),
            author_name: authorName,
            author_email: authorEmail,
            committed_at: committedAt,
          };
        })
    : [];
  return {
    commits,
    boundary: "commit_range",
    warning: null,
  };
}

function collectPullRequestsForCommits(component, commits) {
  const prsByNumber = new Map();
  const warnings = [];

  if (SKIP_GITHUB) {
    return {
      pullRequests: [],
      commits: commits.map((commit) => ({ ...commit, pull_requests: [] })),
      directCommits: commits.map((commit) => ({ ...commit, pull_requests: [] })),
      warnings: ["GitHub API lookup skipped by STACK_RELEASE_SKIP_GITHUB=1"],
    };
  }

  const commitsWithPullRequests = [];

  for (const commit of commits) {
    const response = run(
      "gh",
      [
        "api",
        "-H",
        "Accept: application/vnd.github+json",
        `repos/${component.repo}/commits/${commit.sha}/pulls`,
      ],
      { allowFailure: true },
    );

    if (!response.ok) {
      warnings.push(`failed to read pull requests for ${component.repo}@${commit.short_sha}`);
      commitsWithPullRequests.push({ ...commit, pull_requests: [] });
      continue;
    }

    const pullRequests = parseJson(response.stdout || "[]", `${component.repo} pull requests`);
    const pullRequestsForCommit = [];
    for (const pullRequest of pullRequests) {
      if (!pullRequest.merged_at || pullRequest.base?.ref !== "main") {
        continue;
      }
      const normalizedPullRequest = {
        number: pullRequest.number,
        title: pullRequest.title,
        url: pullRequest.html_url,
        author: pullRequest.user?.login || "",
        merged_at: pullRequest.merged_at,
      };
      prsByNumber.set(pullRequest.number, normalizedPullRequest);
      pullRequestsForCommit.push(normalizedPullRequest);
    }
    commitsWithPullRequests.push({ ...commit, pull_requests: pullRequestsForCommit });
  }

  const directCommits = commitsWithPullRequests.filter((commit) => {
    return commit.pull_requests.length === 0;
  });

  return {
    pullRequests: Array.from(prsByNumber.values()).sort((left, right) => {
      return new Date(left.merged_at).getTime() - new Date(right.merged_at).getTime();
    }),
    commits: commitsWithPullRequests,
    directCommits,
    warnings,
  };
}

function collectInitialPullRequests(component) {
  if (SKIP_GITHUB) {
    return {
      pullRequests: [],
      commits: [],
      directCommits: [],
      warnings: ["GitHub API lookup skipped by STACK_RELEASE_SKIP_GITHUB=1"],
    };
  }

  const response = run(
    "gh",
    [
      "pr",
      "list",
      "--repo",
      component.repo,
      "--state",
      "merged",
      "--base",
      "main",
      "--limit",
      String(INITIAL_RELEASE_PR_LIMIT),
      "--json",
      "number,title,url,author,mergedAt",
    ],
    { allowFailure: true },
  );

  if (!response.ok) {
    return {
      pullRequests: [],
      commits: [],
      directCommits: [],
      warnings: [`failed to list merged pull requests for ${component.repo}`],
    };
  }

  const pullRequests = parseJson(response.stdout || "[]", `${component.repo} initial pull requests`);
  const warnings = [
    "initial release mode: previous component commit is unavailable, so PRs are listed from merged main history",
  ];

  if (pullRequests.length >= INITIAL_RELEASE_PR_LIMIT) {
    warnings.push(
      `initial release merged PR list reached STACK_INITIAL_RELEASE_PR_LIMIT=${INITIAL_RELEASE_PR_LIMIT}`,
    );
  }

  return {
    pullRequests: pullRequests
      .map((pullRequest) => ({
        number: pullRequest.number,
        title: pullRequest.title,
        url: pullRequest.url,
        author: pullRequest.author?.login || "",
        merged_at: pullRequest.mergedAt,
      }))
      .sort((left, right) => {
        return new Date(left.merged_at).getTime() - new Date(right.merged_at).getTime();
      }),
    commits: [],
    directCommits: [],
    warnings,
  };
}

function collectComponent(component, previousManifest, release) {
  const repoPath = ensureRepoPath(component);
  const currentCommit = git(repoPath, ["rev-parse", "HEAD"]);
  const remoteUrl = git(repoPath, ["remote", "get-url", "origin"], { allowFailure: true });
  const previousComponent = previousComponentFor(previousManifest, component);
  const previousCommit = previousComponent?.current_commit || previousComponent?.commit || null;
  const range = collectCommitRange(repoPath, previousCommit, currentCommit);
  if (range.boundary === "previous_commit_not_ancestor") {
    throw new Error(`${component.repo}: ${range.warning}`);
  }

  const prResult =
    range.boundary === "missing_previous_commit"
      ? collectInitialPullRequests(component)
      : collectPullRequestsForCommits(component, range.commits);

  return {
    name: component.name,
    repo: component.repo,
    role: component.role,
    path: component.path,
    remote_url: remoteUrl,
    default_branch: "main",
    previous_commit: previousCommit,
    current_commit: currentCommit,
    commit_range:
      previousCommit && range.boundary === "commit_range"
        ? `${previousCommit}..${currentCommit}`
        : null,
    commit_count: range.commits.length,
    boundary: range.boundary,
    warnings: [range.warning, ...prResult.warnings].filter(Boolean),
    commits: prResult.commits,
    direct_commits: prResult.directCommits,
    merged_pull_requests: prResult.pullRequests,
    artifacts: resolveArtifacts(component, release),
  };
}

function renderComponentTable(components) {
  const lines = [
    "| Component | Repo | Previous | Current | Commits | PRs | Direct |",
    "|---|---|---:|---:|---:|---:|---:|",
  ];

  for (const component of components) {
    lines.push(
      [
        markdownEscape(component.name),
        markdownEscape(component.repo),
        component.previous_commit ? `\`${shortSha(component.previous_commit)}\`` : "none",
        `\`${shortSha(component.current_commit)}\``,
        String(component.commit_count),
        String(component.merged_pull_requests.length),
        String(component.direct_commits.length),
      ].join(" | ").replace(/^/, "| ").replace(/$/, " |"),
    );
  }

  return lines.join("\n");
}

function renderPullRequestsForCommit(commit) {
  if (commit.pull_requests.length === 0) {
    return "PR: none";
  }
  return commit.pull_requests
    .map((pullRequest) => {
      return `PR: [#${pullRequest.number} ${markdownEscape(pullRequest.title)}](${pullRequest.url})`;
    })
    .join("; ");
}

function renderRelatedPullRequests(pullRequests) {
  if (pullRequests.length === 0) {
    return ["- No related PRs found for this release boundary."];
  }

  return pullRequests.map((pullRequest) => {
    const author = pullRequest.author ? ` by @${pullRequest.author}` : "";
    return `- [#${pullRequest.number} ${markdownEscape(pullRequest.title)}](${pullRequest.url})${author} (${pullRequest.merged_at})`;
  });
}

function renderChangesByComponent(components) {
  const lines = ["## Changes By Component", ""];

  for (const component of components) {
    lines.push(`### ${component.name}`, "");
    lines.push("#### Related Pull Requests", "");
    lines.push(...renderRelatedPullRequests(component.merged_pull_requests));
    lines.push("");
    lines.push("#### Main Commits", "");

    if (component.commits.length === 0) {
      lines.push("- No commits found for this release boundary.");
    } else {
      for (const commit of component.commits) {
        const author = commit.author_name ? ` by ${markdownEscape(commit.author_name)}` : "";
        lines.push(
          `- [\`${commit.short_sha}\`](${commitUrl(component, commit)}) ${markdownEscape(commit.title)}${author} (${commit.committed_at})`,
        );
        lines.push(`  - ${renderPullRequestsForCommit(commit)}`);
      }
    }

    for (const warning of component.warnings) {
      lines.push(`- Boundary note: ${markdownEscape(warning)}.`);
    }
    lines.push("");
  }

  return lines.join("\n");
}

function renderArtifacts(components) {
  const artifacts = components.flatMap((component) => component.artifacts);
  const lines = ["## Published Images And Artifacts", ""];

  if (artifacts.length === 0) {
    lines.push("- No published artifacts were declared for this stack release.");
  } else {
    for (const artifact of artifacts) {
      lines.push(`- \`${artifact}\``);
    }
  }

  lines.push("");
  lines.push(
    "Components without listed artifacts are tracked by repository commit in the release manifest.",
  );
  lines.push("");
  return lines.join("\n");
}

function renderNotes(manifest) {
  const lines = [
    `# Wattetheria Stack ${manifest.stack_version}`,
    "",
    "## Components",
    "",
    renderComponentTable(manifest.components),
    "",
  ];

  if (!manifest.previous_manifest.found) {
    lines.push(
      `Previous release manifest: ${manifest.previous_manifest.source}. This release records current component commits; commit-range PR aggregation starts after this release.`,
      "",
    );
  } else {
    lines.push(`Previous release manifest: ${manifest.previous_manifest.source}.`, "");
  }

  lines.push(renderChangesByComponent(manifest.components));
  lines.push(renderArtifacts(manifest.components));
  lines.push(
    [
      "## Verification",
      "",
      "- `scripts/verify-release-deployment.sh` runs inside `scripts/publish-ghcr.sh` before the Wattetheria image build.",
      "- This GitHub Release step runs after GHCR publish and `latest` tag update steps complete.",
    ].join("\n"),
  );

  return lines.join("\n");
}

function writeOutputs(manifest) {
  fs.mkdirSync(OUT_DIR, { recursive: true });
  const manifestPath = path.join(OUT_DIR, `release-manifest.${manifest.stack_version}.json`);
  const notesPath = path.join(OUT_DIR, "release-notes.md");

  fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  fs.writeFileSync(notesPath, `${renderNotes(manifest)}\n`);

  console.log(`wrote ${manifestPath}`);
  console.log(`wrote ${notesPath}`);
}

function main() {
  const release = process.env.RELEASE;
  validateRelease(release);

  const previous = findPreviousManifest(release);
  const components = COMPONENTS.map((component) =>
    collectComponent(component, previous.manifest, release),
  );

  const manifest = {
    schema_version: 1,
    stack_version: release,
    generated_at: new Date().toISOString(),
    release_repository: STACK_RELEASE_REPO,
    previous_manifest: {
      found: Boolean(previous.manifest),
      source: previous.source,
      stack_version: previous.manifest?.stack_version || null,
    },
    components,
  };

  writeOutputs(manifest);
}

main();

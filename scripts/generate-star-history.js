"use strict";

const fs = require("node:fs");
const path = require("node:path");

const API_VERSION = "2022-11-28";
const PAGE_SIZE = 100;
const OUTPUT_PATH = ".github/assets/star-history.svg";
const DAY_MS = 24 * 60 * 60 * 1000;

function parseRepository(value) {
  const repository = String(value || "").trim();
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new Error(`Invalid GitHub repository: ${repository || "(empty)"}`);
  }
  return repository;
}

function escapeXml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

function buildDailySeries(stargazers) {
  const starsByDay = new Map();

  for (const stargazer of stargazers) {
    const starredAt = new Date(stargazer?.starred_at);
    if (Number.isNaN(starredAt.getTime())) {
      throw new Error("GitHub returned a stargazer without a valid starred_at timestamp");
    }
    const day = starredAt.toISOString().slice(0, 10);
    starsByDay.set(day, (starsByDay.get(day) || 0) + 1);
  }

  let total = 0;
  return [...starsByDay.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([date, count]) => {
      total += count;
      return { date, count: total };
    });
}

function formatDate(timestamp) {
  return new Intl.DateTimeFormat("en", {
    month: "short",
    day: "numeric",
    year: "numeric",
    timeZone: "UTC",
  }).format(new Date(timestamp));
}

function formatStarCount(count) {
  if (count >= 1_000_000) {
    return `${Number((count / 1_000_000).toFixed(1))}M`;
  }
  if (count >= 1_000) {
    return `${Number((count / 1_000).toFixed(1))}K`;
  }
  return String(count);
}

function truncateMiddle(value, maximumLength) {
  if (value.length <= maximumLength) {
    return value;
  }
  const visibleLength = maximumLength - 3;
  const leftLength = Math.ceil(visibleLength / 2);
  return `${value.slice(0, leftLength)}...${value.slice(-(visibleLength - leftLength))}`;
}

function renderStarHistorySvg(repository, series) {
  const width = 960;
  const height = 540;
  const plot = { left: 96, right: 912, top: 128, bottom: 454 };
  const totalStars = series.at(-1)?.count || 0;
  const yMaximum = Math.max(1, totalStars);
  const yTickCount = Math.min(4, yMaximum);
  const dates = series.map((point) => Date.parse(`${point.date}T00:00:00Z`));
  const firstDate = dates[0] || 0;
  const lastDate = dates.at(-1) || 0;
  const dateSpan = lastDate - firstDate;
  const datePadding = Math.max(DAY_MS, dateSpan * 0.04);
  const xMinimum = firstDate ? firstDate - datePadding : 0;
  const xMaximum = firstDate
    ? Math.max(lastDate + datePadding, firstDate + DAY_MS)
    : DAY_MS;
  const xRange = xMaximum - xMinimum;
  const plotWidth = plot.right - plot.left;
  const plotHeight = plot.bottom - plot.top;
  const x = (timestamp) => plot.left + ((timestamp - xMinimum) / xRange) * plotWidth;
  const y = (count) => plot.bottom - (count / yMaximum) * plotHeight;

  const yTicks = Array.from({ length: yTickCount + 1 }, (_, index) => {
    const count = Math.round((yMaximum * index) / yTickCount);
    const yPosition = y(count);
    return [
      `<line class="tick" x1="${plot.left - 7}" y1="${yPosition}" x2="${plot.left}" y2="${yPosition}" />`,
      `<text class="axis-label" x="${plot.left - 17}" y="${yPosition + 5}" text-anchor="end">${formatStarCount(count)}</text>`,
    ].join("");
  }).join("");

  let xTickValues = [];
  if (totalStars) {
    const count = lastDate === firstDate ? 1 : 5;
    xTickValues = Array.from({ length: count }, (_, index) =>
      count === 1 ? firstDate : firstDate + (dateSpan * index) / (count - 1)
    );
  }
  const xTickYears = xTickValues.map((timestamp) =>
    String(new Date(timestamp).getUTCFullYear())
  );
  const useYearLabels =
    xTickYears.length > 1 && new Set(xTickYears).size === xTickYears.length;
  const xTicks = xTickValues
    .map((timestamp, index) => {
      const label = useYearLabels ? xTickYears[index] : formatDate(timestamp);
      return [
        `<line class="tick" x1="${x(timestamp)}" y1="${plot.bottom}" x2="${x(timestamp)}" y2="${plot.bottom + 7}" />`,
        `<text class="axis-label" x="${x(timestamp)}" y="${plot.bottom + 28}" text-anchor="middle">${escapeXml(label)}</text>`,
      ].join("");
    })
    .join("");

  let chart = `<text class="empty" x="${(plot.left + plot.right) / 2}" y="${(plot.top + plot.bottom) / 2}" text-anchor="middle">No stars yet</text>`;
  if (totalStars) {
    let pathData = `M ${plot.left} ${plot.bottom}`;
    for (let index = 0; index < series.length; index += 1) {
      pathData += ` L ${x(dates[index])} ${y(series[index].count)}`;
    }
    pathData += ` L ${plot.right} ${y(totalStars)}`;
    chart = [
      `<path class="history-line-shadow" d="${pathData}" transform="translate(0 1.5)" />`,
      `<path class="history-line" d="${pathData}" />`,
      `<circle class="point" cx="${x(lastDate)}" cy="${y(totalStars)}" r="5" />`,
    ].join("");
  }

  const safeRepository = escapeXml(repository);
  const legendRepository = truncateMiddle(repository, 32);
  const starSummary = `${totalStars} ${totalStars === 1 ? "star" : "stars"}`;
  const legendWidth = Math.min(
    440,
    Math.max(250, legendRepository.length * 9 + 105)
  );
  return [
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img" aria-labelledby="title description">`,
    `<title id="title">${safeRepository} star history</title>`,
    `<desc id="description">${totalStars} current GitHub stars shown over time.</desc>`,
    "<style>",
    ".background{fill:#ffffff}.axis,.tick{fill:none;stroke:#171717;stroke-linecap:round}.axis{stroke-width:3}.axis-sketch{fill:none;stroke:#171717;stroke-width:1.2;stroke-linecap:round;opacity:.45}.title,.legend-label,.axis-label,.axis-title,.empty{fill:#171717;font-family:'Comic Sans MS','Bradley Hand',cursive}.title{font-size:24px;font-weight:700}.legend{fill:#ffffff;stroke:#171717;stroke-width:2.2}.legend-label{font-size:15px}.axis-label{font-size:14px}.axis-title{font-size:16px;font-weight:700}.history-line-shadow{fill:none;stroke:#c52f17;stroke-width:5;stroke-linecap:round;stroke-linejoin:round;opacity:.18}.history-line{fill:none;stroke:#f4512c;stroke-width:3.4;stroke-linecap:round;stroke-linejoin:round}.point,.legend-dot{fill:#f4512c}.point{stroke:#ffffff;stroke-width:2}.title-dot{fill:#35c84a}",
    "@media(prefers-color-scheme:dark){.background{fill:#0d1117}.axis,.tick,.axis-sketch{stroke:#e6edf3}.title,.legend-label,.axis-label,.axis-title,.empty{fill:#e6edf3}.legend{fill:#161b22;stroke:#e6edf3}.history-line{stroke:#ff6842}.history-line-shadow{stroke:#ff6842}.point,.legend-dot{fill:#ff6842}.point{stroke:#0d1117}.title-dot{fill:#3fb950}}",
    "</style>",
    `<rect class="background" width="${width}" height="${height}" />`,
    '<circle class="title-dot" cx="310" cy="31" r="5" />',
    `<text class="title" x="${width / 2}" y="39" text-anchor="middle">Wattetheria Star History</text>`,
    `<rect class="legend" x="${plot.left + 12}" y="65" width="${legendWidth}" height="38" rx="5" />`,
    `<circle class="legend-dot" cx="${plot.left + 29}" cy="84" r="5" />`,
    `<text class="legend-label" x="${plot.left + 42}" y="90">${escapeXml(legendRepository)} - ${starSummary}</text>`,
    `<line class="axis" x1="${plot.left}" y1="${plot.top}" x2="${plot.left}" y2="${plot.bottom}" />`,
    `<line class="axis-sketch" x1="${plot.left + 2}" y1="${plot.top - 2}" x2="${plot.left + 1}" y2="${plot.bottom + 2}" />`,
    `<line class="axis" x1="${plot.left}" y1="${plot.bottom}" x2="${plot.right}" y2="${plot.bottom}" />`,
    `<line class="axis-sketch" x1="${plot.left - 2}" y1="${plot.bottom + 2}" x2="${plot.right + 2}" y2="${plot.bottom + 1}" />`,
    yTicks,
    xTicks,
    chart,
    `<text class="axis-title" x="${(plot.left + plot.right) / 2}" y="${height - 24}" text-anchor="middle">Date</text>`,
    `<text class="axis-title" x="28" y="${(plot.top + plot.bottom) / 2}" text-anchor="middle" transform="rotate(-90 28 ${(plot.top + plot.bottom) / 2})">GitHub Stars</text>`,
    "</svg>",
    "",
  ].join("\n");
}

async function fetchStargazers(repository, token, fetchImpl = globalThis.fetch) {
  if (!token) {
    throw new Error("GITHUB_TOKEN is required");
  }

  const stargazers = [];
  for (let page = 1; ; page += 1) {
    const url = new URL(`https://api.github.com/repos/${repository}/stargazers`);
    url.searchParams.set("per_page", String(PAGE_SIZE));
    url.searchParams.set("page", String(page));

    const response = await fetchImpl(url, {
      headers: {
        Accept: "application/vnd.github.star+json",
        Authorization: `Bearer ${token}`,
        "User-Agent": "wattetheria-star-history",
        "X-GitHub-Api-Version": API_VERSION,
      },
      signal: AbortSignal.timeout(30_000),
    });

    if (!response.ok) {
      const details = (await response.text()).trim();
      throw new Error(
        `GitHub stargazers request failed (${response.status}): ${details || response.statusText}`
      );
    }

    const pageItems = await response.json();
    if (!Array.isArray(pageItems)) {
      throw new Error("GitHub stargazers response was not an array");
    }
    stargazers.push(...pageItems);
    if (pageItems.length < PAGE_SIZE) {
      return stargazers;
    }
  }
}

async function main() {
  const repository = parseRepository(
    process.env.GITHUB_REPOSITORY || "wattetheria/wattetheria"
  );
  const outputPath = path.resolve(process.env.STAR_HISTORY_OUTPUT || OUTPUT_PATH);
  const stargazers = await fetchStargazers(repository, process.env.GITHUB_TOKEN);
  const svg = renderStarHistorySvg(repository, buildDailySeries(stargazers));

  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, svg);
  process.stdout.write(`Wrote ${outputPath} with ${stargazers.length} stars\n`);
}

if (require.main === module) {
  main().catch((error) => {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  });
}

module.exports = {
  buildDailySeries,
  fetchStargazers,
  parseRepository,
  renderStarHistorySvg,
};

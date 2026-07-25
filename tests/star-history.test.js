"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");

const {
  buildDailySeries,
  fetchStargazers,
  parseRepository,
  renderStarHistorySvg,
} = require("../scripts/generate-star-history");

test("parseRepository accepts one owner and repository pair", () => {
  assert.equal(parseRepository("wattetheria/wattetheria"), "wattetheria/wattetheria");
  assert.throws(() => parseRepository("wattetheria"), /Invalid GitHub repository/);
  assert.throws(() => parseRepository("owner/repo/extra"), /Invalid GitHub repository/);
});

test("buildDailySeries sorts stars and aggregates each UTC day", () => {
  const series = buildDailySeries([
    { starred_at: "2026-07-03T12:00:00Z" },
    { starred_at: "2026-07-01T20:00:00Z" },
    { starred_at: "2026-07-01T01:00:00Z" },
  ]);

  assert.deepEqual(series, [
    { date: "2026-07-01", count: 2 },
    { date: "2026-07-03", count: 3 },
  ]);
});

test("buildDailySeries rejects missing timestamps", () => {
  assert.throws(
    () => buildDailySeries([{ user: { login: "missing-date" } }]),
    /valid starred_at timestamp/
  );
});

test("renderStarHistorySvg handles empty and single-point histories", () => {
  const empty = renderStarHistorySvg("wattetheria/wattetheria", []);
  const single = renderStarHistorySvg("wattetheria/wattetheria", [
    { date: "2026-07-01", count: 1 },
  ]);

  assert.match(empty, /No stars yet/);
  assert.match(single, /Wattetheria Star History/);
  assert.match(single, /wattetheria\/wattetheria - 1 star/);
  assert.match(single, /Jul 1, 2026/);
  assert.match(single, /GitHub Stars/);
  assert.match(single, />Date<\/text>/);
  assert.match(single, /class="history-line"/);
  assert.doesNotMatch(single, /class="plot"/);
  assert.doesNotMatch(`${empty}${single}`, /NaN|Infinity/);
});

test("renderStarHistorySvg formats large counts and long timelines", () => {
  const svg = renderStarHistorySvg("wattetheria/wattetheria", [
    { date: "2020-01-01", count: 2_000 },
    { date: "2023-01-01", count: 6_000 },
    { date: "2026-01-01", count: 9_000 },
  ]);

  assert.match(svg, />9K<\/text>/);
  assert.match(svg, />2020<\/text>/);
  assert.match(svg, />2026<\/text>/);
  assert.doesNotMatch(svg, /NaN|Infinity/);
});

test("renderStarHistorySvg keeps medium timelines and long names readable", () => {
  const mediumTimeline = renderStarHistorySvg("wattetheria/wattetheria", [
    { date: "2024-01-01", count: 1 },
    { date: "2026-01-01", count: 2 },
  ]);
  const leapYearTimeline = renderStarHistorySvg("wattetheria/wattetheria", [
    { date: "2024-01-01", count: 1 },
    { date: "2028-01-01", count: 2 },
  ]);
  const longRepository = `${"owner".repeat(8)}/${"repository".repeat(10)}`;
  const longName = renderStarHistorySvg(longRepository, [
    { date: "2026-01-01", count: 1 },
  ]);
  const legendLabel = longName.match(/class="legend-label"[^>]*>([^<]+)<\/text>/);

  assert.match(mediumTimeline, /Jul 1, 2024/);
  assert.doesNotMatch(mediumTimeline, />2024<\/text>/);
  assert.match(leapYearTimeline, /Jan 1, 2024/);
  assert.doesNotMatch(leapYearTimeline, />2024<\/text>/);
  assert.ok(legendLabel);
  assert.match(legendLabel[1], /\.\.\./);
  assert.ok(legendLabel[1].length <= 41);
  assert.match(longName, new RegExp(`<title id="title">${longRepository} star history</title>`));
});

test("fetchStargazers authenticates and follows pagination", async () => {
  const requests = [];
  const firstPage = Array.from({ length: 100 }, (_, index) => ({
    starred_at: `2026-01-${String((index % 28) + 1).padStart(2, "0")}T00:00:00Z`,
  }));
  const pages = [firstPage, [{ starred_at: "2026-02-01T00:00:00Z" }]];
  const fetchImpl = async (url, options) => {
    requests.push({ url: String(url), options });
    return {
      ok: true,
      json: async () => pages.shift(),
    };
  };

  const result = await fetchStargazers("wattetheria/wattetheria", "test-token", fetchImpl);

  assert.equal(result.length, 101);
  assert.equal(requests.length, 2);
  assert.match(requests[0].url, /per_page=100&page=1/);
  assert.equal(requests[0].options.headers.Authorization, "Bearer test-token");
  assert.equal(requests[0].options.headers.Accept, "application/vnd.github.star+json");
});

test("fetchStargazers reports GitHub API failures", async () => {
  const fetchImpl = async () => ({
    ok: false,
    status: 403,
    statusText: "Forbidden",
    text: async () => "rate limited",
  });

  await assert.rejects(
    fetchStargazers("wattetheria/wattetheria", "test-token", fetchImpl),
    /GitHub stargazers request failed \(403\): rate limited/
  );
});

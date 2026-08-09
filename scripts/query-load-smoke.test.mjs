import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";

import {
  PROBE_REQUEST,
  PROBE_SQL,
  formatQuerySummary,
  parseQueryLoadOptions,
  runQueryLoad,
} from "./query-load-smoke.mjs";

const PROJECT_ID = "0190cafe-1234-7abc-8def-0123456789ab";
const TOKEN = "ffdb_dev_test-prefix.test-secret-that-must-not-render";

function environment(overrides = {}) {
  return {
    FFDB_QUERY_LOAD_BASE_URL: "http://127.0.0.1:5173",
    FFDB_QUERY_LOAD_PROJECT_ID: PROJECT_ID,
    FFDB_QUERY_LOAD_TOKEN: TOKEN,
    ...overrides,
  };
}

test("runs the fixed SELECT 1 probe with bounded concurrency and redacted output", async () => {
  const options = parseQueryLoadOptions(["--requests", "12", "--concurrency", "4", "--warmup", "2"], environment());
  let requestSequence = 0;
  let active = 0;
  let peakActive = 0;
  const fetchMock = async (url, request) => {
    requestSequence += 1;
    const sequence = requestSequence;
    active += 1;
    peakActive = Math.max(peakActive, active);
    assert.equal(url.href, `http://127.0.0.1:5173/v1/projects/${PROJECT_ID}/query`);
    assert.equal(url.href.includes(TOKEN), false);
    assert.equal(request.method, "POST");
    assert.equal(request.headers.authorization, `Bearer ${TOKEN}`);
    assert.deepEqual(JSON.parse(request.body), PROBE_REQUEST);
    await delay(2);
    active -= 1;
    return new Response('{"rows":[[1]]}', { status: 200, headers: { "x-request-id": `query-${sequence}` } });
  };

  const summary = await runQueryLoad(options, fetchMock);

  assert.equal(summary.ok, true);
  assert.equal(summary.operation, PROBE_SQL);
  assert.equal(summary.completed, 12);
  assert.equal(summary.totalRequestsWithEffects, 14);
  assert.equal(peakActive, 4);
  assert.deepEqual(summary.statuses, { 200: 12 });
  assert.deepEqual(summary.requestIds, { unique: 12, missing: 0, duplicate: 0 });
  assert.equal(summary.target.includes(PROJECT_ID), false);
  assert.equal(JSON.stringify(options).includes(PROJECT_ID), false);
  assert.equal(JSON.stringify(options).includes(TOKEN), false);
  assert.equal(formatQuerySummary(summary).includes(TOKEN), false);
  assert.equal(JSON.stringify(summary).includes(TOKEN), false);
});

test("rejects remote origins, missing credentials, unsafe paths and SQL, and excessive load", () => {
  assert.throws(() => parseQueryLoadOptions([], environment({ FFDB_QUERY_LOAD_TOKEN: undefined })), /TOKEN is required/u);
  assert.throws(() => parseQueryLoadOptions([], environment({ FFDB_QUERY_LOAD_PROJECT_ID: undefined })), /PROJECT_ID is required/u);
  assert.throws(() => parseQueryLoadOptions([], environment({ FFDB_QUERY_LOAD_BASE_URL: "https://example.com" })), /must target localhost/u);
  assert.throws(() => parseQueryLoadOptions([], environment({ FFDB_QUERY_LOAD_BASE_URL: "http://user:secret@127.0.0.1:5173" })), /must not contain credentials/u);
  assert.throws(() => parseQueryLoadOptions([], environment({ FFDB_QUERY_LOAD_BASE_URL: "http://127.0.0.1:5173/v1/projects/unsafe/query" })), /without a path/u);
  assert.throws(() => parseQueryLoadOptions([], environment({ FFDB_QUERY_LOAD_PROJECT_ID: "../../readyz" })), /canonical UUID/u);
  assert.throws(() => parseQueryLoadOptions([], environment({ FFDB_QUERY_LOAD_SQL: "DELETE FROM users" })), /custom SQL is not supported/u);
  assert.throws(() => parseQueryLoadOptions([], environment({ FFDB_QUERY_LOAD_PATH: "/readyz" })), /custom query paths are not supported/u);
  assert.throws(() => parseQueryLoadOptions(["--requests", "2001"], environment()), /between 1 and 2000/u);
  assert.throws(() => parseQueryLoadOptions(["--concurrency", "33"], environment()), /between 1 and 32/u);
});

test("refuses command-line credentials without rendering their values", () => {
  for (const option of ["--project-id", "--token", "--authorization"]) {
    assert.throws(
      () => parseQueryLoadOptions([option, TOKEN], environment()),
      (error) => error instanceof Error && /environment-only/u.test(error.message) && !error.message.includes(TOKEN),
    );
  }
  assert.throws(
    () => parseQueryLoadOptions([TOKEN], environment()),
    (error) => error instanceof Error && /unexpected positional/u.test(error.message) && !error.message.includes(TOKEN),
  );
});

test("redacts the bearer token even when a transport error exposes it", async () => {
  const options = parseQueryLoadOptions(["--requests", "1", "--concurrency", "1", "--warmup", "0"], environment());
  let authorization;
  const fetchMock = async (_url, request) => {
    authorization = request.headers.authorization;
    throw new Error(`upstream error at /v1/projects/${PROJECT_ID}/query for Bearer ${TOKEN}; raw=${TOKEN}`);
  };

  const summary = await runQueryLoad(options, fetchMock);
  const text = formatQuerySummary(summary);
  const json = JSON.stringify(summary);

  assert.equal(authorization, `Bearer ${TOKEN}`);
  assert.equal(summary.ok, false);
  assert.equal(text.includes(TOKEN), false);
  assert.equal(json.includes(TOKEN), false);
  assert.equal(text.includes(PROJECT_ID), false);
  assert.equal(json.includes(PROJECT_ID), false);
  assert.match(text, /\[REDACTED\]/u);
});

test("fails a query sample that reuses request IDs", async () => {
  const options = parseQueryLoadOptions(["--requests", "3", "--concurrency", "1", "--warmup", "0"], environment());
  const fetchMock = async () => new Response('{"rows":[[1]]}', { status: 200, headers: { "x-request-id": "reused" } });
  const summary = await runQueryLoad(options, fetchMock);

  assert.equal(summary.ok, false);
  assert.deepEqual(summary.requestIds, { unique: 1, missing: 0, duplicate: 2 });
});

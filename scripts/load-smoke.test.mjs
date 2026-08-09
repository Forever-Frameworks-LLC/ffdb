import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";

import { parseOptions, runLoad } from "./load-smoke.mjs";

test("runs a fixed-concurrency loopback sample and validates request IDs", async () => {
  const options = parseOptions(["--url", "http://127.0.0.1:5173/healthz", "--requests", "24", "--concurrency", "4", "--warmup", "2"]);
  let requestSequence = 0;
  let active = 0;
  let peakActive = 0;
  const fetchMock = async () => {
    requestSequence += 1;
    const requestId = `request-${requestSequence}`;
    active += 1;
    peakActive = Math.max(peakActive, active);
    await delay(2);
    active -= 1;
    return new Response('{"status":"ok"}', { status: 200, headers: { "content-type": "application/json", "x-request-id": requestId } });
  };
  const summary = await runLoad(options, fetchMock);

  assert.equal(summary.ok, true);
  assert.equal(summary.completed, 24);
  assert.deepEqual(summary.statuses, { 200: 24 });
  assert.deepEqual(summary.requestIds, { unique: 24, missing: 0, duplicate: 0 });
  assert.equal(summary.latencyMs.p95 >= 0, true);
  assert.equal(peakActive, 4);
});

test("refuses remote, credential-bearing, mutable, and unbounded targets", () => {
  assert.throws(() => parseOptions(["--url", "https://example.com/healthz"]), /must target localhost/u);
  assert.throws(() => parseOptions(["--url", "http://user:secret@127.0.0.1:8080/healthz"]), /must not contain credentials/u);
  assert.throws(() => parseOptions(["--url", "http://127.0.0.1:8080/v1/projects"]), /url path must be one of/u);
  assert.throws(() => parseOptions(["--requests", "10001"]), /between 1 and 10000/u);
  assert.throws(() => parseOptions(["--concurrency", "129"]), /between 1 and 128/u);
});

test("fails samples with reused request IDs", async () => {
  const options = parseOptions(["--url", "http://127.0.0.1:5173/readyz", "--requests", "3", "--concurrency", "1", "--warmup", "0"]);
  const repeatedIdFetch = async () => new Response('{"status":"ready"}', { status: 200, headers: { "x-request-id": "reused" } });
  const summary = await runLoad(options, repeatedIdFetch);

  assert.equal(summary.ok, false);
  assert.deepEqual(summary.requestIds, { unique: 1, missing: 0, duplicate: 2 });
});

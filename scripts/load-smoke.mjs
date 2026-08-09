#!/usr/bin/env node

import { pathToFileURL } from "node:url";

const MAX_REQUESTS = 10_000;
const MAX_CONCURRENCY = 128;
const MAX_WARMUP = 1_000;
const MAX_TIMEOUT_MS = 30_000;
const ALLOWED_PATHS = new Set(["/healthz", "/readyz", "/metrics", "/openapi.json"]);
const LOOPBACK_HOSTS = new Set(["127.0.0.1", "[::1]", "localhost"]);

export function parseOptions(arguments_, environment = process.env) {
  const values = new Map();
  const switches = new Set();
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === "--help" || argument === "--json" || argument === "--no-request-id") {
      switches.add(argument);
      continue;
    }
    if (!argument?.startsWith("--")) throw new Error(`Unexpected argument: ${argument ?? ""}`);
    const value = arguments_[index + 1];
    if (value === undefined || value.startsWith("--")) throw new Error(`${argument} requires a value`);
    values.set(argument, value);
    index += 1;
  }
  if (switches.has("--help")) return { help: true };

  const url = new URL(values.get("--url") ?? environment.FFDB_LOAD_URL ?? "http://127.0.0.1:5173/healthz");
  validateTarget(url);
  const maxP95Value = values.get("--max-p95-ms") ?? environment.FFDB_LOAD_MAX_P95_MS;
  return {
    help: false,
    url,
    requests: boundedInteger(values.get("--requests") ?? environment.FFDB_LOAD_REQUESTS ?? "300", "requests", 1, MAX_REQUESTS),
    concurrency: boundedInteger(values.get("--concurrency") ?? environment.FFDB_LOAD_CONCURRENCY ?? "12", "concurrency", 1, MAX_CONCURRENCY),
    warmup: boundedInteger(values.get("--warmup") ?? environment.FFDB_LOAD_WARMUP ?? "12", "warmup", 0, MAX_WARMUP),
    timeoutMs: boundedInteger(values.get("--timeout-ms") ?? environment.FFDB_LOAD_TIMEOUT_MS ?? "2000", "timeout-ms", 100, MAX_TIMEOUT_MS),
    expectedStatus: boundedInteger(values.get("--expected-status") ?? "200", "expected-status", 100, 599),
    maxP95Ms: maxP95Value === undefined || maxP95Value === "" ? null : boundedNumber(maxP95Value, "max-p95-ms", 0.01, MAX_TIMEOUT_MS),
    requireRequestId: !switches.has("--no-request-id"),
    json: switches.has("--json"),
  };
}

export async function runLoad(options, fetchImplementation = globalThis.fetch) {
  if (typeof fetchImplementation !== "function") throw new Error("This harness requires a runtime with fetch support");
  const warmup = await runBatch(options.warmup, Math.min(options.concurrency, Math.max(options.warmup, 1)), options, fetchImplementation);
  if (warmup.failures.length > 0) {
    return summarize(options, warmup, 0, "warmup_failed");
  }

  const started = performance.now();
  const sample = await runBatch(options.requests, options.concurrency, options, fetchImplementation);
  const elapsedMs = performance.now() - started;
  return summarize(options, sample, elapsedMs, null);
}

async function runBatch(count, concurrency, options, fetchImplementation) {
  const latencies = [];
  const statuses = new Map();
  const requestIds = new Set();
  const failures = [];
  let missingRequestIds = 0;
  let duplicateRequestIds = 0;
  let cursor = 0;

  const worker = async () => {
    while (true) {
      const requestNumber = cursor;
      cursor += 1;
      if (requestNumber >= count) return;
      const started = performance.now();
      try {
        const response = await fetchImplementation(options.url, {
          method: "GET",
          headers: { accept: "application/json, text/plain;q=0.9, */*;q=0.1", "user-agent": "ffdb-local-load-smoke/1" },
          redirect: "error",
          signal: AbortSignal.timeout(options.timeoutMs),
        });
        await response.arrayBuffer();
        const latencyMs = performance.now() - started;
        latencies.push(latencyMs);
        statuses.set(response.status, (statuses.get(response.status) ?? 0) + 1);
        if (response.status !== options.expectedStatus && failures.length < 8) {
          failures.push(`request ${requestNumber + 1}: expected HTTP ${options.expectedStatus}, received ${response.status}`);
        }
        if (options.requireRequestId) {
          const requestId = response.headers.get("x-request-id");
          if (requestId === null || requestId === "") {
            missingRequestIds += 1;
          } else if (requestIds.has(requestId)) {
            duplicateRequestIds += 1;
          } else {
            requestIds.add(requestId);
          }
        }
      } catch (error) {
        if (failures.length < 8) failures.push(`request ${requestNumber + 1}: ${safeError(error)}`);
      }
    }
  };

  await Promise.all(Array.from({ length: Math.min(count, concurrency) }, () => worker()));
  return { count, latencies, statuses, requestIds, failures, missingRequestIds, duplicateRequestIds };
}

function summarize(options, sample, elapsedMs, reason) {
  const latencies = [...sample.latencies].sort((left, right) => left - right);
  const completed = latencies.length;
  const statusFailures = [...sample.statuses.entries()].reduce((total, [status, count]) => total + (status === options.expectedStatus ? 0 : count), 0);
  const transportFailures = sample.count - completed;
  const requestIdFailures = options.requireRequestId ? sample.missingRequestIds + sample.duplicateRequestIds : 0;
  const p95Ms = percentile(latencies, 0.95);
  const thresholdFailed = options.maxP95Ms !== null && p95Ms !== null && p95Ms > options.maxP95Ms;
  const ok = reason === null && transportFailures === 0 && statusFailures === 0 && requestIdFailures === 0 && !thresholdFailed;
  return {
    ok,
    reason,
    target: options.url.href,
    requests: sample.count,
    concurrency: options.concurrency,
    elapsedMs,
    throughputRps: elapsedMs > 0 ? completed / (elapsedMs / 1_000) : 0,
    completed,
    transportFailures,
    statusFailures,
    statuses: Object.fromEntries([...sample.statuses.entries()].sort(([left], [right]) => left - right)),
    requestIds: options.requireRequestId ? { unique: sample.requestIds.size, missing: sample.missingRequestIds, duplicate: sample.duplicateRequestIds } : null,
    latencyMs: {
      average: completed === 0 ? null : latencies.reduce((sum, value) => sum + value, 0) / completed,
      p50: percentile(latencies, 0.5),
      p95: p95Ms,
      p99: percentile(latencies, 0.99),
      maximum: completed === 0 ? null : latencies.at(-1),
    },
    maxP95Ms: options.maxP95Ms,
    thresholdFailed,
    failures: sample.failures,
  };
}

export function formatSummary(summary) {
  const latency = summary.latencyMs;
  const lines = [
    "FFDB bounded local load smoke",
    `target: ${summary.target}`,
    `requests: ${summary.completed}/${summary.requests} completed at concurrency ${summary.concurrency}`,
    `throughput: ${fixed(summary.throughputRps)} requests/second`,
    `latency ms: avg ${fixed(latency.average)} | p50 ${fixed(latency.p50)} | p95 ${fixed(latency.p95)} | p99 ${fixed(latency.p99)} | max ${fixed(latency.maximum)}`,
    `statuses: ${Object.entries(summary.statuses).map(([status, count]) => `${status}=${count}`).join(", ") || "none"}`,
  ];
  if (summary.requestIds !== null) lines.push(`request IDs: ${summary.requestIds.unique} unique, ${summary.requestIds.missing} missing, ${summary.requestIds.duplicate} duplicate`);
  if (summary.maxP95Ms !== null) lines.push(`p95 budget: ${fixed(summary.maxP95Ms)} ms (${summary.thresholdFailed ? "failed" : "passed"})`);
  if (summary.reason !== null) lines.push(`result: ${summary.reason}`);
  for (const failure of summary.failures) lines.push(`failure: ${failure}`);
  lines.push(`result: ${summary.ok ? "passed" : "failed"}`);
  return lines.join("\n");
}

function validateTarget(url) {
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("url must use http or https");
  if (!LOOPBACK_HOSTS.has(url.hostname)) throw new Error("url must target localhost, 127.0.0.1, or ::1");
  if (url.username !== "" || url.password !== "") throw new Error("url must not contain credentials");
  if (!ALLOWED_PATHS.has(url.pathname)) throw new Error(`url path must be one of: ${[...ALLOWED_PATHS].join(", ")}`);
  if (url.search !== "" || url.hash !== "") throw new Error("url must not contain a query string or fragment");
}

function boundedInteger(value, name, minimum, maximum) {
  if (!/^[0-9]+$/u.test(value)) throw new Error(`${name} must be an integer`);
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) throw new Error(`${name} must be between ${minimum} and ${maximum}`);
  return parsed;
}

function boundedNumber(value, name, minimum, maximum) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < minimum || parsed > maximum) throw new Error(`${name} must be between ${minimum} and ${maximum}`);
  return parsed;
}

function percentile(sorted, quantile) {
  if (sorted.length === 0) return null;
  return sorted[Math.max(0, Math.ceil(sorted.length * quantile) - 1)];
}

function fixed(value) {
  return value === null || !Number.isFinite(value) ? "n/a" : value.toFixed(2);
}

function safeError(error) {
  if (error instanceof Error) return error.name === "TimeoutError" ? "request timed out" : error.message;
  return "request failed";
}

function usage() {
  return `Usage: node scripts/load-smoke.mjs [options]

Options:
  --url URL                 Loopback FFDB health, readiness, metrics, or OpenAPI URL
  --requests N              Measured requests (default 300; maximum ${MAX_REQUESTS})
  --concurrency N           Parallel requests (default 12; maximum ${MAX_CONCURRENCY})
  --warmup N                Warmup requests (default 12; maximum ${MAX_WARMUP})
  --timeout-ms N            Per-request timeout (default 2000; maximum ${MAX_TIMEOUT_MS})
  --expected-status N       Required HTTP status (default 200)
  --max-p95-ms N            Optional failing p95 latency budget
  --no-request-id           Do not require unique X-Request-Id headers
  --json                    Print machine-readable JSON
  --help                    Show this message

Environment equivalents: FFDB_LOAD_URL, FFDB_LOAD_REQUESTS,
FFDB_LOAD_CONCURRENCY, FFDB_LOAD_WARMUP, FFDB_LOAD_TIMEOUT_MS,
and FFDB_LOAD_MAX_P95_MS.`;
}

async function main() {
  try {
    const options = parseOptions(process.argv.slice(2));
    if (options.help) {
      console.log(usage());
      return;
    }
    const summary = await runLoad(options);
    console.log(options.json ? JSON.stringify(summary, null, 2) : formatSummary(summary));
    if (!summary.ok) process.exitCode = 1;
  } catch (error) {
    console.error(`load smoke configuration error: ${safeError(error)}`);
    process.exitCode = 2;
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) await main();

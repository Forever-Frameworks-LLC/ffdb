#!/usr/bin/env node

import { pathToFileURL } from "node:url";

const MAX_REQUESTS = 2_000;
const MAX_CONCURRENCY = 32;
const MAX_WARMUP = 100;
const MAX_TIMEOUT_MS = 30_000;
const LOOPBACK_HOSTS = new Set(["127.0.0.1", "[::1]", "localhost"]);
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;
const FORBIDDEN_CREDENTIAL_OPTIONS = new Set(["--project-id", "--token", "--authorization"]);

export const PROBE_SQL = "SELECT 1 AS ffdb_load_probe";
export const PROBE_REQUEST = Object.freeze({
  sql: PROBE_SQL,
  parameters: Object.freeze([]),
  options: Object.freeze({ max_rows: 1 }),
});

export function parseQueryLoadOptions(arguments_, environment = process.env) {
  const values = new Map();
  const switches = new Set();
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === "--help" || argument === "--json") {
      switches.add(argument);
      continue;
    }
    if (FORBIDDEN_CREDENTIAL_OPTIONS.has(argument)) {
      throw new Error("project ID and bearer token are environment-only; command-line credentials are refused");
    }
    if (!argument?.startsWith("--")) {
      throw new Error("unexpected positional argument; credentials must be supplied through environment");
    }
    if (!["--requests", "--concurrency", "--warmup", "--timeout-ms", "--max-p95-ms"].includes(argument)) {
      throw new Error("unsupported option");
    }
    const value = arguments_[index + 1];
    if (value === undefined || value.startsWith("--")) throw new Error(`${argument} requires a value`);
    values.set(argument, value);
    index += 1;
  }
  if (switches.has("--help")) return { help: true };

  rejectCustomRequest(environment);
  const projectId = requiredEnvironment(environment, "FFDB_QUERY_LOAD_PROJECT_ID");
  if (!UUID_PATTERN.test(projectId)) throw new Error("FFDB_QUERY_LOAD_PROJECT_ID must be a canonical UUID");
  const token = requiredEnvironment(environment, "FFDB_QUERY_LOAD_TOKEN");
  validateToken(token);
  const baseUrl = parseBaseUrl(environment.FFDB_QUERY_LOAD_BASE_URL ?? "http://127.0.0.1:5173");
  const url = new URL(`/v1/projects/${projectId.toLowerCase()}/query`, baseUrl);
  const maxP95Value = values.get("--max-p95-ms") ?? environment.FFDB_QUERY_LOAD_MAX_P95_MS;

  const options = {
    help: false,
    displayTarget: `${baseUrl.origin}/v1/projects/[PROJECT_ID]/query`,
    requests: boundedInteger(values.get("--requests") ?? environment.FFDB_QUERY_LOAD_REQUESTS ?? "100", "requests", 1, MAX_REQUESTS),
    concurrency: boundedInteger(values.get("--concurrency") ?? environment.FFDB_QUERY_LOAD_CONCURRENCY ?? "4", "concurrency", 1, MAX_CONCURRENCY),
    warmup: boundedInteger(values.get("--warmup") ?? environment.FFDB_QUERY_LOAD_WARMUP ?? "4", "warmup", 0, MAX_WARMUP),
    timeoutMs: boundedInteger(values.get("--timeout-ms") ?? environment.FFDB_QUERY_LOAD_TIMEOUT_MS ?? "5000", "timeout-ms", 100, MAX_TIMEOUT_MS),
    maxP95Ms: maxP95Value === undefined || maxP95Value === "" ? null : boundedNumber(maxP95Value, "max-p95-ms", 0.01, MAX_TIMEOUT_MS),
    json: switches.has("--json"),
  };
  Object.defineProperties(options, {
    url: { value: url },
    token: { value: token },
    requestBody: { value: JSON.stringify(PROBE_REQUEST) },
  });
  return options;
}

export async function runQueryLoad(options, fetchImplementation = globalThis.fetch) {
  if (typeof fetchImplementation !== "function") throw new Error("This harness requires a runtime with fetch support");
  const warmup = await runBatch(options.warmup, Math.min(options.concurrency, Math.max(options.warmup, 1)), options, fetchImplementation);
  if (warmup.failures.length > 0 || warmup.missingRequestIds > 0 || warmup.duplicateRequestIds > 0) {
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
          method: "POST",
          headers: {
            accept: "application/json",
            authorization: `Bearer ${options.token}`,
            "content-type": "application/json",
            "user-agent": "ffdb-local-query-load-smoke/1",
          },
          body: options.requestBody,
          redirect: "error",
          signal: AbortSignal.timeout(options.timeoutMs),
        });
        await response.arrayBuffer();
        const latencyMs = performance.now() - started;
        latencies.push(latencyMs);
        statuses.set(response.status, (statuses.get(response.status) ?? 0) + 1);
        if (response.status !== 200 && failures.length < 8) {
          failures.push(`request ${requestNumber + 1}: expected HTTP 200, received ${response.status}`);
        }
        const requestId = response.headers.get("x-request-id");
        if (requestId === null || requestId === "") {
          missingRequestIds += 1;
        } else if (requestIds.has(requestId)) {
          duplicateRequestIds += 1;
        } else {
          requestIds.add(requestId);
        }
      } catch (error) {
        if (failures.length < 8) failures.push(`request ${requestNumber + 1}: ${redactedError(error, options.token)}`);
      }
    }
  };

  await Promise.all(Array.from({ length: Math.min(count, concurrency) }, () => worker()));
  return { count, latencies, statuses, requestIds, failures, missingRequestIds, duplicateRequestIds };
}

function summarize(options, sample, elapsedMs, reason) {
  const latencies = [...sample.latencies].sort((left, right) => left - right);
  const completed = latencies.length;
  const statusFailures = [...sample.statuses.entries()].reduce((total, [status, count]) => total + (status === 200 ? 0 : count), 0);
  const transportFailures = sample.count - completed;
  const requestIdFailures = sample.missingRequestIds + sample.duplicateRequestIds;
  const p95Ms = percentile(latencies, 0.95);
  const thresholdFailed = options.maxP95Ms !== null && p95Ms !== null && p95Ms > options.maxP95Ms;
  const ok = reason === null && transportFailures === 0 && statusFailures === 0 && requestIdFailures === 0 && !thresholdFailed;
  return {
    ok,
    reason,
    target: options.displayTarget,
    operation: PROBE_SQL,
    measuredRequests: sample.count,
    warmupRequests: options.warmup,
    totalRequestsWithEffects: sample.count + (reason === "warmup_failed" ? 0 : options.warmup),
    concurrency: options.concurrency,
    elapsedMs,
    throughputRps: elapsedMs > 0 ? completed / (elapsedMs / 1_000) : 0,
    completed,
    transportFailures,
    statusFailures,
    statuses: Object.fromEntries([...sample.statuses.entries()].sort(([left], [right]) => left - right)),
    requestIds: { unique: sample.requestIds.size, missing: sample.missingRequestIds, duplicate: sample.duplicateRequestIds },
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

export function formatQuerySummary(summary) {
  const latency = summary.latencyMs;
  const lines = [
    "FFDB authenticated read-only query load smoke",
    `target: ${summary.target}`,
    `operation: ${summary.operation}`,
    `requests: ${summary.completed}/${summary.measuredRequests} measured at concurrency ${summary.concurrency}; ${summary.warmupRequests} warmup`,
    `throughput: ${fixed(summary.throughputRps)} requests/second`,
    `latency ms: avg ${fixed(latency.average)} | p50 ${fixed(latency.p50)} | p95 ${fixed(latency.p95)} | p99 ${fixed(latency.p99)} | max ${fixed(latency.maximum)}`,
    `statuses: ${Object.entries(summary.statuses).map(([status, count]) => `${status}=${count}`).join(", ") || "none"}`,
    `request IDs: ${summary.requestIds.unique} unique, ${summary.requestIds.missing} missing, ${summary.requestIds.duplicate} duplicate`,
    `operational effects: up to ${summary.totalRequestsWithEffects} metered/audited/rate-limited query attempts`,
  ];
  if (summary.maxP95Ms !== null) lines.push(`p95 budget: ${fixed(summary.maxP95Ms)} ms (${summary.thresholdFailed ? "failed" : "passed"})`);
  if (summary.reason !== null) lines.push(`result: ${summary.reason}`);
  for (const failure of summary.failures) lines.push(`failure: ${failure}`);
  lines.push(`result: ${summary.ok ? "passed" : "failed"}`);
  return lines.join("\n");
}

function rejectCustomRequest(environment) {
  if (environment.FFDB_QUERY_LOAD_SQL !== undefined) throw new Error("custom SQL is not supported; the harness always executes SELECT 1");
  if (environment.FFDB_QUERY_LOAD_PATH !== undefined) throw new Error("custom query paths are not supported");
}

function requiredEnvironment(environment, name) {
  const value = environment[name];
  if (typeof value !== "string" || value === "") throw new Error(`${name} is required in the environment`);
  return value;
}

function validateToken(token) {
  if (!/^[\u0021-\u007e]+$/u.test(token)) throw new Error("FFDB_QUERY_LOAD_TOKEN must contain visible ASCII without whitespace");
  if (token.length > 16 * 1_024) throw new Error("FFDB_QUERY_LOAD_TOKEN exceeds the server's 16384-byte bearer limit");
}

function parseBaseUrl(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error("FFDB_QUERY_LOAD_BASE_URL must be a valid URL");
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("base URL must use http or https");
  if (!LOOPBACK_HOSTS.has(url.hostname)) throw new Error("base URL must target localhost, 127.0.0.1, or ::1");
  if (url.username !== "" || url.password !== "") throw new Error("base URL must not contain credentials");
  if (url.pathname !== "/" || url.search !== "" || url.hash !== "") throw new Error("base URL must contain only a loopback origin, without a path, query, or fragment");
  return url;
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

function redactedError(error, token) {
  const message = error instanceof Error ? (error.name === "TimeoutError" ? "request timed out" : error.message) : "request failed";
  return redact(message, token);
}

function redact(value, token) {
  let redacted = String(value).replace(/Bearer\s+[^\s,;]+/giu, "Bearer [REDACTED]");
  if (typeof token === "string" && token !== "") redacted = redacted.split(token).join("[REDACTED]");
  redacted = redacted.replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/giu, "[PROJECT_ID]");
  return redacted;
}

function usage() {
  return `Usage: FFDB_QUERY_LOAD_PROJECT_ID=... FFDB_QUERY_LOAD_TOKEN=... node scripts/query-load-smoke.mjs [options]

This loopback-only harness always POSTs one hardcoded, non-mutating statement:
  ${PROBE_SQL}

Options:
  --requests N              Measured requests (default 100; maximum ${MAX_REQUESTS})
  --concurrency N           Parallel requests (default 4; maximum ${MAX_CONCURRENCY})
  --warmup N                Warmup requests (default 4; maximum ${MAX_WARMUP})
  --timeout-ms N            Per-request timeout (default 5000; maximum ${MAX_TIMEOUT_MS})
  --max-p95-ms N            Optional failing p95 latency budget
  --json                    Print machine-readable JSON
  --help                    Show this message

Required environment: FFDB_QUERY_LOAD_PROJECT_ID and FFDB_QUERY_LOAD_TOKEN.
Optional environment: FFDB_QUERY_LOAD_BASE_URL, FFDB_QUERY_LOAD_REQUESTS,
FFDB_QUERY_LOAD_CONCURRENCY, FFDB_QUERY_LOAD_WARMUP,
FFDB_QUERY_LOAD_TIMEOUT_MS, and FFDB_QUERY_LOAD_MAX_P95_MS.

Project ID and token command-line options are refused. The target is restricted
to a loopback origin. Custom paths and SQL are not supported.`;
}

async function main() {
  try {
    const options = parseQueryLoadOptions(process.argv.slice(2));
    if (options.help) {
      console.log(usage());
      return;
    }
    const summary = await runQueryLoad(options);
    console.log(options.json ? JSON.stringify(summary, null, 2) : formatQuerySummary(summary));
    if (!summary.ok) process.exitCode = 1;
  } catch (error) {
    console.error(`query load smoke configuration error: ${redactedError(error, process.env.FFDB_QUERY_LOAD_TOKEN)}`);
    process.exitCode = 2;
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) await main();

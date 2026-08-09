import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FFDBError, type FFDBClient, type ObservabilitySummary } from "@ffdb/client";

import { ObservabilityPanel } from "./Observability.js";

afterEach(() => cleanup());

describe("observability workspace", () => {
  it("renders retained project telemetry and searchable route and query tables", async () => {
    const client = observabilityClient();
    render(<ObservabilityPanel client={client} canViewInstance={false} />);

    expect(await screen.findAllByText("12.5 QPS")).toHaveLength(2);
    expect(screen.getAllByText("48 ms")).toHaveLength(2);
    expect(screen.getByRole("region", { name: "API route metrics" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Query fingerprint metrics" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "Entire instance" })).not.toBeInTheDocument();

    const queryRegion = screen.getByRole("region", { name: "Query fingerprint metrics" });
    expect(within(queryRegion).getByText("SELECT ? FROM ? WHERE ? = ?")).toBeInTheDocument();
    fireEvent.change(screen.getAllByPlaceholderText("Filter…")[1] as HTMLInputElement, { target: { value: "missing" } });
    expect(within(queryRegion).getByText("No matching query telemetry")).toBeInTheDocument();
    expect(client.projectObservability).toHaveBeenCalledWith("24h", expect.objectContaining({ signal: expect.any(AbortSignal) }));

    const trafficChart = screen.getByRole("img", { name: "Request throughput over time" });
    const latencyChart = screen.getByRole("img", { name: "p95 response latency over time" });
    expect(trafficChart).toHaveAttribute("data-scale-max", "20");
    expect(latencyChart).toHaveAttribute("data-scale-max", "100");
    fireEvent.focus(trafficChart);
    fireEvent.keyDown(trafficChart, { key: "ArrowLeft" });
    expect(screen.getByText("11.7")).toBeInTheDocument();
    expect(screen.getByText("45 ms")).toBeInTheDocument();
  });

  it("keeps traffic and latency readable on independent scales when their magnitudes diverge", async () => {
    const summary = observabilitySummary();
    const client = observabilityClient({
      ...summary,
      series: summary.series.map((point, index) => ({ ...point, qps: index === 0 ? 480 : 620, p95_latency_ms: index === 0 ? 190 : 210 })),
    });
    render(<ObservabilityPanel client={client} canViewInstance={false} />);

    const trafficChart = await screen.findByRole("img", { name: "Request throughput over time" });
    const latencyChart = screen.getByRole("img", { name: "p95 response latency over time" });
    expect(trafficChart).toHaveAttribute("data-scale-max", "1000");
    expect(latencyChart).toHaveAttribute("data-scale-max", "500");
    expect(screen.getByText("620 QPS")).toBeInTheDocument();
    expect(screen.getByText("210 ms")).toBeInTheDocument();
  });

  it("lets instance administrators switch to instance-wide telemetry and range", async () => {
    const client = observabilityClient();
    render(<ObservabilityPanel client={client} canViewInstance />);
    await screen.findAllByText("12.5 QPS");

    fireEvent.change(screen.getByLabelText("Scope"), { target: { value: "instance" } });
    await waitFor(() => expect(client.instanceObservability).toHaveBeenCalledWith("24h", undefined, expect.any(Object)));
    fireEvent.change(screen.getByLabelText("Range"), { target: { value: "7d" } });
    await waitFor(() => expect(client.instanceObservability).toHaveBeenCalledWith("7d", undefined, expect.any(Object)));
  });

  it("locks manual refresh during its cooldown instead of starting overlapping requests", async () => {
    const client = observabilityClient();
    render(<ObservabilityPanel client={client} canViewInstance={false} />);
    await screen.findAllByText("12.5 QPS");

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(screen.getByRole("button", { name: "Refreshing…" })).toBeDisabled();
    expect(screen.getByText("Refreshing telemetry. Manual refresh will be available again in 8 seconds.")).toBeInTheDocument();
    await waitFor(() => expect(client.projectObservability).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("button", { name: /^Refresh in [78]s$/u })).toBeDisabled();
  });

  it("surfaces rate limiting in the page and extends the refresh cooldown", async () => {
    const client = observabilityClient();
    vi.mocked(client.projectObservability)
      .mockResolvedValueOnce(observabilitySummary())
      .mockRejectedValueOnce(new FFDBError(429, { code: "rate_limited", message: "Too many requests", request_id: "request-1" }));
    render(<ObservabilityPanel client={client} canViewInstance={false} />);
    await screen.findAllByText("12.5 QPS");

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findAllByText("Refresh limit reached. Wait 15 seconds before trying again.")).toHaveLength(2);
    expect(screen.getByRole("button", { name: /^Refresh in 1[45]s$/u })).toBeDisabled();
    expect(screen.getByText("Telemetry could not be refreshed.")).toBeInTheDocument();
  });
});

function observabilityClient(summary = observabilitySummary()): FFDBClient {
  return {
    projectObservability: vi.fn().mockResolvedValue(summary),
    instanceObservability: vi.fn().mockResolvedValue({ ...summary, scope: "instance", project_id: null }),
  } as unknown as FFDBClient;
}

function observabilitySummary(): ObservabilitySummary {
  const now = 1_775_259_000_000;
  return {
    scope: "project",
    project_id: "project-1",
    generated_at_ms: now,
    window_start_ms: now - 3_600_000,
    window_end_ms: now,
    resolution_seconds: 60,
    retention_days: 30,
    current_inflight: 3,
    dropped_samples: 0,
    totals: { requests: 45_000, qps: 12.5, client_errors: 20, server_errors: 4, error_rate: 24 / 45_000, average_latency_ms: 18, p50_latency_ms: 10, p95_latency_ms: 48, p99_latency_ms: 100, max_latency_ms: 288 },
    series: [
      { timestamp_ms: now - 60_000, requests: 700, qps: 11.7, client_errors: 1, server_errors: 0, p50_latency_ms: 10, p95_latency_ms: 45, p99_latency_ms: 100 },
      { timestamp_ms: now, requests: 750, qps: 12.5, client_errors: 0, server_errors: 0, p50_latency_ms: 10, p95_latency_ms: 48, p99_latency_ms: 100 },
    ],
    busiest_routes: [{ method: "POST", route: "/v1/projects/:id/query", requests: 4_000, qps: 1.1, error_rate: 0.001, average_latency_ms: 17, p50_latency_ms: 10, p95_latency_ms: 50, p99_latency_ms: 100, max_latency_ms: 240 }],
    slowest_routes: [{ method: "POST", route: "/v1/projects/:id/query", requests: 4_000, qps: 1.1, error_rate: 0.001, average_latency_ms: 17, p50_latency_ms: 10, p95_latency_ms: 50, p99_latency_ms: 100, max_latency_ms: 240 }],
    frequent_queries: [{ fingerprint: "a".repeat(64), shape: "SELECT ? FROM ? WHERE ? = ?", statement_kind: "select", read_only: true, executions: 3_900, errors: 0, error_rate: 0, average_latency_ms: 8, p50_latency_ms: 5, p95_latency_ms: 25, p99_latency_ms: 50, max_latency_ms: 80, rows_returned: 3_900, rows_affected: 0 }],
    slow_queries: [{ fingerprint: "a".repeat(64), shape: "SELECT ? FROM ? WHERE ? = ?", statement_kind: "select", read_only: true, executions: 3_900, errors: 0, error_rate: 0, average_latency_ms: 8, p50_latency_ms: 5, p95_latency_ms: 25, p99_latency_ms: 50, max_latency_ms: 80, rows_returned: 3_900, rows_affected: 0 }],
    runtime: { healthy: true, active_workers: 2, max_workers: 16, worker_saturation: 0.125, execution_slots_in_use: 3, queue_capacity: 128, queue_saturation: 0.0234 },
    storage: { logical_database_bytes: 42_000_000, sampled_projects: 1, database_disk_total_bytes: 1_000_000_000, database_disk_available_bytes: 650_000_000, database_disk_used_percent: 35, backup_disk_total_bytes: 1_000_000_000, backup_disk_available_bytes: 710_000_000, backup_disk_used_percent: 29, last_sample_at_ms: now },
  };
}

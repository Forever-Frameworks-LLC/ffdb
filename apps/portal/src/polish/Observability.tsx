import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import {
  Activity,
  AlertTriangle,
  Clock3,
  Database,
  Gauge,
  HardDrive,
  RefreshCw,
  Search,
  Server,
  Timer,
} from "lucide-react";
import {
  FFDBError,
  type FFDBClient,
  type ObservabilityQueryMetric,
  type ObservabilityRange,
  type ObservabilityRouteMetric,
  type ObservabilitySummary,
  type ObservabilityTimePoint,
} from "@ffdb/client";

import "./observability.css";

type Scope = "project" | "instance";
type Resource =
  | { readonly status: "loading"; readonly value: ObservabilitySummary | null }
  | { readonly status: "ready"; readonly value: ObservabilitySummary }
  | { readonly status: "error"; readonly value: ObservabilitySummary | null; readonly message: string };
type Notice = { readonly id: number; readonly tone: "info" | "error"; readonly message: string };

export interface ObservabilityPanelProps {
  readonly client: FFDBClient;
  readonly canViewInstance: boolean;
}

const ranges: readonly { readonly value: ObservabilityRange; readonly label: string }[] = [
  { value: "1h", label: "Last hour" },
  { value: "6h", label: "Last 6 hours" },
  { value: "24h", label: "Last 24 hours" },
  { value: "7d", label: "Last 7 days" },
  { value: "30d", label: "Last 30 days" },
];

const manualRefreshCooldownSeconds = 8;
const rateLimitCooldownSeconds = 15;

export function ObservabilityPanel({ client, canViewInstance }: ObservabilityPanelProps) {
  const [scope, setScope] = useState<Scope>("project");
  const [range, setRange] = useState<ObservabilityRange>("24h");
  const [refreshVersion, setRefreshVersion] = useState(0);
  const [resource, setResource] = useState<Resource>({ status: "loading", value: null });
  const [refreshCooldown, setRefreshCooldown] = useState(0);
  const [notice, setNotice] = useState<Notice | null>(null);
  const refreshAvailableAt = useRef(0);
  const noticeSequence = useRef(0);
  const requestInFlight = useRef(false);

  const showNotice = useCallback((tone: Notice["tone"], message: string) => {
    noticeSequence.current += 1;
    setNotice({ id: noticeSequence.current, tone, message });
  }, []);

  const startRefreshCooldown = useCallback((seconds: number) => {
    refreshAvailableAt.current = Math.max(refreshAvailableAt.current, Date.now() + seconds * 1_000);
    setRefreshCooldown(Math.max(1, Math.ceil((refreshAvailableAt.current - Date.now()) / 1_000)));
  }, []);

  useEffect(() => {
    if (!canViewInstance && scope === "instance") setScope("project");
  }, [canViewInstance, scope]);

  useEffect(() => {
    const controller = new AbortController();
    requestInFlight.current = true;
    setResource((current) => ({ status: "loading", value: current.value }));
    const request = scope === "instance"
      ? client.instanceObservability(range, undefined, { signal: controller.signal })
      : client.projectObservability(range, { signal: controller.signal });
    void request.then(
      (value) => {
        if (controller.signal.aborted) return;
        requestInFlight.current = false;
        setResource({ status: "ready", value });
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        requestInFlight.current = false;
        const message = telemetryErrorMessage(error);
        if (isRateLimitError(error)) startRefreshCooldown(rateLimitCooldownSeconds);
        setResource((current) => ({ status: "error", value: current.value, message }));
        showNotice("error", message);
      },
    );
    return () => {
      controller.abort();
      requestInFlight.current = false;
    };
  }, [client, range, refreshVersion, scope, showNotice, startRefreshCooldown]);

  useEffect(() => {
    const interval = globalThis.setInterval(() => {
      if (!requestInFlight.current && Date.now() >= refreshAvailableAt.current) {
        setRefreshVersion((current) => current + 1);
      }
    }, 30_000);
    return () => globalThis.clearInterval(interval);
  }, []);

  const refreshCoolingDown = refreshCooldown > 0;

  useEffect(() => {
    if (!refreshCoolingDown) return;
    const interval = globalThis.setInterval(() => {
      setRefreshCooldown(Math.max(0, Math.ceil((refreshAvailableAt.current - Date.now()) / 1_000)));
    }, 250);
    return () => globalThis.clearInterval(interval);
  }, [refreshCoolingDown]);

  useEffect(() => {
    if (notice === null) return;
    const timeout = globalThis.setTimeout(() => {
      setNotice((current) => current?.id === notice.id ? null : current);
    }, 5_000);
    return () => globalThis.clearTimeout(timeout);
  }, [notice]);

  const handleManualRefresh = useCallback(() => {
    const remaining = Math.max(0, Math.ceil((refreshAvailableAt.current - Date.now()) / 1_000));
    if (remaining > 0) {
      setRefreshCooldown(remaining);
      showNotice("info", `Telemetry refresh is cooling down. Try again in ${remaining} second${remaining === 1 ? "" : "s"}.`);
      return;
    }
    if (resource.status === "loading") {
      showNotice("info", "A telemetry refresh is already in progress.");
      return;
    }
    startRefreshCooldown(manualRefreshCooldownSeconds);
    showNotice("info", "Refreshing telemetry. Manual refresh will be available again in 8 seconds.");
    setRefreshVersion((current) => current + 1);
  }, [resource.status, showNotice, startRefreshCooldown]);

  const refreshBusy = resource.status === "loading";
  const refreshLabel = refreshBusy ? "Refreshing…" : refreshCoolingDown ? `Refresh in ${refreshCooldown}s` : "Refresh";

  const data = resource.value;
  return (
    <section className="obs-page" aria-labelledby="obs-title">
      <header className="obs-toolbar">
        <div>
          <span className="obs-eyebrow"><Activity size={14} /> Operations</span>
          <h1 id="obs-title">Observability</h1>
          <p>Retained request, worker, storage, and privacy-safe query performance for the selected scope.</p>
        </div>
        <div className="obs-controls">
          <label>
            <span>Scope</span>
            <select value={scope} onChange={(event) => setScope(event.target.value as Scope)}>
              <option value="project">Current project</option>
              {canViewInstance ? <option value="instance">Entire instance</option> : null}
            </select>
          </label>
          <label>
            <span>Range</span>
            <select value={range} onChange={(event) => setRange(event.target.value as ObservabilityRange)}>
              {ranges.map((option) => <option value={option.value} key={option.value}>{option.label}</option>)}
            </select>
          </label>
          <button
            type="button"
            className="obs-refresh"
            disabled={refreshBusy || refreshCoolingDown}
            onClick={handleManualRefresh}
            title={refreshCoolingDown ? `Manual refresh available in ${refreshCooldown} seconds` : undefined}
          >
            <RefreshCw size={15} className={refreshBusy ? "is-spinning" : undefined} />
            {refreshLabel}
          </button>
        </div>
      </header>

      {resource.status === "error" ? (
        <div className="obs-alert" role="alert">
          <AlertTriangle size={17} />
          <span><strong>Telemetry could not be refreshed.</strong>{resource.message}</span>
          <button type="button" disabled={refreshBusy || refreshCoolingDown} onClick={handleManualRefresh}>{refreshCoolingDown ? `Retry in ${refreshCooldown}s` : "Try again"}</button>
        </div>
      ) : null}

      {data === null ? <ObservabilitySkeleton /> : <ObservabilityDashboard data={data} />}

      {notice === null ? null : (
        <div className={`obs-toast obs-toast--${notice.tone}`} role={notice.tone === "error" ? "alert" : "status"}>
          {notice.tone === "error" ? <AlertTriangle size={17} /> : <RefreshCw size={17} />}
          <span>{notice.message}</span>
          <button type="button" aria-label="Dismiss notification" onClick={() => setNotice(null)}>×</button>
        </div>
      )}
    </section>
  );
}

function ObservabilityDashboard({ data }: { readonly data: ObservabilitySummary }) {
  return (
    <>
      <div className="obs-stat-grid" aria-label="Performance summary">
        <Stat icon={<Gauge size={17} />} label="Average throughput" value={`${formatQps(data.totals.qps)} QPS`} detail={`${formatNumber(data.totals.requests)} requests`} />
        <Stat icon={<Timer size={17} />} label="p95 latency" value={formatLatency(data.totals.p95_latency_ms)} detail={`p50 ${formatLatency(data.totals.p50_latency_ms)} · p99 ${formatLatency(data.totals.p99_latency_ms)}`} />
        <Stat icon={<AlertTriangle size={17} />} label="Error rate" value={formatPercent(data.totals.error_rate)} detail={`${formatNumber(data.totals.server_errors)} server · ${formatNumber(data.totals.client_errors)} client`} tone={data.totals.error_rate >= 0.05 ? "warning" : "default"} />
        <Stat icon={<Activity size={17} />} label="In flight" value={formatNumber(data.current_inflight)} detail="At last refresh" />
        <Stat icon={<Clock3 size={17} />} label="Retention" value={`${data.retention_days} days`} detail={`${formatResolution(data.resolution_seconds)} chart buckets`} />
      </div>

      {data.dropped_samples > 0 ? (
        <div className="obs-alert obs-alert--warning" role="status">
          <AlertTriangle size={17} />
          <span><strong>Recorder capacity was exceeded.</strong>{formatNumber(data.dropped_samples)} samples were dropped; inspect worker and PostgreSQL saturation.</span>
        </div>
      ) : null}

      <div className="obs-primary-grid">
        <section className="obs-panel obs-chart-panel" aria-labelledby="traffic-title">
          <PanelHeading id="traffic-title" title="Traffic and latency" detail={`Independent scales · retained ${data.scope} telemetry · ${formatDateTime(data.generated_at_ms)}`} />
          <TrafficLatencyCharts points={data.series} />
        </section>
        <HealthPanel data={data} />
      </div>

      <RouteTable busiest={data.busiest_routes} slowest={data.slowest_routes} />
      <QueryTable frequent={data.frequent_queries} slowest={data.slow_queries} />
    </>
  );
}

function Stat({ icon, label, value, detail, tone = "default" }: {
  readonly icon: React.ReactNode;
  readonly label: string;
  readonly value: string;
  readonly detail: string;
  readonly tone?: "default" | "warning";
}) {
  return <article className={`obs-stat obs-stat--${tone}`}><span className="obs-stat-icon">{icon}</span><div><span>{label}</span><strong>{value}</strong><small>{detail}</small></div></article>;
}

function PanelHeading({ id, title, detail, action }: {
  readonly id: string;
  readonly title: string;
  readonly detail: string;
  readonly action?: React.ReactNode;
}) {
  return <header className="obs-panel-heading"><div><h2 id={id}>{title}</h2><p>{detail}</p></div>{action}</header>;
}

function TrafficLatencyCharts({ points }: { readonly points: readonly ObservabilityTimePoint[] }) {
  const [activeIndex, setActiveIndex] = useState<number | null>(null);
  return (
    <div className="obs-chart-wrap">
      <div className="obs-chart-pair">
        <MetricChart metric="traffic" points={points} activeIndex={activeIndex} onActiveIndex={setActiveIndex} />
        <MetricChart metric="latency" points={points} activeIndex={activeIndex} onActiveIndex={setActiveIndex} />
      </div>
    </div>
  );
}

function MetricChart({ metric, points, activeIndex, onActiveIndex }: {
  readonly metric: "traffic" | "latency";
  readonly points: readonly ObservabilityTimePoint[];
  readonly activeIndex: number | null;
  readonly onActiveIndex: (index: number | null) => void;
}) {
  const instructionsId = useId();
  const valueId = useId();
  const width = 520;
  const height = 226;
  const plot = { left: 54, top: 16, right: 14, bottom: 32 };
  const innerWidth = width - plot.left - plot.right;
  const innerHeight = height - plot.top - plot.bottom;
  const traffic = metric === "traffic";
  const values = points.map((point) => traffic ? point.qps : point.p95_latency_ms);
  const observedValues = values.filter((value): value is number => value !== null);
  const scaleMaximum = niceChartMaximum(Math.max(traffic ? 0.01 : 1, ...observedValues) * 1.08);
  const x = (index: number) => plot.left + (points.length <= 1 ? 0 : (index / (points.length - 1)) * innerWidth);
  const y = (value: number) => plot.top + innerHeight - (value / scaleMaximum) * innerHeight;
  const line = segmentedLinePath(values.map((value, index) => [x(index), value === null ? null : y(value)]));
  const area = traffic && line !== "" ? `${line} L ${x(points.length - 1)} ${plot.top + innerHeight} L ${plot.left} ${plot.top + innerHeight} Z` : "";
  const ticks = [0, 0.25, 0.5, 0.75, 1];
  const hasData = traffic ? points.some((point) => point.requests > 0) : observedValues.length > 0;
  const activePoint = activeIndex === null ? null : points[activeIndex] ?? null;
  const activeX = activeIndex === null ? 0 : x(activeIndex);
  const activeValue = activeIndex === null ? null : values[activeIndex] ?? null;
  const activeY = activeValue === null ? null : y(activeValue);
  const tooltipWidth = 156;
  const tooltipHeight = 45;
  const tooltipX = activeX > width - plot.right - tooltipWidth - 12 ? activeX - tooltipWidth - 12 : activeX + 12;
  const tooltipY = activeY === null ? plot.top + 8 : Math.max(plot.top + 2, Math.min(plot.top + innerHeight - tooltipHeight - 4, activeY - tooltipHeight - 10));
  const title = traffic ? "Traffic" : "p95 latency";
  const latestValue = values.at(-1) ?? null;
  const formattedLatestValue = traffic ? `${formatQps(latestValue ?? 0)} QPS` : formatLatency(latestValue);
  const activeDescription = activePoint === null
    ? ""
    : `${formatChartTime(activePoint.timestamp_ms)}. ${formatQps(activePoint.qps)} QPS. p95 latency ${formatLatency(activePoint.p95_latency_ms)}. ${formatNumber(activePoint.requests)} requests.`;

  const moveToPointer = (clientX: number, chart: SVGSVGElement) => {
    if (points.length === 0) return;
    const bounds = chart.getBoundingClientRect();
    if (bounds.width <= 0) return;
    const pointerX = ((clientX - bounds.left) / bounds.width) * width;
    const constrainedX = Math.max(plot.left, Math.min(width - plot.right, pointerX));
    const index = points.length === 1 ? 0 : Math.round(((constrainedX - plot.left) / innerWidth) * (points.length - 1));
    onActiveIndex(index);
  };

  const moveWithKeyboard = (key: string) => {
    if (points.length === 0) return false;
    if (key === "ArrowLeft") onActiveIndex(Math.max(0, (activeIndex ?? points.length) - 1));
    else if (key === "ArrowRight") onActiveIndex(Math.min(points.length - 1, (activeIndex ?? -1) + 1));
    else if (key === "Home") onActiveIndex(0);
    else if (key === "End") onActiveIndex(points.length - 1);
    else return false;
    return true;
  };

  return (
    <article className={`obs-metric-chart obs-metric-chart--${metric}`}>
      <header className="obs-metric-chart-heading">
        <div><span><i />{title}</span><strong>{formattedLatestValue}</strong></div>
        <small>Scale 0–{traffic ? `${formatQps(scaleMaximum)} QPS` : formatLatency(scaleMaximum)}</small>
      </header>
      <p className="sr-only" id={instructionsId}>Focus the chart and use the left and right arrow keys to inspect each time bucket.</p>
      <output className="sr-only" id={valueId} aria-live="polite">{activeDescription}</output>
      <div className="obs-chart-canvas">
        <svg
          className="obs-chart"
          viewBox={`0 0 ${width} ${height}`}
          role="img"
          aria-label={traffic ? "Request throughput over time" : "p95 response latency over time"}
          aria-describedby={`${instructionsId} ${valueId}`}
          data-metric={metric}
          data-scale-max={scaleMaximum}
          tabIndex={points.length === 0 ? -1 : 0}
          onBlur={() => onActiveIndex(null)}
          onFocus={() => onActiveIndex(activeIndex ?? points.length - 1)}
          onKeyDown={(event) => {
            if (moveWithKeyboard(event.key)) event.preventDefault();
          }}
          onMouseLeave={() => onActiveIndex(null)}
          onMouseMove={(event) => moveToPointer(event.clientX, event.currentTarget)}
        >
          {ticks.map((tick) => {
            const tickY = plot.top + innerHeight - tick * innerHeight;
            const tickValue = scaleMaximum * tick;
            return <g key={tick}><line className="obs-grid-line" x1={plot.left} x2={width - plot.right} y1={tickY} y2={tickY} /><text className="obs-axis-label" x={plot.left - 9} y={tickY + 4} textAnchor="end">{traffic ? formatQps(tickValue) : formatLatency(tickValue)}</text></g>;
          })}
          {area === "" ? null : <path className="obs-qps-area" d={area} />}
          {line === "" ? null : <path className={traffic ? "obs-qps-line" : "obs-latency-line"} d={line} />}
          {timeLabels(points).map((label) => <text className="obs-axis-label obs-axis-time" x={x(label.index)} y={height - 7} textAnchor={label.anchor} key={label.index}>{label.text}</text>)}
          <rect className="obs-chart-hit-area" x={plot.left} y={plot.top} width={innerWidth} height={innerHeight} />
          {activePoint === null ? null : (
            <g className="obs-chart-inspector" aria-hidden="true">
              <line className="obs-chart-crosshair" x1={activeX} x2={activeX} y1={plot.top} y2={plot.top + innerHeight} />
              {activeY === null ? null : <circle className={`obs-chart-point obs-chart-point--${metric}`} cx={activeX} cy={activeY} r="4" />}
              <g className="obs-chart-tooltip" transform={`translate(${tooltipX} ${tooltipY})`}>
                <rect width={tooltipWidth} height={tooltipHeight} rx="7" />
                <text className="obs-tooltip-time" x="10" y="16">{formatChartTime(activePoint.timestamp_ms)}</text>
                <circle className={`obs-tooltip-dot obs-tooltip-dot--${metric}`} cx="11" cy="32" r="2.5" />
                <text className="obs-tooltip-label" x="19" y="35">{traffic ? "QPS" : "p95"}</text>
                <text className="obs-tooltip-value" x={tooltipWidth - 10} y="35" textAnchor="end">{traffic ? formatQps(activeValue ?? 0) : formatLatency(activeValue)}</text>
              </g>
            </g>
          )}
        </svg>
        {hasData ? null : <div className="obs-chart-empty"><Activity size={20} /><strong>{traffic ? "No requests in this range" : "No latency samples in this range"}</strong><span>{traffic ? "Traffic will appear after the recorder flushes its next five-second batch." : "Latency appears after a request completes in the selected range."}</span></div>}
      </div>
    </article>
  );
}

function HealthPanel({ data }: { readonly data: ObservabilitySummary }) {
  const databaseUsed = data.storage.database_disk_used_percent;
  const backupUsed = data.storage.backup_disk_used_percent;
  return (
    <section className="obs-panel obs-health-panel" aria-labelledby="health-title">
      <PanelHeading id="health-title" title="Capacity and saturation" detail="Current worker pool and host filesystem signals" />
      <div className="obs-health-list">
        <HealthRow icon={<Server size={16} />} label="Worker processes" value={`${data.runtime.active_workers} / ${data.runtime.max_workers}`} ratio={data.runtime.worker_saturation} />
        <HealthRow icon={<Activity size={16} />} label="Execution slots" value={`${data.runtime.execution_slots_in_use} / ${data.runtime.queue_capacity} in use`} ratio={data.runtime.queue_saturation} />
        <HealthRow icon={<HardDrive size={16} />} label="Database filesystem" value={databaseUsed === null ? "Unavailable" : `${databaseUsed.toFixed(1)}% used`} ratio={(databaseUsed ?? 0) / 100} detail={diskDetail(data.storage.database_disk_total_bytes, data.storage.database_disk_available_bytes)} />
        <HealthRow icon={<Database size={16} />} label="Logical databases" value={bytes(data.storage.logical_database_bytes)} ratio={0} detail={`${data.storage.sampled_projects} sampled project${data.storage.sampled_projects === 1 ? "" : "s"}`} hideMeter />
        <HealthRow icon={<HardDrive size={16} />} label="Backup filesystem" value={backupUsed === null ? "Unavailable" : `${backupUsed.toFixed(1)}% used`} ratio={(backupUsed ?? 0) / 100} detail={diskDetail(data.storage.backup_disk_total_bytes, data.storage.backup_disk_available_bytes)} />
      </div>
    </section>
  );
}

function HealthRow({ icon, label, value, ratio, detail, hideMeter = false }: {
  readonly icon: React.ReactNode;
  readonly label: string;
  readonly value: string;
  readonly ratio: number;
  readonly detail?: string | undefined;
  readonly hideMeter?: boolean;
}) {
  const width = Math.max(0, Math.min(100, ratio * 100));
  return (
    <div className="obs-health-row">
      <span className="obs-health-icon">{icon}</span>
      <div><span>{label}</span><strong>{value}</strong>{detail === undefined ? null : <small>{detail}</small>}</div>
      {hideMeter ? null : <svg viewBox="0 0 100 5" preserveAspectRatio="none" aria-hidden="true"><rect className="obs-meter-track" width="100" height="5" rx="2.5" /><rect className={width >= 85 ? "obs-meter-fill is-warning" : "obs-meter-fill"} width={width} height="5" rx="2.5" /></svg>}
    </div>
  );
}

function RouteTable({ busiest, slowest }: {
  readonly busiest: readonly ObservabilityRouteMetric[];
  readonly slowest: readonly ObservabilityRouteMetric[];
}) {
  const [order, setOrder] = useState<"traffic" | "latency">("traffic");
  const [query, setQuery] = useState("");
  const rows = useMemo(() => {
    const source = order === "traffic" ? busiest : slowest;
    const needle = query.trim().toLowerCase();
    return needle === "" ? source : source.filter((row) => `${row.method} ${row.route}`.toLowerCase().includes(needle));
  }, [busiest, order, query, slowest]);
  return (
    <section className="obs-panel obs-table-panel" aria-labelledby="routes-title">
      <PanelHeading
        id="routes-title"
        title="API routes"
        detail="Stable route templates only; project IDs and URL parameters are excluded."
        action={<TableControls query={query} onQuery={setQuery} order={order} onOrder={(value) => setOrder(value as "traffic" | "latency")} first="Most traffic" second="Slowest p95" />}
      />
      <div className="obs-table-scroll portal-table-scroll" role="region" aria-label="API route metrics" tabIndex={0}>
        <table><thead><tr><th>Route</th><th>Requests</th><th>QPS</th><th>Errors</th><th>Average</th><th>p95</th><th>p99</th><th>Max</th></tr></thead>
          <tbody>{rows.length === 0 ? <EmptyTable colSpan={8} message="No matching route telemetry" /> : rows.map((row) => <tr key={`${row.method}:${row.route}`}><td><span className={`obs-method obs-method--${row.method.toLowerCase()}`}>{row.method}</span><code>{row.route}</code></td><td>{formatNumber(row.requests)}</td><td>{formatQps(row.qps)}</td><td>{formatPercent(row.error_rate)}</td><td>{formatLatency(row.average_latency_ms)}</td><td>{formatLatency(row.p95_latency_ms)}</td><td>{formatLatency(row.p99_latency_ms)}</td><td>{formatLatency(row.max_latency_ms)}</td></tr>)}</tbody>
        </table>
      </div>
    </section>
  );
}

function QueryTable({ frequent, slowest }: {
  readonly frequent: readonly ObservabilityQueryMetric[];
  readonly slowest: readonly ObservabilityQueryMetric[];
}) {
  const [order, setOrder] = useState<"traffic" | "latency">("traffic");
  const [query, setQuery] = useState("");
  const rows = useMemo(() => {
    const source = order === "traffic" ? frequent : slowest;
    const needle = query.trim().toLowerCase();
    return needle === "" ? source : source.filter((row) => `${row.statement_kind} ${row.shape} ${row.fingerprint}`.toLowerCase().includes(needle));
  }, [frequent, order, query, slowest]);
  return (
    <section className="obs-panel obs-table-panel" aria-labelledby="queries-title">
      <PanelHeading
        id="queries-title"
        title="Query fingerprints"
        detail="Normalized shapes preserve SQL structure while removing identifiers, comments, literals, and parameter values."
        action={<TableControls query={query} onQuery={setQuery} order={order} onOrder={(value) => setOrder(value as "traffic" | "latency")} first="Most frequent" second="Slowest p95" />}
      />
      <div className="obs-table-scroll portal-table-scroll" role="region" aria-label="Query fingerprint metrics" tabIndex={0}>
        <table className="obs-query-table"><thead><tr><th>Fingerprint</th><th>Shape</th><th>Executions</th><th>Errors</th><th>Average</th><th>p95</th><th>p99</th><th>Rows</th></tr></thead>
          <tbody>{rows.length === 0 ? <EmptyTable colSpan={8} message="No matching query telemetry" /> : rows.map((row) => <tr key={row.fingerprint}><td><span className="obs-kind">{row.statement_kind}</span><code title={row.fingerprint}>{row.fingerprint.slice(0, 12)}</code></td><td><code className="obs-query-shape">{row.shape}</code></td><td>{formatNumber(row.executions)}</td><td>{formatPercent(row.error_rate)}</td><td>{formatLatency(row.average_latency_ms)}</td><td>{formatLatency(row.p95_latency_ms)}</td><td>{formatLatency(row.p99_latency_ms)}</td><td>{formatNumber(row.rows_returned + row.rows_affected)}</td></tr>)}</tbody>
        </table>
      </div>
    </section>
  );
}

function TableControls({ query, onQuery, order, onOrder, first, second }: {
  readonly query: string;
  readonly onQuery: (value: string) => void;
  readonly order: string;
  readonly onOrder: (value: string) => void;
  readonly first: string;
  readonly second: string;
}) {
  return <div className="obs-table-controls"><label><Search size={14} /><span className="sr-only">Filter metrics</span><input type="search" value={query} onChange={(event) => onQuery(event.target.value)} placeholder="Filter…" /></label><select aria-label="Metric order" value={order} onChange={(event) => onOrder(event.target.value)}><option value="traffic">{first}</option><option value="latency">{second}</option></select></div>;
}

function EmptyTable({ colSpan, message }: { readonly colSpan: number; readonly message: string }) {
  return <tr><td className="obs-empty-cell" colSpan={colSpan}>{message}</td></tr>;
}

function ObservabilitySkeleton() {
  return <div className="obs-skeleton" aria-label="Loading observability"><div className="obs-skeleton-stats">{Array.from({ length: 5 }, (_, index) => <span key={index} />)}</div><div className="obs-skeleton-body"><span /><span /></div><span className="obs-skeleton-table" /></div>;
}

function segmentedLinePath(points: readonly (readonly [number, number | null])[]): string {
  let drawing = false;
  return points.flatMap(([x, y]) => {
    if (y === null) {
      drawing = false;
      return [];
    }
    const command = drawing ? "L" : "M";
    drawing = true;
    return [`${command} ${x.toFixed(2)} ${y.toFixed(2)}`];
  }).join(" ");
}

function niceChartMaximum(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return 1;
  const magnitude = 10 ** Math.floor(Math.log10(value));
  const normalized = value / magnitude;
  const step = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return step * magnitude;
}

function timeLabels(points: readonly ObservabilityTimePoint[]): readonly { readonly index: number; readonly text: string; readonly anchor: "start" | "middle" | "end" }[] {
  if (points.length === 0) return [];
  if (points.length === 1) {
    return [{
      index: 0,
      text: new Date(points[0]?.timestamp_ms ?? 0).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }),
      anchor: "start",
    }];
  }
  const indexes = [...new Set([0, Math.floor((points.length - 1) / 2), points.length - 1])];
  return indexes.map((index, position) => ({
    index,
    text: new Date(points[index]?.timestamp_ms ?? 0).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", ...(points.length > 100 ? { month: "short", day: "numeric" } : {}) }),
    anchor: position === 0 ? "start" : position === indexes.length - 1 ? "end" : "middle",
  }));
}

function formatNumber(value: number): string { return new Intl.NumberFormat(undefined, { notation: value >= 10_000 ? "compact" : "standard", maximumFractionDigits: 1 }).format(value); }
function formatQps(value: number): string { return value < 0.1 ? value.toFixed(2) : value < 10 ? value.toFixed(1) : formatNumber(value); }
function formatPercent(value: number): string { return `${(value * 100).toFixed(value >= 0.1 ? 1 : 2)}%`; }
function formatLatency(value: number | null): string { if (value === null) return "—"; return value >= 1_000 ? `${(value / 1_000).toFixed(2)} s` : `${value < 10 ? value.toFixed(1) : Math.round(value)} ms`; }
function formatResolution(seconds: number): string { if (seconds >= 3_600) return `${seconds / 3_600}h`; if (seconds >= 60) return `${seconds / 60}m`; return `${seconds}s`; }
function formatDateTime(value: number): string { return new Date(value).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }); }
function formatChartTime(value: number): string { return new Date(value).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit" }); }
function bytes(value: number): string { const units = ["B", "KB", "MB", "GB", "TB"]; let amount = value; let unit = 0; while (amount >= 1_000 && unit < units.length - 1) { amount /= 1_000; unit += 1; } return `${amount.toFixed(unit === 0 || amount >= 10 ? 0 : 1)} ${units[unit]}`; }
function diskDetail(total: number | null, available: number | null): string | undefined { return total === null || available === null ? undefined : `${bytes(available)} available of ${bytes(total)}`; }
function isRateLimitError(error: unknown): boolean { return error instanceof FFDBError && error.status === 429; }
function telemetryErrorMessage(error: unknown): string {
  if (isRateLimitError(error)) return "Refresh limit reached. Wait 15 seconds before trying again.";
  return error instanceof Error ? error.message : "The request failed.";
}

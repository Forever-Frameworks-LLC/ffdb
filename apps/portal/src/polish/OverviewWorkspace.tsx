import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type FormEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";

import {
  type AuditLogEntry,
  type BackupSummary,
  FFDBClient,
  type OrganizationMembershipSummary,
  type OrganizationRole,
  type OrganizationSummary,
  type ObservabilitySummary,
  type PolicyDefinition,
  type ProjectSummary,
  type SchemaSnapshot,
  type StorageBucket,
} from "@ffdb/client";

import {
  issuePortalProjectCredential,
  persistPortalProject,
  portalProjectKey,
  type PortalConfiguration,
} from "../config.js";
import { Icon, type IconName } from "../icons.js";

import "./overview-workspace.css";

export type OverviewDestination =
  | "Activity"
  | "Backups"
  | "Database"
  | "Members"
  | "Migrations"
  | "Observability"
  | "Policies"
  | "Settings"
  | "SQL Editor"
  | "Storage";

export interface PolishedOverviewPanelProps {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onNavigate(destination: OverviewDestination): void;
}

interface OverviewAvailability {
  readonly health: boolean;
  readonly readiness: boolean;
  readonly observability: boolean;
  readonly schema: boolean;
  readonly policies: boolean;
  readonly activity: boolean;
  readonly backups: boolean;
  readonly storage: boolean;
  readonly organization: boolean;
}

interface OverviewSnapshot {
  readonly health: string | null;
  readonly readiness: string | null;
  readonly observability: ObservabilitySummary | null;
  readonly schema: SchemaSnapshot | null;
  readonly policies: readonly PolicyDefinition[];
  readonly logs: readonly AuditLogEntry[];
  readonly backups: readonly BackupSummary[];
  readonly buckets: readonly StorageBucket[];
  readonly organization: OrganizationSummary | null;
  readonly availability: OverviewAvailability;
  readonly unavailable: readonly string[];
}

interface SettledRead<T> {
  readonly available: boolean;
  readonly label: string;
  readonly value: T;
}

async function optionalRead<T>(label: string, read: () => Promise<T>, fallback: T): Promise<SettledRead<T>> {
  try {
    return { available: true, label, value: await read() };
  } catch {
    return { available: false, label, value: fallback };
  }
}

export function PolishedOverviewPanel({
  client,
  configuration,
  onNavigate,
}: PolishedOverviewPanelProps) {
  const [revision, setRevision] = useState(0);
  const [snapshot, setSnapshot] = useState<OverviewSnapshot | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedEvent, setSelectedEvent] = useState<AuditLogEntry | null>(null);

  const load = useCallback(async () => {
    setLoadError(null);
    setSnapshot(null);
    try {
      const [health, readiness, observability, schema, policies, activity, backups, storage, organizations] = await Promise.all([
        optionalRead("API health", () => client.health(), null),
        optionalRead("readiness", () => client.readiness(), null),
        optionalRead<ObservabilitySummary | null>("performance telemetry", () => client.projectObservability("1h"), null),
        optionalRead<SchemaSnapshot | null>("database schema", () => client.schema(), null),
        optionalRead<readonly PolicyDefinition[]>("policies", () => client.policies(), []),
        optionalRead<readonly AuditLogEntry[]>("activity", () => client.logs({ limit: 25 }), []),
        optionalRead<readonly BackupSummary[]>("backups", () => client.backups(), []),
        optionalRead<readonly StorageBucket[]>("storage", () => client.storage.buckets(), []),
        optionalRead<readonly OrganizationSummary[]>("organization access", () => client.organizations(), []),
      ]);
      const reads = [health, readiness, observability, schema, policies, activity, backups, storage, organizations];
      if (reads.every((read) => !read.available)) {
        throw new Error("No project services could be reached with the active project and credential.");
      }
      const normalizedPolicies = Array.isArray(policies.value) ? policies.value : [];
      const normalizedActivity = Array.isArray(activity.value) ? activity.value : [];
      const normalizedBackups = Array.isArray(backups.value) ? backups.value : [];
      const normalizedStorage = Array.isArray(storage.value) ? storage.value : [];
      const normalizedOrganizations = Array.isArray(organizations.value) ? organizations.value : [];
      const normalizedObservability = isObservabilitySummary(observability.value) ? observability.value : null;
      const invalidResponses = [
        ["policies", policies.available && !Array.isArray(policies.value)],
        ["activity", activity.available && !Array.isArray(activity.value)],
        ["backups", backups.available && !Array.isArray(backups.value)],
        ["storage", storage.available && !Array.isArray(storage.value)],
        ["organization access", organizations.available && !Array.isArray(organizations.value)],
        ["performance telemetry", observability.available && normalizedObservability === null],
      ] as const;
      const organization = normalizedOrganizations.find((item) => item.id === configuration.organizationId)
        ?? normalizedOrganizations.find((item) => item.name === configuration.organizationName)
        ?? null;
      setSnapshot({
        health: health.value?.status ?? null,
        readiness: readiness.value?.status ?? null,
        observability: normalizedObservability,
        schema: schema.value,
        policies: normalizedPolicies,
        logs: normalizedActivity,
        backups: normalizedBackups,
        buckets: normalizedStorage,
        organization,
        availability: {
          health: health.available,
          readiness: readiness.available,
          observability: observability.available && normalizedObservability !== null,
          schema: schema.available,
          policies: policies.available && Array.isArray(policies.value),
          activity: activity.available && Array.isArray(activity.value),
          backups: backups.available && Array.isArray(backups.value),
          storage: storage.available && Array.isArray(storage.value),
          organization: organizations.available && Array.isArray(organizations.value),
        },
        unavailable: [...new Set([
          ...reads.filter((read) => !read.available).map((read) => read.label),
          ...invalidResponses.filter(([, invalid]) => invalid).map(([label]) => label),
        ])],
      });
    } catch (cause) {
      setLoadError(errorMessage(cause));
    }
  }, [client, configuration.organizationId, configuration.organizationName, revision]);

  useEffect(() => {
    void load();
  }, [load]);

  if (loadError !== null) {
    return (
      <PanelError
        detail={loadError}
        title="Project overview could not be loaded"
        onRetry={() => setRevision((value) => value + 1)}
      />
    );
  }
  if (snapshot === null) return <OverviewSkeleton />;

  const role = snapshot.organization?.role ?? null;
  const canDevelop = role === null || role === "owner" || role === "admin" || role === "developer";
  const canManageMembers = role === "owner" || role === "admin";
  const tableCount = snapshot.schema?.tables.length ?? null;
  const rlsCount = snapshot.schema?.tables.filter((table) => table.rls_enabled).length ?? null;
  const requests = snapshot.observability?.totals.requests ?? null;
  const p95 = snapshot.observability?.totals.p95_latency_ms ?? null;

  return (
    <div className="ow-root">
      {snapshot.unavailable.length === 0 ? null : (
        <div className="ow-notice" role="status">
          <Icon name="shield" size={17} />
          <div>
            <strong>Some project data is unavailable</strong>
            <span>{sentenceList(snapshot.unavailable)} could not be read. Available values below remain live.</span>
          </div>
          <button type="button" onClick={() => onNavigate("Settings")}>Review access</button>
        </div>
      )}

      <section className="ow-overview-heading" aria-labelledby="ow-project-status-title">
        <div>
          <span className="ow-eyebrow">Active project</span>
          <h2 id="ow-project-status-title">{configuration.projectName}</h2>
          <p>Live service, schema, security, and operations signals for this project.</p>
        </div>
        <StatusPill
          available={snapshot.availability.readiness}
          label={snapshot.readiness === null ? "Readiness unavailable" : `Readiness: ${snapshot.readiness}`}
          positive={snapshot.readiness === "ready" || snapshot.readiness === "ok"}
        />
      </section>

      <div className="ow-signal-grid" aria-label="Project status summary">
        <SignalCard
          detail={snapshot.health === null ? "The health endpoint could not be read." : `Reported by ${configuration.apiUrl}`}
          icon="terminal"
          label="API health"
          value={snapshot.health ?? "Unavailable"}
          available={snapshot.availability.health}
        />
        <SignalCard
          detail={tableCount === null ? "Schema access is unavailable." : `Schema version ${snapshot.schema?.version ?? 0}`}
          icon="database"
          label="Database tables"
          value={tableCount === null ? "Unavailable" : formatNumber(tableCount)}
          available={snapshot.availability.schema}
        />
        <SignalCard
          detail={rlsCount === null || tableCount === null ? "Policy coverage is unavailable." : `${rlsCount} of ${tableCount} tables have RLS enabled`}
          icon="shield"
          label="RLS-protected tables"
          value={rlsCount === null ? "Unavailable" : formatNumber(rlsCount)}
          available={snapshot.availability.schema}
        />
        <SignalCard
          detail={requests === null ? "Project performance telemetry is unavailable." : `${compactNumber(requests)} project requests retained over the last hour`}
          icon="chart"
          label="p95 response time"
          value={!snapshot.availability.observability ? "Unavailable" : p95 === null ? "No samples" : formatLatency(p95)}
          available={snapshot.availability.observability}
        />
      </div>

      <ProjectPerformanceCard
        available={snapshot.availability.observability}
        data={snapshot.observability}
        onOpen={() => onNavigate("Observability")}
      />

      <section className="ow-card ow-project-workspace" aria-label="Project workspace">
        <CardHeading
          description="Open common project tools or inspect the resources reported by FFDB."
          title="Project workspace"
          action={<button type="button" className="ow-button ow-button-quiet" onClick={() => onNavigate("Database")}>Inspect schema</button>}
        />
        <div className="ow-project-workspace-body">
          <section className="ow-project-workspace-section ow-capabilities" aria-labelledby="ow-capabilities-title">
            <header className="ow-project-workspace-heading"><div><h4 id="ow-capabilities-title">Project resources</h4><p>Live project inventory</p></div></header>
            <div className="ow-resource-grid">
            <ResourceCell
              available={snapshot.availability.policies}
              icon="shield"
              label="Policies"
              value={snapshot.availability.policies ? formatNumber(snapshot.policies.length) : "Unavailable"}
              onOpen={() => onNavigate("Policies")}
            />
            <ResourceCell
              available={snapshot.availability.storage}
              icon="archive"
              label="Storage buckets"
              value={snapshot.availability.storage ? formatNumber(snapshot.buckets.length) : "Unavailable"}
              onOpen={() => onNavigate("Storage")}
            />
            <ResourceCell
              available={snapshot.availability.backups}
              icon="backup"
              label="Backups"
              value={snapshot.availability.backups ? formatNumber(snapshot.backups.length) : "Unavailable"}
              onOpen={() => onNavigate("Backups")}
            />
            <ResourceCell
              available={snapshot.availability.activity}
              icon="list"
              label="Audit events"
              value={snapshot.availability.activity ? formatNumber(snapshot.logs.length) : "Unavailable"}
              onOpen={() => onNavigate("Activity")}
            />
            </div>
          </section>

          <section className="ow-project-workspace-section ow-quick-actions" aria-labelledby="ow-actions-title">
            <header className="ow-project-workspace-heading"><div><h4 id="ow-actions-title">Quick actions</h4><p>{role === null ? "Available to the active credential" : `${roleLabel(role)} access`}</p></div></header>
            <div className="ow-action-list">
            {canDevelop ? (
              <>
                <ActionButton icon="terminal" label="Run a SQL query" detail="Open the project SQL workspace" onClick={() => onNavigate("SQL Editor")} />
                <ActionButton icon="code" label="Create a migration" detail="Open a versioned migration draft" onClick={() => onNavigate("Migrations")} />
                <ActionButton icon="archive" label="Create a bucket" detail="Open storage configuration" onClick={() => onNavigate("Storage")} />
              </>
            ) : (
              <ActionButton icon="database" label="Inspect the database" detail="View schema and migration history" onClick={() => onNavigate("Database")} />
            )}
            {canManageMembers ? <ActionButton icon="users" label="Invite a member" detail="Open organization membership" onClick={() => onNavigate("Members")} /> : null}
            </div>
          </section>
        </div>
      </section>

      <RecentActivityCard
        available={snapshot.availability.activity}
        logs={snapshot.logs}
        onOpenAll={() => onNavigate("Activity")}
        onOpenEvent={setSelectedEvent}
      />

      {selectedEvent === null ? null : <ActivityDetails event={selectedEvent} onClose={() => setSelectedEvent(null)} />}
    </div>
  );
}

function OverviewSkeleton() {
  return (
    <div className="ow-root" aria-busy="true" aria-label="Loading project overview">
      <div className="ow-skeleton ow-skeleton-heading" />
      <div className="ow-signal-grid">
        {Array.from({ length: 4 }, (_, index) => <div className="ow-skeleton ow-skeleton-signal" key={index} />)}
      </div>
      <div className="ow-skeleton ow-skeleton-performance" />
      <div className="ow-skeleton ow-skeleton-workspace" />
      <div className="ow-skeleton ow-skeleton-table" />
    </div>
  );
}

function ProjectPerformanceCard({ available, data, onOpen }: {
  readonly available: boolean;
  readonly data: ObservabilitySummary | null;
  readonly onOpen: () => void;
}) {
  const points = data?.series ?? [];
  const width = 720;
  const height = 116;
  const inset = { left: 8, top: 8, right: 8, bottom: 8 };
  const innerWidth = width - inset.left - inset.right;
  const innerHeight = height - inset.top - inset.bottom;
  const maxQps = Math.max(0.01, ...points.map((point) => point.qps));
  const x = (index: number) => inset.left + (points.length <= 1 ? 0 : (index / (points.length - 1)) * innerWidth);
  const y = (value: number) => inset.top + innerHeight - (value / maxQps) * innerHeight;
  const line = overviewLinePath(points.map((point, index) => [x(index), y(point.qps)]));
  const area = line === "" ? "" : `${line} L ${x(points.length - 1)} ${height - inset.bottom} L ${inset.left} ${height - inset.bottom} Z`;
  return <section className="ow-card ow-performance-card" aria-label="Traffic and performance">
    <CardHeading
      description="Retained project telemetry from the last hour."
      title="Traffic and performance"
      action={<button type="button" className="ow-button ow-button-quiet" onClick={onOpen}>Open observability <Icon name="chevronRight" size={13} /></button>}
    />
    {!available || data === null ? <InlineState icon="chart" title="Performance telemetry unavailable" detail="The project recorder could not be read with the active session." /> : <div className="ow-performance-body">
      <dl className="ow-performance-metrics" aria-label="One-hour performance metrics">
        <div><dt>Requests</dt><dd>{compactNumber(data.totals.requests)}</dd></div>
        <div><dt>Average QPS</dt><dd>{formatQps(data.totals.qps)}</dd></div>
        <div><dt>Error rate</dt><dd>{formatPercent(data.totals.error_rate)}</dd></div>
        <div><dt>In flight</dt><dd>{formatNumber(data.current_inflight)}</dd></div>
      </dl>
      <div className="ow-performance-chart">
        <div><span>Throughput</span><small>QPS · last hour</small></div>
        <svg viewBox={`0 0 ${width} ${height}`} role="img" aria-label="Project request throughput over the last hour">
          <line className="ow-performance-baseline" x1={inset.left} x2={width - inset.right} y1={height - inset.bottom} y2={height - inset.bottom} />
          {area === "" ? null : <path className="ow-performance-area" d={area} />}
          {line === "" ? null : <path className="ow-performance-line" d={line} />}
        </svg>
        {points.some((point) => point.requests > 0) ? null : <span className="ow-performance-empty">No requests recorded in this window</span>}
      </div>
    </div>}
  </section>;
}

function SignalCard({ available, detail, icon, label, value }: {
  readonly available: boolean;
  readonly detail: string;
  readonly icon: IconName;
  readonly label: string;
  readonly value: string;
}) {
  return (
    <article className="ow-card ow-signal-card">
      <div className="ow-signal-label"><span className={available ? "ow-icon-well is-positive" : "ow-icon-well is-muted"}><Icon name={icon} size={16} /></span><span>{label}</span></div>
      <strong className={available ? undefined : "is-unavailable"}>{value}</strong>
      <p>{detail}</p>
    </article>
  );
}

function StatusPill({ available, label, positive }: { readonly available: boolean; readonly label: string; readonly positive: boolean }) {
  return <span className={`ow-status-pill ${!available ? "is-muted" : positive ? "is-positive" : "is-attention"}`}><i />{label}</span>;
}

function CardHeading({ action, description, title }: { readonly action?: ReactNode; readonly description: string; readonly title: string }) {
  return <header className="ow-card-heading"><div><h3>{title}</h3><p>{description}</p></div>{action}</header>;
}

function ResourceCell({ available, icon, label, onOpen, value }: {
  readonly available: boolean;
  readonly icon: IconName;
  readonly label: string;
  readonly value: string;
  onOpen(): void;
}) {
  return (
    <button className="ow-resource-cell" type="button" onClick={onOpen}>
      <span className="ow-icon-well"><Icon name={icon} size={17} /></span>
      <span><small>{label}</small><strong className={available ? undefined : "is-unavailable"}>{value}</strong></span>
      <Icon name="chevronRight" size={15} />
    </button>
  );
}

function ActionButton({ detail, icon, label, onClick, primary = false }: {
  readonly detail: string;
  readonly icon: IconName;
  readonly label: string;
  readonly primary?: boolean;
  onClick(): void;
}) {
  return (
    <button className={primary ? "ow-action-button is-primary" : "ow-action-button"} type="button" onClick={onClick}>
      <span className="ow-icon-well"><Icon name={icon} size={17} /></span>
      <span><strong>{label}</strong><small>{detail}</small></span>
      <Icon name="chevronRight" size={16} />
    </button>
  );
}

function RecentActivityCard({ available, logs, onOpenAll, onOpenEvent }: {
  readonly available: boolean;
  readonly logs: readonly AuditLogEntry[];
  onOpenAll(): void;
  onOpenEvent(event: AuditLogEntry): void;
}) {
  const visible = logs.slice(0, 6);
  return (
    <section className="ow-card ow-activity-card" aria-labelledby="ow-activity-title">
      <CardHeading
        title="Recent activity"
        description="Security and lifecycle events recorded by this project."
        action={<button className="ow-button ow-button-quiet" type="button" onClick={onOpenAll}>View all activity <Icon name="external" size={13} /></button>}
      />
      {!available ? (
        <InlineState icon="list" title="Activity is unavailable" detail="Audit logs could not be read with the active credential." />
      ) : visible.length === 0 ? (
        <InlineState icon="list" title="No activity recorded yet" detail="Queries, migrations, auth, storage, and administrative events will appear here." />
      ) : (
        <div className="ow-table-scroll portal-table-scroll" role="region" aria-label="Recent activity records" tabIndex={0}>
          <table className="ow-table ow-activity-table">
            <thead><tr><th>Time</th><th>Actor</th><th>Action</th><th>Resource</th><th>Outcome</th><th><span className="ow-sr-only">Details</span></th></tr></thead>
            <tbody>{visible.map((entry) => (
              <tr key={entry.id}>
                <td data-label="Time">{formatDateTime(entry.occurred_at_ms)}</td>
                <td data-label="Actor"><span className="ow-actor"><i>{initials(entry.actor)}</i><span>{friendlyActor(entry.actor)}</span></span></td>
                <td data-label="Action">{friendlyAction(entry.action)}</td>
                <td data-label="Resource"><span className="ow-resource"><Icon name={resourceIcon(entry.resource)} size={15} />{entry.resource}</span></td>
                <td data-label="Outcome"><OutcomePill outcome={entry.outcome} /></td>
                <td data-label="Details"><button className="ow-row-action" type="button" aria-label={`View details for ${friendlyAction(entry.action)}`} onClick={() => onOpenEvent(entry)}>View <Icon name="chevronRight" size={13} /></button></td>
              </tr>
            ))}</tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function OutcomePill({ outcome }: { readonly outcome: AuditLogEntry["outcome"] }) {
  return <span className={`ow-outcome is-${outcome}`}>{outcome === "success" ? <Icon name="check" size={11} /> : <span>!</span>}{capitalize(outcome)}</span>;
}

function ActivityDetails({ event, onClose }: { readonly event: AuditLogEntry; onClose(): void }) {
  useEscape(onClose);
  return (
    <Modal ariaLabelledBy="ow-event-title" onClose={onClose}>
      <div className="ow-modal-heading">
        <span className="ow-icon-well"><Icon name={resourceIcon(event.resource)} size={18} /></span>
        <div><span className="ow-eyebrow">Audit event</span><h2 id="ow-event-title">{friendlyAction(event.action)}</h2></div>
        <button className="ow-icon-button" aria-label="Close event details" type="button" onClick={onClose}>×</button>
      </div>
      <dl className="ow-detail-list">
        <div><dt>Outcome</dt><dd><OutcomePill outcome={event.outcome} /></dd></div>
        <div><dt>Occurred</dt><dd>{new Date(event.occurred_at_ms).toLocaleString()}</dd></div>
        <div><dt>Actor</dt><dd>{event.actor}</dd></div>
        <div><dt>Resource</dt><dd>{event.resource}</dd></div>
        <div><dt>Event ID</dt><dd><code>{event.id}</code></dd></div>
        <div><dt>Request ID</dt><dd>{event.request_id === null ? "Not recorded" : <code>{event.request_id}</code>}</dd></div>
      </dl>
      <div className="ow-modal-actions"><button className="ow-button" type="button" onClick={onClose}>Close</button></div>
    </Modal>
  );
}

export type WorkspaceDestination = "Overview" | "Settings" | "Projects" | "Members";

export interface PolishedWorkspacePanelProps {
  readonly view: "projects" | "members";
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onConfiguration(value: PortalConfiguration): void;
  onNavigate(destination: WorkspaceDestination): void;
  onNotice(message: string): void;
  onSetupRequired(): void;
}

interface WorkspaceSnapshot {
  readonly organizations: readonly OrganizationSummary[];
  readonly projects: readonly ProjectSummary[];
  readonly members: readonly OrganizationMembershipSummary[];
  readonly activeOrganization: OrganizationSummary | null;
  readonly currentUserId: string | null;
}

export function PolishedWorkspacePanel({
  view,
  client,
  configuration,
  onConfiguration,
  onNavigate,
  onNotice,
  onSetupRequired,
}: PolishedWorkspacePanelProps) {
  const [revision, setRevision] = useState(0);
  const [snapshot, setSnapshot] = useState<WorkspaceSnapshot | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [createOrganizationOpen, setCreateOrganizationOpen] = useState(false);
  const [createProjectOpen, setCreateProjectOpen] = useState(false);
  const [inviteOpen, setInviteOpen] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [projectQuery, setProjectQuery] = useState("");

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const organizations = await client.organizations();
      const activeOrganization = organizations.find((organization) => organization.id === configuration.organizationId)
        ?? organizations.find((organization) => organization.name === configuration.organizationName)
        ?? organizations[0]
        ?? null;
      const [projects, members, session] = activeOrganization === null
        ? [[], [], null] as const
        : await Promise.all([
          client.projects(activeOrganization.id),
          client.organizationMembers(activeOrganization.id),
          client.developerSession().catch(() => null),
        ]);
      setSnapshot({ organizations, projects, members, activeOrganization, currentUserId: session?.user_id ?? null });
      if (activeOrganization !== null && configuration.organizationId !== activeOrganization.id) {
        persistPortalProject(configuration.projectId, configuration.developerKey, activeOrganization.name, activeOrganization.id, configuration.projectName, configuration.apiUrl);
        onConfiguration({ ...configuration, organizationId: activeOrganization.id, organizationName: activeOrganization.name });
      }
    } catch (cause) {
      setLoadError(errorMessage(cause));
    }
  }, [client, configuration, onConfiguration, revision]);

  useEffect(() => {
    void load();
  }, [load]);

  const refresh = () => setRevision((value) => value + 1);

  const activateProject = async (project: ProjectSummary) => {
    const organization = snapshot?.activeOrganization;
    if (organization === null || organization === undefined || pending !== null) return;
    setPending(`activate:${project.id}`);
    setActionError(null);
    try {
      client.setProjectId(project.id);
      let issuedKey = portalProjectKey(configuration.apiUrl, project.id);
      if (issuedKey === undefined && (organization.role === "owner" || organization.role === "admin")) {
        try {
          issuedKey = await issuePortalProjectCredential(client);
        } catch {
          issuedKey = undefined;
        }
      }
      client.setDeveloperKey(issuedKey ?? null);
      persistPortalProject(project.id, issuedKey, organization.name, organization.id, project.name, configuration.apiUrl);
      onConfiguration({
        ...configuration,
        organizationId: organization.id,
        organizationName: organization.name,
        projectId: project.id,
        projectName: project.name,
        developerKey: issuedKey,
      });
      onNotice(issuedKey === undefined
        ? `${project.name} is active. Project data actions require an existing scoped key or organization administrator access.`
        : `${project.name} is now the active project.`);
      onNavigate("Overview");
    } catch (cause) {
      setActionError(errorMessage(cause));
    } finally {
      setPending(null);
    }
  };

  if (loadError !== null) {
    return <PanelError title="Workspace could not be loaded" detail={loadError} onRetry={refresh} />;
  }
  if (snapshot === null) return <WorkspaceSkeleton view={view} />;

  if (snapshot.activeOrganization === null) {
    return (
      <WorkspaceEmpty
        icon="users"
        title="Create your first organization"
        detail="An organization owns projects, members, usage, and billing. Create one here to continue onboarding."
        actionLabel="Create organization"
        onAction={() => setCreateOrganizationOpen(true)}
      >
        {createOrganizationOpen ? (
          <CreateOrganizationModal
            client={client}
            configuration={configuration}
            onClose={() => setCreateOrganizationOpen(false)}
            onConfiguration={onConfiguration}
            onCreated={() => { setCreateOrganizationOpen(false); refresh(); }}
            onNotice={onNotice}
            onSetupRequired={onSetupRequired}
          />
        ) : null}
      </WorkspaceEmpty>
    );
  }

  const organization = snapshot.activeOrganization;
  const canManage = organization.role === "owner" || organization.role === "admin";

  if (snapshot.projects.length === 0 && view === "projects") {
    return (
      <WorkspaceEmpty
        icon="database"
        title={`Create ${organization.name}'s first project`}
        detail={canManage ? "Each project receives an isolated database, auth, storage, sync, email, and commerce boundary." : "An organization owner or administrator must create the first project."}
        actionLabel={canManage ? "Create project" : undefined}
        onAction={canManage ? () => setCreateProjectOpen(true) : undefined}
      >
        {createProjectOpen ? (
          <CreateProjectModal
            client={client}
            organization={organization}
            onActivate={activateProject}
            onClose={() => setCreateProjectOpen(false)}
            onCreated={() => { setCreateProjectOpen(false); refresh(); }}
            onSetupRequired={onSetupRequired}
          />
        ) : null}
      </WorkspaceEmpty>
    );
  }

  if (view === "members") {
    return (
      <MembersWorkspace
        canManage={canManage}
        client={client}
        currentUserId={snapshot.currentUserId}
        error={actionError}
        members={snapshot.members}
        organization={organization}
        pending={pending}
        projectCount={snapshot.projects.length}
        onClearError={() => setActionError(null)}
        onError={setActionError}
        onInvite={() => setInviteOpen(true)}
        onNotice={onNotice}
        onPending={setPending}
        onRefresh={refresh}
      >
        {inviteOpen ? (
          <InviteMemberModal
            client={client}
            organization={organization}
            onClose={() => setInviteOpen(false)}
            onInvited={() => { setInviteOpen(false); refresh(); }}
            onNotice={onNotice}
          />
        ) : null}
      </MembersWorkspace>
    );
  }

  const normalizedProjectQuery = projectQuery.trim().toLocaleLowerCase();
  const visibleProjects = normalizedProjectQuery === ""
    ? snapshot.projects
    : snapshot.projects.filter((project) => [project.name, project.slug, project.region, project.state, project.id]
      .some((value) => value.toLocaleLowerCase().includes(normalizedProjectQuery)));

  return (
    <section className="ow-workspace" aria-label={`${organization.name} projects`}>
      <WorkspaceToolbar
        canManage={canManage}
        memberCount={snapshot.members.length}
        organization={organization}
        projectCount={snapshot.projects.length}
        query={projectQuery}
        resultCount={visibleProjects.length}
        view="projects"
        onAction={() => setCreateProjectOpen(true)}
        onQuery={setProjectQuery}
      />
      {actionError === null ? null : <InlineError message={actionError} onDismiss={() => setActionError(null)} />}
      {visibleProjects.length === 0 ? (
        <div className="ow-card"><InlineState icon="database" title="No matching projects" detail="Try another name, slug, region, state, or project ID." /></div>
      ) : <div className="ow-project-grid">
        {visibleProjects.map((project) => (
          <ProjectCard
            active={project.id === configuration.projectId}
            key={project.id}
            pending={pending === `activate:${project.id}`}
            project={project}
            role={organization.role}
            onActivate={() => void activateProject(project)}
          />
        ))}
      </div>}
      {!canManage ? <p className="ow-permission-note"><Icon name="lock" size={14} />{roleLabel(organization.role)} access can open projects; an owner or administrator creates new projects.</p> : null}
      {createProjectOpen ? (
        <CreateProjectModal
          client={client}
          organization={organization}
          onActivate={activateProject}
          onClose={() => setCreateProjectOpen(false)}
          onCreated={() => { setCreateProjectOpen(false); refresh(); }}
          onSetupRequired={onSetupRequired}
        />
      ) : null}
    </section>
  );
}

function WorkspaceToolbar({ canManage, memberCount, onAction, onQuery, organization, projectCount, query, resultCount, view }: {
  readonly canManage: boolean;
  readonly memberCount: number;
  readonly organization: OrganizationSummary;
  readonly projectCount: number;
  readonly query: string;
  readonly resultCount: number;
  readonly view: "projects" | "members";
  onAction(): void;
  onQuery(value: string): void;
}) {
  const entity = view === "projects" ? "projects" : "members";
  return (
    <header className="ow-workspace-toolbar" aria-label={`${capitalize(view)} controls`}>
      <div className="ow-workspace-heading"><Icon name={view === "projects" ? "database" : "users"} size={16} /><span><strong>{capitalize(view)}</strong><small>{view === "projects" ? `${projectCount} in ${organization.name}` : `${memberCount} in ${organization.name}`}</small></span></div>
      <div className="ow-workspace-tools">
        <label className="ow-search ow-workspace-search"><span className="ow-sr-only">Search {entity}</span><Icon name={view === "projects" ? "database" : "users"} size={14} /><input value={query} onChange={(event) => onQuery(event.target.value)} placeholder={`Search ${entity}`} type="search" /></label>
        <span className="ow-toolbar-count">{resultCount}{query.trim() === "" ? "" : ` of ${view === "projects" ? projectCount : memberCount}`}</span>
        <span className="ow-workspace-context" title={`${organization.name} · ${roleLabel(organization.role)}`}><i>{initials(organization.name)}</i><span><strong>{organization.name}</strong><small>{roleLabel(organization.role)}</small></span></span>
        {canManage ? <button className="ow-button ow-button-primary ow-workspace-action" type="button" onClick={onAction}><Icon name="plus" size={14} />{view === "projects" ? "Create project" : "Invite member"}</button> : null}
      </div>
    </header>
  );
}

function ProjectCard({ active, onActivate, pending, project, role }: {
  readonly active: boolean;
  readonly pending: boolean;
  readonly project: ProjectSummary;
  readonly role: OrganizationRole;
  onActivate(): void;
}) {
  const activate = () => { if (!pending) onActivate(); };
  const handleKeyDown = (event: ReactKeyboardEvent<HTMLElement>) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    activate();
  };
  return (
    <article
      aria-current={active ? "true" : undefined}
      aria-disabled={pending}
      aria-label={active ? `Open ${project.name} overview` : `Use ${project.name} project`}
      className={active ? "ow-card ow-project-card is-active" : "ow-card ow-project-card"}
      role="button"
      tabIndex={0}
      onClick={activate}
      onKeyDown={handleKeyDown}
    >
      <header>
        <div className="ow-project-identity"><span className="ow-icon-well"><Icon name="database" size={18} /></span><div><h3>{project.name}</h3><p>{project.slug}</p></div></div>
        <StatusPill available label={capitalize(project.state)} positive={project.state === "active"} />
      </header>
      <div className="ow-project-facts">
        <ProjectFact label="Schema version" value={`v${project.schema_version}`} />
        <ProjectFact label="Region" value={project.region} />
        <ProjectFact label="Created" value={new Date(project.created_at_ms).toLocaleDateString()} />
        <ProjectFact label="Your access" value={roleLabel(role)} />
      </div>
      <footer>
        <div><span>Project ID</span><code title={project.id}>{project.id}</code></div>
        <span className={active ? "ow-project-cta is-active" : "ow-project-cta"}>{pending ? "Opening…" : active ? "Open overview" : "Use project"}<Icon name="chevronRight" size={14} /></span>
      </footer>
    </article>
  );
}

function ProjectFact({ label, value }: { readonly label: string; readonly value: string }) {
  return <div><span>{label}</span><strong>{value}</strong></div>;
}

function MembersWorkspace({
  canManage,
  children,
  client,
  currentUserId,
  error,
  members,
  organization,
  pending,
  projectCount,
  onClearError,
  onError,
  onInvite,
  onNotice,
  onPending,
  onRefresh,
}: {
  readonly canManage: boolean;
  readonly children: ReactNode;
  readonly client: FFDBClient;
  readonly currentUserId: string | null;
  readonly error: string | null;
  readonly members: readonly OrganizationMembershipSummary[];
  readonly organization: OrganizationSummary;
  readonly pending: string | null;
  readonly projectCount: number;
  onClearError(): void;
  onError(message: string): void;
  onInvite(): void;
  onNotice(message: string): void;
  onPending(value: string | null): void;
  onRefresh(): void;
}) {
  const [query, setQuery] = useState("");
  const filtered = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return normalized === "" ? members : members.filter((member) => member.email.toLowerCase().includes(normalized) || member.role.includes(normalized));
  }, [members, query]);

  const updateRole = async (member: OrganizationMembershipSummary, role: OrganizationRole) => {
    onPending(`role:${member.user_id}`);
    onClearError();
    try {
      await client.updateOrganizationMember(organization.id, member.user_id, { role });
      onNotice(`${member.email} is now ${roleLabel(role).toLowerCase()}.`);
      onRefresh();
    } catch (cause) {
      onError(errorMessage(cause));
    } finally {
      onPending(null);
    }
  };

  const remove = async (member: OrganizationMembershipSummary) => {
    if (!globalThis.confirm(`Remove ${member.email} from ${organization.name}? They will lose access to every project in this organization.`)) return;
    onPending(`remove:${member.user_id}`);
    onClearError();
    try {
      await client.removeOrganizationMember(organization.id, member.user_id);
      onNotice(`${member.email} was removed from ${organization.name}.`);
      onRefresh();
    } catch (cause) {
      onError(errorMessage(cause));
    } finally {
      onPending(null);
    }
  };

  return (
    <section className="ow-workspace" aria-label={`${organization.name} members`}>
      <WorkspaceToolbar
        canManage={canManage}
        memberCount={members.length}
        organization={organization}
        projectCount={projectCount}
        query={query}
        resultCount={filtered.length}
        view="members"
        onAction={onInvite}
        onQuery={setQuery}
      />
      {error === null ? null : <InlineError message={error} onDismiss={onClearError} />}
      <div className="ow-card ow-members-card">
        {members.length === 0 ? (
          <InlineState icon="users" title="No members yet" detail={canManage ? "Invite an administrator, developer, or viewer to collaborate." : "No organization members are visible."} />
        ) : filtered.length === 0 ? (
          <InlineState icon="users" title="No matching members" detail="Try a different email address or role." />
        ) : (
          <div className="ow-table-scroll portal-table-scroll" role="region" aria-label="Organization members" tabIndex={0}>
            <table className="ow-table ow-members-table">
              <thead><tr><th>Member</th><th>Role</th><th>Added</th><th>Access</th><th><span className="ow-sr-only">Actions</span></th></tr></thead>
              <tbody>{filtered.map((member) => {
                const immutableOwner = member.role === "owner";
                const isCurrentUser = member.user_id === currentUserId;
                const editable = canManage && !immutableOwner && !isCurrentUser;
                return (
                  <tr key={member.user_id}>
                    <td data-label="Member"><span className="ow-member"><i>{initials(member.email)}</i><span><strong>{member.email}</strong>{isCurrentUser ? <small>You</small> : null}</span></span></td>
                    <td data-label="Role">
                      {editable ? (
                        <select aria-label={`Role for ${member.email}`} disabled={pending !== null} value={member.role} onChange={(event) => void updateRole(member, event.target.value as OrganizationRole)}>
                          <option value="admin">Administrator</option><option value="developer">Developer</option><option value="viewer">Viewer</option>
                        </select>
                      ) : <span className="ow-role-pill">{roleLabel(member.role)}</span>}
                    </td>
                    <td data-label="Added">{new Date(member.created_at_ms).toLocaleDateString()}</td>
                    <td data-label="Access">All projects</td>
                    <td data-label="Actions">
                      {editable && !isCurrentUser ? <button className="ow-button ow-button-danger" disabled={pending !== null} type="button" onClick={() => void remove(member)}>{pending === `remove:${member.user_id}` ? "Removing…" : "Remove"}</button> : <span className="ow-muted-action">{immutableOwner ? "Owner" : isCurrentUser ? "Current account" : "View only"}</span>}
                    </td>
                  </tr>
                );
              })}</tbody>
            </table>
          </div>
        )}
      </div>
      {!canManage ? <p className="ow-permission-note"><Icon name="lock" size={14} />{roleLabel(organization.role)} access can view members; an owner or administrator manages membership.</p> : null}
      {children}
    </section>
  );
}

function CreateOrganizationModal({ client, configuration, onClose, onConfiguration, onCreated, onNotice, onSetupRequired }: {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onClose(): void;
  onConfiguration(value: PortalConfiguration): void;
  onCreated(): void;
  onNotice(message: string): void;
  onSetupRequired(): void;
}) {
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugEdited, setSlugEdited] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEscape(onClose);
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setPending(true); setError(null);
    try {
      const organization = await client.createOrganization({ name: name.trim(), slug: slug.trim() });
      persistPortalProject("", undefined, organization.name, organization.id, "Choose a project", configuration.apiUrl);
      onConfiguration({ ...configuration, organizationId: organization.id, organizationName: organization.name, projectId: "", projectName: "Choose a project", developerKey: undefined });
      onNotice(`${organization.name} was created.`);
      onCreated();
    } catch (cause) {
      if (isInstanceSetupRequired(cause)) onSetupRequired();
      else setError(errorMessage(cause));
    } finally { setPending(false); }
  };
  return <EntityFormModal description="Organizations own projects, members, usage, and billing." error={error} id="ow-create-organization" onClose={onClose} onSubmit={submit} pending={pending} submitLabel="Create organization" title="Create organization">
    <TextField label="Organization name" required value={name} onChange={(value) => { setName(value); if (!slugEdited) setSlug(slugify(value)); }} />
    <TextField label="Organization slug" required pattern="[a-z0-9]+(?:-[a-z0-9]+)*" hint="Lowercase letters, numbers, and hyphens." value={slug} onChange={(value) => { setSlugEdited(true); setSlug(slugify(value)); }} />
  </EntityFormModal>;
}

function CreateProjectModal({ client, onActivate, onClose, onCreated, onSetupRequired, organization }: {
  readonly client: FFDBClient;
  readonly organization: OrganizationSummary;
  onActivate(project: ProjectSummary): Promise<void>;
  onClose(): void;
  onCreated(): void;
  onSetupRequired(): void;
}) {
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugEdited, setSlugEdited] = useState(false);
  const [region, setRegion] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEscape(onClose);
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setPending(true); setError(null);
    try {
      const project = await client.createProject({ organization_id: organization.id, name: name.trim(), slug: slug.trim(), ...(region.trim() === "" ? {} : { region: region.trim() }) });
      onCreated();
      await onActivate(project);
    } catch (cause) {
      if (isInstanceSetupRequired(cause)) onSetupRequired();
      else setError(errorMessage(cause));
    } finally { setPending(false); }
  };
  return <EntityFormModal description={`Create an isolated data boundary inside ${organization.name}.`} error={error} id="ow-create-project" onClose={onClose} onSubmit={submit} pending={pending} submitLabel="Create and open project" title="Create project">
    <TextField label="Project name" required value={name} onChange={(value) => { setName(value); if (!slugEdited) setSlug(slugify(value)); }} />
    <TextField label="Project slug" required pattern="[a-z0-9]+(?:-[a-z0-9]+)*" hint="Used in project URLs and commands." value={slug} onChange={(value) => { setSlugEdited(true); setSlug(slugify(value)); }} />
    <TextField label="Region" value={region} placeholder="Use the instance default" hint="Leave blank unless this instance offers multiple regions." onChange={setRegion} />
  </EntityFormModal>;
}

function InviteMemberModal({ client, onClose, onInvited, onNotice, organization }: {
  readonly client: FFDBClient;
  readonly organization: OrganizationSummary;
  onClose(): void;
  onInvited(): void;
  onNotice(message: string): void;
}) {
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<"admin" | "developer" | "viewer">("developer");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEscape(onClose);
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setPending(true); setError(null);
    try {
      await client.createOrganizationInvitation(organization.id, { email: email.trim(), role });
      onNotice(`Invitation sent to ${email.trim()}.`);
      onInvited();
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setPending(false); }
  };
  return <EntityFormModal description={`The invitation grants access to every project in ${organization.name}.`} error={error} id="ow-invite-member" onClose={onClose} onSubmit={submit} pending={pending} submitLabel="Send invitation" title="Invite member">
    <TextField label="Email address" required type="email" value={email} onChange={setEmail} />
    <div className="ow-field"><label htmlFor="ow-invitation-role">Organization role</label><select id="ow-invitation-role" value={role} onChange={(event) => setRole(event.target.value as typeof role)}><option value="admin">Administrator — manage projects and members</option><option value="developer">Developer — build across projects</option><option value="viewer">Viewer — read-only portal access</option></select><small>Ownership cannot be transferred through an invitation.</small></div>
  </EntityFormModal>;
}

function EntityFormModal({ children, description, error, id, onClose, onSubmit, pending, submitLabel, title }: {
  readonly children: ReactNode;
  readonly description: string;
  readonly error: string | null;
  readonly id: string;
  readonly pending: boolean;
  readonly submitLabel: string;
  readonly title: string;
  onClose(): void;
  onSubmit(event: FormEvent): void;
}) {
  return <Modal ariaLabelledBy={`${id}-title`} onClose={onClose}><form className="ow-entity-form" onSubmit={onSubmit}>
    <div className="ow-modal-heading"><span className="ow-icon-well"><Icon name={title.includes("member") ? "users" : "database"} size={18} /></span><div><h2 id={`${id}-title`}>{title}</h2><p>{description}</p></div><button className="ow-icon-button" aria-label={`Close ${title.toLowerCase()}`} type="button" onClick={onClose}>×</button></div>
    <div className="ow-form-fields">{children}</div>
    {error === null ? null : <InlineError message={error} />}
    <div className="ow-modal-actions"><button className="ow-button" disabled={pending} type="button" onClick={onClose}>Cancel</button><button className="ow-button ow-button-primary" disabled={pending} type="submit">{pending ? "Working…" : submitLabel}</button></div>
  </form></Modal>;
}

function TextField({ hint, label, onChange, pattern, placeholder, required = false, type = "text", value }: {
  readonly hint?: string;
  readonly label: string;
  readonly pattern?: string;
  readonly placeholder?: string;
  readonly required?: boolean;
  readonly type?: string;
  readonly value: string;
  onChange(value: string): void;
}) {
  const id = `ow-field-${slugify(label)}`;
  return <div className="ow-field"><label htmlFor={id}>{label}</label><input id={id} pattern={pattern} placeholder={placeholder} required={required} type={type} value={value} onChange={(event) => onChange(event.target.value)} />{hint === undefined ? null : <small>{hint}</small>}</div>;
}

function Modal({ ariaLabelledBy, children, onClose }: { readonly ariaLabelledBy: string; readonly children: ReactNode; onClose(): void }) {
  return <div className="ow-modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.currentTarget === event.target) onClose(); }}><div aria-labelledby={ariaLabelledBy} aria-modal="true" className="ow-modal" role="dialog">{children}</div></div>;
}

function WorkspaceEmpty({ actionLabel, children, detail, icon, onAction, title }: {
  readonly actionLabel?: string | undefined;
  readonly children: ReactNode;
  readonly detail: string;
  readonly icon: IconName;
  readonly title: string;
  onAction?: (() => void) | undefined;
}) {
  return <section className="ow-card ow-workspace-empty"><span className="ow-empty-icon"><Icon name={icon} size={25} /></span><span className="ow-eyebrow">Workspace setup</span><h2>{title}</h2><p>{detail}</p>{actionLabel === undefined || onAction === undefined ? null : <button className="ow-button ow-button-primary" type="button" onClick={onAction}><Icon name="plus" size={15} />{actionLabel}</button>}{children}</section>;
}

function WorkspaceSkeleton({ view }: { readonly view: "projects" | "members" }) {
  return <div className="ow-root" aria-busy="true" aria-label={`Loading ${view}`}><div className="ow-skeleton ow-skeleton-heading" /><div className={view === "projects" ? "ow-project-grid" : ""}>{Array.from({ length: view === "projects" ? 3 : 1 }, (_, index) => <div className="ow-skeleton ow-skeleton-project" key={index} />)}</div></div>;
}

function InlineState({ detail, icon, title }: { readonly detail: string; readonly icon: IconName; readonly title: string }) {
  return <div className="ow-inline-state"><span className="ow-icon-well"><Icon name={icon} size={18} /></span><strong>{title}</strong><p>{detail}</p></div>;
}

function InlineError({ message, onDismiss }: { readonly message: string; onDismiss?: (() => void) | undefined }) {
  return <div className="ow-inline-error" role="alert"><Icon name="shield" size={17} /><div><strong>Request failed</strong><span>{message}</span></div>{onDismiss === undefined ? null : <button type="button" onClick={onDismiss}>Dismiss</button>}</div>;
}

function PanelError({ detail, onRetry, title }: { readonly detail: string; readonly title: string; onRetry(): void }) {
  return <section className="ow-card ow-panel-error" role="alert"><span className="ow-empty-icon"><Icon name="shield" size={24} /></span><h2>{title}</h2><p>{detail}</p><button className="ow-button" type="button" onClick={onRetry}><Icon name="sync" size={15} />Try again</button></section>;
}

function useEscape(onEscape: () => void) {
  useEffect(() => {
    const listener = (event: KeyboardEvent) => { if (event.key === "Escape") onEscape(); };
    globalThis.addEventListener("keydown", listener);
    return () => globalThis.removeEventListener("keydown", listener);
  }, [onEscape]);
}

function compactNumber(value: number): string { return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(value); }
function formatLatency(value: number): string { return value >= 1_000 ? `${(value / 1_000).toFixed(2)} s` : `${value < 10 ? value.toFixed(1) : Math.round(value)} ms`; }
function formatQps(value: number): string { return value < 0.1 ? value.toFixed(2) : value < 10 ? value.toFixed(1) : compactNumber(value); }
function formatPercent(value: number): string { return `${(value * 100).toFixed(value >= 0.1 ? 1 : 2)}%`; }
function overviewLinePath(points: readonly (readonly [number, number])[]): string { return points.map(([x, y], index) => `${index === 0 ? "M" : "L"} ${x.toFixed(2)} ${y.toFixed(2)}`).join(" "); }
function isObservabilitySummary(value: unknown): value is ObservabilitySummary {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as { readonly totals?: { readonly requests?: unknown } };
  return typeof candidate.totals?.requests === "number";
}
function formatNumber(value: number): string { return new Intl.NumberFormat().format(value); }
function formatDateTime(value: number): string { return new Date(value).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }); }
function capitalize(value: string): string { return `${value[0]?.toUpperCase() ?? ""}${value.slice(1).replaceAll("_", " ")}`; }
function sentenceList(values: readonly string[]): string { return new Intl.ListFormat(undefined, { style: "long", type: "conjunction" }).format(values); }
function slugify(value: string): string { return value.trim().toLowerCase().replace(/[^a-z0-9]+/gu, "-").replace(/^-|-$/gu, ""); }
function initials(value: string): string { const parts = value.split(/[\s:@._-]+/u).filter(Boolean); return parts.slice(0, 2).map((part) => part[0]?.toUpperCase() ?? "").join("") || "FF"; }
function friendlyActor(value: string): string { if (value === "system") return "System"; return value.length > 32 ? `${value.slice(0, 29)}…` : value; }
function friendlyAction(value: string): string { return value.split(/[._]/u).filter(Boolean).slice(-2).map(capitalize).join(" "); }
function roleLabel(role: OrganizationRole): string { return role === "admin" ? "Administrator" : capitalize(role); }
function resourceIcon(resource: string): IconName { const value = resource.toLowerCase(); if (value.includes("storage")) return "archive"; if (value.includes("auth") || value.includes("user")) return "users"; if (value.includes("policy")) return "shield"; if (value.includes("email")) return "mail"; if (value.includes("backup")) return "backup"; return "code"; }
function errorMessage(cause: unknown): string { if (cause instanceof Error) return cause.message; if (typeof cause === "string") return cause; if (cause !== null && typeof cause === "object" && "message" in cause && typeof cause.message === "string") return cause.message; return "The request could not be completed."; }
function isInstanceSetupRequired(cause: unknown): boolean { return cause !== null && typeof cause === "object" && "code" in cause && cause.code === "instance.setup_required"; }

export { PolishedOverviewPanel as OverviewPanel, PolishedWorkspacePanel as WorkspacePanel };

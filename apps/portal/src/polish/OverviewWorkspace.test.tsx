import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  type AuditLogEntry,
  type FFDBClient,
  type OrganizationSummary,
  type ObservabilitySummary,
  type ProjectSummary,
} from "@ffdb/client";

import type { PortalConfiguration } from "../config.js";
import { PolishedOverviewPanel, PolishedWorkspacePanel } from "./OverviewWorkspace.js";

const organization: OrganizationSummary = {
  id: "org-1",
  name: "Northstar Labs",
  slug: "northstar-labs",
  role: "owner",
  created_at_ms: 1_700_000_000_000,
};

const project: ProjectSummary = {
  id: "project-1",
  organization_id: organization.id,
  name: "Atlas",
  slug: "atlas",
  region: "local",
  state: "active",
  schema_version: 4,
  created_at_ms: 1_700_000_000_000,
};

const configuration: PortalConfiguration = {
  apiUrl: "https://ffdb.example.test",
  organizationId: organization.id,
  organizationName: organization.name,
  projectId: project.id,
  projectName: project.name,
  developerKey: "ffdb_dev_test.secret",
};

function overviewClient(logs: readonly AuditLogEntry[]): FFDBClient {
  return {
    health: vi.fn().mockResolvedValue({ status: "ok" }),
    readiness: vi.fn().mockResolvedValue({ status: "ready" }),
    projectObservability: vi.fn().mockResolvedValue(observabilitySummary()),
    schema: vi.fn().mockResolvedValue({ version: 4, tables: [{ name: "documents", sql: "CREATE TABLE documents(id TEXT)", rls_enabled: true, rls_forced: true }] }),
    policies: vi.fn().mockResolvedValue([{ name: "documents_read", table: "documents", kind: "permissive", command: "select", roles: ["authenticated"], using_expression: "owner_id = auth.uid()", check_expression: null, enabled: true, forced: true }]),
    logs: vi.fn().mockResolvedValue(logs),
    backups: vi.fn().mockResolvedValue([]),
    storage: { buckets: vi.fn().mockResolvedValue([]) },
    organizations: vi.fn().mockResolvedValue([organization]),
  } as unknown as FFDBClient;
}

function workspaceClient(overrides: Partial<Record<string, unknown>> = {}): FFDBClient {
  return {
    organizations: vi.fn().mockResolvedValue([organization]),
    projects: vi.fn().mockResolvedValue([project]),
    organizationMembers: vi.fn().mockResolvedValue([{ organization_id: organization.id, user_id: "owner-1", email: "owner@example.test", role: "owner", created_at_ms: 1_700_000_000_000 }]),
    developerSession: vi.fn().mockResolvedValue({ session_token: "session", user_id: "owner-1", email: "owner@example.test", expires_at_ms: Date.now() + 60_000 }),
    createProject: vi.fn().mockResolvedValue({ ...project, id: "project-2", name: "Notes", slug: "notes" }),
    createOrganizationInvitation: vi.fn().mockResolvedValue(undefined),
    updateOrganizationMember: vi.fn(),
    removeOrganizationMember: vi.fn(),
    setProjectId: vi.fn(),
    setDeveloperKey: vi.fn(),
    createApiKey: vi.fn().mockResolvedValue({ secret: "ffdb_dev_created.secret" }),
    ...overrides,
  } as unknown as FFDBClient;
}

describe("polished overview and workspace panels", () => {
  beforeEach(() => {
    globalThis.sessionStorage.clear();
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("opens useful audit details and sends quick actions to their real destinations", async () => {
    const event: AuditLogEntry = {
      id: "event-1",
      occurred_at_ms: 1_700_000_000_000,
      actor: "owner@example.test",
      action: "database.query",
      resource: "database/documents",
      outcome: "success",
      request_id: "request-1",
    };
    const navigate = vi.fn();
    render(<PolishedOverviewPanel client={overviewClient([event])} configuration={configuration} onNavigate={navigate} />);

    expect(await screen.findByRole("heading", { name: "Project resources" })).toBeInTheDocument();
    const projectWorkspace = screen.getByRole("region", { name: "Project workspace" });
    expect(within(projectWorkspace).getByRole("heading", { name: "Quick actions" })).toBeInTheDocument();
    expect(within(projectWorkspace).getByRole("button", { name: /Run a SQL query/i })).toBeInTheDocument();
    expect(screen.getByText("25 ms")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Traffic and performance" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Open observability/i }));
    expect(navigate).toHaveBeenCalledWith("Observability");

    fireEvent.click(screen.getByRole("button", { name: "View details for Database Query" }));
    const dialog = screen.getByRole("dialog", { name: "Database Query" });
    expect(within(dialog).getByText("request-1")).toBeInTheDocument();
    expect(within(dialog).getByText("event-1")).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    fireEvent.click(screen.getByRole("button", { name: /Create a migration/i }));
    expect(navigate).toHaveBeenCalledWith("Migrations");
    fireEvent.click(screen.getByRole("button", { name: /Invite a member/i }));
    expect(navigate).toHaveBeenCalledWith("Members");
  });

  it("creates a project through the API and activates the returned project", async () => {
    const client = workspaceClient();
    const navigate = vi.fn();
    const onConfiguration = vi.fn();
    render(<PolishedWorkspacePanel view="projects" client={client} configuration={configuration} onConfiguration={onConfiguration} onNavigate={navigate} onNotice={vi.fn()} onSetupRequired={vi.fn()} />);

    expect(await screen.findByRole("region", { name: "Northstar Labs projects" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Projects" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    const dialog = screen.getByRole("dialog", { name: "Create project" });
    fireEvent.change(within(dialog).getByLabelText("Project name"), { target: { value: "Notes" } });
    expect(within(dialog).getByLabelText("Project slug")).toHaveValue("notes");
    fireEvent.click(within(dialog).getByRole("button", { name: "Create and open project" }));

    await waitFor(() => expect(client.createProject).toHaveBeenCalledWith({ organization_id: organization.id, name: "Notes", slug: "notes" }));
    await waitFor(() => expect(onConfiguration).toHaveBeenCalledWith(expect.objectContaining({ projectId: "project-2", projectName: "Notes" })));
    expect(navigate).toHaveBeenCalledWith("Overview");
  });

  it("sends a real organization invitation from the Members page", async () => {
    const client = workspaceClient();
    render(<PolishedWorkspacePanel view="members" client={client} configuration={configuration} onConfiguration={vi.fn()} onNavigate={vi.fn()} onNotice={vi.fn()} onSetupRequired={vi.fn()} />);

    expect(await screen.findByRole("region", { name: "Northstar Labs members" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Members" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Invite member" }));
    const dialog = screen.getByRole("dialog", { name: "Invite member" });
    fireEvent.change(within(dialog).getByLabelText("Email address"), { target: { value: "developer@example.test" } });
    fireEvent.change(within(dialog).getByLabelText("Organization role"), { target: { value: "developer" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Send invitation" }));

    await waitFor(() => expect(client.createOrganizationInvitation).toHaveBeenCalledWith(organization.id, { email: "developer@example.test", role: "developer" }));
  });

  it("keeps organization mutation controls out of a viewer workspace", async () => {
    const viewerOrganization = { ...organization, role: "viewer" as const };
    const client = workspaceClient({ organizations: vi.fn().mockResolvedValue([viewerOrganization]) });
    const { unmount } = render(<PolishedWorkspacePanel view="projects" client={client} configuration={configuration} onConfiguration={vi.fn()} onNavigate={vi.fn()} onNotice={vi.fn()} onSetupRequired={vi.fn()} />);

    expect(await screen.findByRole("region", { name: "Northstar Labs projects" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Create project" })).not.toBeInTheDocument();
    expect(screen.getByText(/Viewer access can open projects/i)).toBeInTheDocument();

    unmount();
    render(<PolishedWorkspacePanel view="members" client={client} configuration={configuration} onConfiguration={vi.fn()} onNavigate={vi.fn()} onNotice={vi.fn()} onSetupRequired={vi.fn()} />);
    expect(await screen.findByRole("region", { name: "Northstar Labs members" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Invite member" })).not.toBeInTheDocument();
    expect(screen.getByText(/Viewer access can view members/i)).toBeInTheDocument();
  });

  it("does not duplicate sidebar routes and makes each project card a keyboard-selectable target", async () => {
    const secondProject: ProjectSummary = { ...project, id: "project-2", name: "Beacon", slug: "beacon", region: "iad" };
    const client = workspaceClient({ projects: vi.fn().mockResolvedValue([project, secondProject]) });
    const onConfiguration = vi.fn();
    render(<PolishedWorkspacePanel view="projects" client={client} configuration={configuration} onConfiguration={onConfiguration} onNavigate={vi.fn()} onNotice={vi.fn()} onSetupRequired={vi.fn()} />);

    const workspace = await screen.findByRole("region", { name: "Northstar Labs projects" });
    expect(within(workspace).queryByRole("tablist")).not.toBeInTheDocument();
    expect(within(workspace).getByText("2 in Northstar Labs")).toBeInTheDocument();

    const beacon = within(workspace).getByRole("button", { name: "Use Beacon project" });
    fireEvent.keyDown(beacon, { key: "Enter" });
    await waitFor(() => expect(onConfiguration).toHaveBeenCalledWith(expect.objectContaining({ projectId: "project-2", projectName: "Beacon" })));

    fireEvent.change(within(workspace).getByPlaceholderText("Search projects"), { target: { value: "atlas" } });
    expect(within(workspace).queryByRole("button", { name: "Use Beacon project" })).not.toBeInTheDocument();
    expect(within(workspace).getByRole("button", { name: "Open Atlas overview" })).toHaveAttribute("aria-current", "true");
  });
});

function observabilitySummary(): ObservabilitySummary {
  return {
    scope: "project", project_id: project.id, generated_at_ms: Date.now(), window_start_ms: Date.now() - 3_600_000, window_end_ms: Date.now(), resolution_seconds: 60, retention_days: 30, current_inflight: 1, dropped_samples: 0,
    totals: { requests: 16, qps: 16 / 3_600, client_errors: 0, server_errors: 0, error_rate: 0, average_latency_ms: 12, p50_latency_ms: 10, p95_latency_ms: 25, p99_latency_ms: 50, max_latency_ms: 44 },
    series: [], busiest_routes: [], slowest_routes: [], frequent_queries: [], slow_queries: [],
    runtime: { healthy: true, active_workers: 1, max_workers: 8, worker_saturation: 0.125, execution_slots_in_use: 0, queue_capacity: 64, queue_saturation: 0 },
    storage: { logical_database_bytes: 4096, sampled_projects: 1, database_disk_total_bytes: null, database_disk_available_bytes: null, database_disk_used_percent: null, backup_disk_total_bytes: null, backup_disk_available_bytes: null, backup_disk_used_percent: null, last_sample_at_ms: null },
  };
}

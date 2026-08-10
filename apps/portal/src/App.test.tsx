import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { FFDBClient, MemoryDeveloperSessionStore, MemorySessionStore, type InstanceStatus } from "@ffdb/client";

import { App } from "./App.js";
import type { PortalConfiguration } from "./config.js";

const configuration: PortalConfiguration = {
  apiUrl: "https://ffdb.example.test",
  organizationId: "org-1",
  projectId: "project-1",
  developerKey: "ffdb_dev_test.secret",
  projectName: "Atlas",
  organizationName: "Northstar Labs",
};

function administratorStatus(role: "owner" | "admin" = "admin"): InstanceStatus {
  return {
    owner_user_id: "owner-1",
    current_user_role: role,
    deployment_mode: "team",
    organization_creation_policy: "authenticated",
    billing_enforcement_enabled: false,
    setup_completed_at_ms: 1,
    billing_account: null,
    administrator_count: 2,
    created_at_ms: 1,
    updated_at_ms: 1,
  };
}

describe("portal backend integration", () => {
  beforeEach(() => {
    globalThis.history.replaceState({}, "", "/");
    globalThis.localStorage.clear();
    globalThis.sessionStorage.clear();
  });
  afterEach(() => {
    cleanup();
    globalThis.sessionStorage.clear();
  });
  it("loads live overview data and executes SQL through @ffdb/client", async () => {
    const calls: Request[] = [];
    const fetchMock: typeof fetch = async (input, init) => {
      const request = new Request(input, init);
      calls.push(request);
      if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
      if (request.url.endsWith("/healthz")) return Response.json({ status: "ok", version: 1 });
      if (request.url.endsWith("/readyz")) return Response.json({ status: "ready" });
      if (request.url.endsWith("/metrics")) return new Response("ffdb_http_requests_total{method=\"GET\",route=\"/healthz\",status_class=\"2xx\"} 12\nffdb_http_requests_inflight 1\n");
      if (request.url.endsWith("/schema")) return Response.json({ version: 4, tables: [{ name: "documents", sql: "CREATE TABLE documents(id TEXT)", rls_enabled: true, rls_forced: true }] });
      if (request.url.endsWith("/policies")) return Response.json([{ name: "documents_read", table: "documents", kind: "permissive", command: "select", roles: ["authenticated"], using_expression: "owner_id = auth.uid()", check_expression: null, enabled: true, forced: true }]);
      if (request.url.endsWith("/query")) return Response.json({ columns: [{ name: "version", type: "text" }], rows: [["3.49.0"]], affected_rows: 0, last_insert_rowid: null, truncated: false });
      return Response.json({ error: { code: "route.missing", message: "missing", request_id: "request-1" } }, { status: 404 });
    };
    const client = new FFDBClient({ baseUrl: configuration.apiUrl, projectId: configuration.projectId, developerKey: "ffdb_dev_test.secret", developerSessionStore: await signedInDeveloperStore("portal-overview-test"), fetch: fetchMock });
    render(<App client={client} configuration={configuration} />);

    const summary = await screen.findByLabelText("Project status summary");
    const databaseCard = within(summary).getByText("Database tables").closest("article");
    expect(databaseCard).not.toBeNull();
    expect(within(databaseCard as HTMLElement).getByText("1")).toBeInTheDocument();
    expect(within(databaseCard as HTMLElement).getByText("Schema version 4")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /View all activity/i }));
    expect(screen.getByRole("button", { name: "Activity" })).toHaveAttribute("aria-current", "page");
    fireEvent.click(screen.getByRole("button", { name: "SQL Editor" }));
    fireEvent.click(screen.getByRole("button", { name: /Run query/i }));
    expect(await screen.findByText("3.49.0")).toBeInTheDocument();

    const query = calls.find((request) => request.url.endsWith("/query"));
    expect(query?.method).toBe("POST");
    expect(query?.headers.get("authorization")).toBe("Bearer ffdb_dev_test.secret");
    await expect(query?.json()).resolves.toMatchObject({ sql: "SELECT sqlite_version() AS version" });
  });

  it("restores project access from the signed-in account when this browser has no project key", async () => {
    const calls: Request[] = [];
    const sessionStore = await signedInDeveloperStore("portal-project-session-restore-test");
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerSessionStore: sessionStore,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/v1/instance")) return Response.json(administratorStatus("owner"));
        if (request.url.endsWith("/v1/organizations")) return Response.json([{ id: "org-1", name: "Northstar Labs", slug: "northstar", role: "owner", created_at_ms: 1 }]);
        if (request.url.endsWith("/v1/organizations/org-1/projects")) return Response.json([{ id: "project-1", organization_id: "org-1", name: "Atlas", slug: "atlas", region: "local", state: "active", schema_version: 1, created_at_ms: 1 }]);
        if (request.url.endsWith("/api-keys") && request.method === "POST") return Response.json({
          id: "portal-key-1",
          name: "portal-session",
          prefix: "ffdb_dev_portal",
          secret: "ffdb_dev_portal.session-secret",
          scopes: ["database_query", "database_schema"],
          expires_at_ms: Date.now() + 60_000,
          created_at_ms: Date.now(),
        }, { status: 201 });
        if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
        if (request.url.endsWith("/policies")) return Response.json([]);
        if (request.url.endsWith("/logs")) return Response.json([]);
        if (request.url.endsWith("/backups")) return Response.json([]);
        if (request.url.endsWith("/storage/buckets")) return Response.json([]);
        return Response.json({ status: "ok", version: 1 });
      },
    });
    const withoutProjectKey = { ...configuration, developerKey: undefined };

    render(<App client={client} configuration={withoutProjectKey} />);

    await screen.findByLabelText("Project status summary");
    const issuance = calls.find((request) => request.url.endsWith("/api-keys") && request.method === "POST");
    expect(issuance?.headers.get("authorization")).toBe("Bearer platform-session");
    await expect(issuance?.json()).resolves.toMatchObject({
      name: "portal-session",
      expires_at_ms: expect.any(Number),
    });
    expect(calls.find((request) => request.url.endsWith("/schema"))?.headers.get("authorization"))
      .toBe("Bearer ffdb_dev_portal.session-secret");
    expect(globalThis.sessionStorage.getItem("ffdb.portal.instance.https%3A%2F%2Fffdb.example.test.project-key.project-1"))
      .toBe("ffdb_dev_portal.session-secret");
  });

  it("labels unavailable operational data instead of inventing health or activity", async () => {
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerKey: "ffdb_dev_test.secret",
      developerSessionStore: await signedInDeveloperStore("portal-unavailable-data-test"),
      fetch: async (input, init) => {
        const request = new Request(input, init);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/healthz")) return Response.json({ status: "ok", version: 1 });
        if (request.url.endsWith("/readyz")) return Response.json({ status: "ready" });
        if (request.url.endsWith("/metrics")) return Response.json({ error: { code: "metrics.unavailable", message: "unavailable", request_id: "metrics-1" } }, { status: 503 });
        if (request.url.endsWith("/schema")) return Response.json({ version: 4, tables: [] });
        if (request.url.endsWith("/policies")) return Response.json([]);
        return Response.json({ error: { code: "route.unavailable", message: "unavailable", request_id: "optional-1" } }, { status: 503 });
      },
    });

    render(<App client={client} configuration={configuration} />);

    const signals = await screen.findByLabelText("Project status summary");
    const healthCard = within(signals).getByText("API health").closest("article");
    expect(healthCard).not.toBeNull();
    expect(within(healthCard as HTMLElement).getByText("ok")).toBeInTheDocument();
    expect(screen.getByText("Readiness: ready")).toBeInTheDocument();
    expect(within(signals).getAllByText("Unavailable").length).toBeGreaterThan(0);
    expect(screen.getByText("Some project data is unavailable")).toBeInTheDocument();
    expect(screen.getByText("Activity is unavailable")).toBeInTheDocument();
    expect(screen.getByText("Audit logs could not be read with the active credential.")).toBeInTheDocument();
    expect(screen.queryByText("Operational")).not.toBeInTheDocument();
    expect(screen.queryByText("Healthy")).not.toBeInTheDocument();
    expect(screen.queryByText("ffdb-local-01")).not.toBeInTheDocument();
    expect(screen.queryByRole("img", { name: /activity trend/i })).not.toBeInTheDocument();
  });

  it("hides and blocks instance administration for a non-administrator session", async () => {
    globalThis.history.replaceState({}, "", "/app/instance");
    const calls: Request[] = [];
    const developerSessionStore = new MemoryDeveloperSessionStore("portal-developer-role-test");
    await developerSessionStore.set({ session_token: "developer-session", user_id: "developer-1", email: "developer@example.test", expires_at_ms: 99_999 });
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerKey: "ffdb_dev_test.secret",
      developerSessionStore,
      fetch: async (input, init) => {
        const request = new Request(input, init); calls.push(request);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/v1/instance")) return Response.json({ error: { code: "instance.forbidden", message: "administrator access required", request_id: "instance-role-1" } }, { status: 403 });
        if (request.url.endsWith("/v1/organizations")) return Response.json([]);
        return Response.json([]);
      },
    });

    render(<App client={client} configuration={configuration} />);

    expect(await screen.findByRole("heading", { name: "Instance administration unavailable" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Instance" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Billing" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Users" })).not.toBeInTheDocument();
    expect(calls.some((request) => request.url.endsWith("/v1/instance/organizations"))).toBe(false);
  });

  it("shows instance administration only after the API confirms an administrator role", async () => {
    const developerSessionStore = new MemoryDeveloperSessionStore("portal-administrator-role-test");
    await developerSessionStore.set({ session_token: "administrator-session", user_id: "admin-1", email: "admin@example.test", expires_at_ms: 99_999 });
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerKey: "ffdb_dev_test.secret",
      developerSessionStore,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/v1/instance")) return Response.json(administratorStatus());
        if (request.url.endsWith("/v1/organizations")) return Response.json([]);
        if (request.url.endsWith("/healthz")) return Response.json({ status: "ok" });
        if (request.url.endsWith("/readyz")) return Response.json({ status: "ready" });
        if (request.url.endsWith("/metrics")) return new Response("ffdb_http_requests_total 1\nffdb_http_requests_inflight 0\n");
        if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
        if (request.url.endsWith("/policies")) return Response.json([]);
        return Response.json([]);
      },
    });

    render(<App client={client} configuration={configuration} />);

    expect(await screen.findByRole("button", { name: "Instance" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Billing" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Users" })).toBeInTheDocument();
    expect(screen.getAllByText("Self-hosted admin · Team mode").length).toBeGreaterThan(0);
  });

  it("surfaces an available host release to administrators without running a check mutation", async () => {
    const calls: Request[] = [];
    const developerSessionStore = await signedInDeveloperStore("portal-update-badge-test");
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerKey: "ffdb_dev_test.secret",
      developerSessionStore,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/v1/instance/updates")) return Response.json({
          supported: true,
          unavailable_reason: null,
          capabilities: { check: true, install: true, rollback: true, automatic_checks: true, automatic_apply: true },
          state_schema: 1,
          minimum_rollback_version: "0.3.2",
          signature_identity: "release-workflow",
          installed_version: "0.3.2",
          available_version: "0.3.3",
          update_available: true,
          last_check_at_ms: Date.now(),
          active_job: null,
          releases: [],
          settings: { channel: "stable", automatic_checks: true, check_interval_hours: 24, automatic_apply: false, maintenance_window_start: null, maintenance_window_duration_minutes: 60 },
        });
        if (request.url.endsWith("/v1/instance")) return Response.json(administratorStatus("owner"));
        if (request.url.endsWith("/v1/organizations")) return Response.json([]);
        if (request.url.endsWith("/healthz")) return Response.json({ status: "ok" });
        if (request.url.endsWith("/readyz")) return Response.json({ status: "ready" });
        if (request.url.endsWith("/metrics")) return new Response("ffdb_http_requests_total 1\n");
        if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
        if (request.url.endsWith("/policies")) return Response.json([]);
        return Response.json([]);
      },
    });

    render(<App client={client} configuration={configuration} />);

    expect(await screen.findByLabelText("Update available")).toHaveTextContent("New");
    expect(calls.some((request) => request.url.endsWith("/v1/instance/updates/check"))).toBe(false);
  });

  it("renders a safe degraded state when a scoped route is unavailable", async () => {
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerKey: "ffdb_dev_test.secret",
      developerSessionStore: await signedInDeveloperStore("portal-degraded-route-test"),
      fetch: async (input, init) => {
        const request = new Request(input, init);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.includes("/observability?range=1h")) return Response.json({ totals: { requests: 1, p95_latency_ms: 5 } });
        return Response.json({ error: { code: "route.not_installed", message: "Route unavailable", request_id: "request-2" } }, { status: 404 });
      },
    });
    render(<App client={client} configuration={configuration} />);
    expect(await screen.findByText("Some project data is unavailable")).toBeInTheDocument();
    expect(screen.getByText(/API health, readiness, database schema/i)).toBeInTheDocument();
    expect(screen.getByText("Activity is unavailable")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("May 24, 2025")).not.toBeInTheDocument());
  });

  it("keeps FFDB billing separate from project commerce capabilities", async () => {
    const calls: Request[] = [];
    const developerSessionStore = new MemoryDeveloperSessionStore("portal-billing-test");
    await developerSessionStore.set({ session_token: "platform-session", user_id: "user-1", email: "owner@example.test", expires_at_ms: 99_999 });
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerKey: "ffdb_dev_test.secret",
      developerSessionStore,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/v1/organizations")) return Response.json([{ id: "org-1", name: "Northstar Labs", slug: "northstar", role: "owner", created_at_ms: 1 }]);
        if (request.url.endsWith("/v1/organizations/org-1/billing")) return Response.json({
          organization_id: "org-1",
          tier: "free",
          status: "free",
          billing_unit: "organization",
          seat_quantity: 1,
          project_limit: null,
          usage_allowance: { storage_bytes: 1_000_000_000, monthly_reads: 1_000_000, monthly_writes: 50_000, monthly_active_users: 5_000, overage_enabled: false },
          current_period_end_ms: null,
          cancel_at_period_end: false,
          provider_configured: false,
          billing_enforcement_enabled: false,
          billing_exempt: false,
        });
        if (request.url.endsWith("/v1/organizations/org-1/billing/usage")) return Response.json({
          organization_id: "org-1",
          period_start_ms: 1,
          period_end_ms: 2,
          reads: 12,
          writes: 3,
          storage_bytes: 4096,
          storage_byte_hours: 2048,
          monthly_active_users: 1,
          reporting_status: "healthy",
          reporting_last_success_ms: null,
          as_of_ms: 1,
        });
        if (request.url.endsWith("/v1/organizations/org-1/billing/invoices")) return Response.json([]);
        if (request.url.endsWith("/v1/projects/project-1/commerce/account")) return Response.json(null);
        if (/\/v1\/projects\/project-1\/commerce\/(?:products|prices|orders|payments|subscriptions)(?:\?.*)?$/u.test(request.url)) return Response.json([]);
        if (request.url.endsWith("/healthz")) return Response.json({ status: "ok" });
        if (request.url.endsWith("/readyz")) return Response.json({ status: "ready" });
        if (request.url.endsWith("/metrics")) return new Response("ffdb_http_requests_total 1\n");
        if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
        if (request.url.endsWith("/policies")) return Response.json([]);
        return Response.json([]);
      },
    });

    render(<App client={client} configuration={configuration} />);
    fireEvent.click(await screen.findByRole("button", { name: "Usage" }));
    expect((await screen.findAllByText("Northstar Labs")).length).toBeGreaterThan(0);
    expect(screen.getByText("No billing limits apply.")).toBeInTheDocument();
    expect(screen.getByText("4.1 KB")).toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Plan & payment" })).not.toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Products" }));
    expect(await screen.findByRole("heading", { name: "Products" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Provider" }));
    expect(await screen.findByRole("heading", { name: "Payment provider" })).toBeInTheDocument();
    expect(screen.getByText(/neither option affects FFDB platform billing/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Orders" }));
    expect(await screen.findByRole("heading", { name: "Orders and fulfillment" })).toBeInTheDocument();
    expect(calls.some((request) => request.url.endsWith("/v1/organizations/org-1/billing"))).toBe(true);
    expect(calls.some((request) => request.url.endsWith("/v1/projects/project-1/commerce/account"))).toBe(true);
  });

  it("signs in an end user before sync and links to the served documentation", async () => {
    const calls: Request[] = [];
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      ...(configuration.developerKey === undefined ? {} : { developerKey: configuration.developerKey }),
      sessionStore: new MemorySessionStore("portal-end-user-test"),
      developerSessionStore: await signedInDeveloperStore("portal-end-user-developer-test"),
      fetch: async (input, init) => {
        const request = new Request(input, init); calls.push(request);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/v1/organizations")) return Response.json([]);
        if (request.url.endsWith("/v1/organizations/org-1/projects")) return Response.json([]);
        if (request.url.endsWith("/auth/sign-in")) return Response.json({
          access_token: "user-access",
          refresh_token: "user-refresh",
          token_type: "Bearer",
          expires_in_seconds: 900,
          session_id: "session-1",
          user: { id: "user-1", email: "reader@example.test", email_verified: true, disabled: false, role: "authenticated", custom_claims: {}, created_at_ms: 1 },
        });
        if (request.url.endsWith("/auth/users")) return Response.json([]);
        if (request.url.endsWith("/auth/settings")) return Response.json({ registration_enabled: true, email_verification_required: true, access_token_ttl_seconds: 900, refresh_token_ttl_seconds: 2_592_000, password_min_length: 12 });
        if (request.url.endsWith("/snapshot")) return Response.json({ schema_version: 1, cursor: "opaque", tables: {} });
        if (request.url.endsWith("/healthz")) return Response.json({ status: "ok" });
        if (request.url.endsWith("/readyz")) return Response.json({ status: "ready" });
        if (request.url.endsWith("/metrics")) return new Response("ffdb_http_requests_total 1\nffdb_http_requests_inflight 0\n");
        if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
        if (request.url.endsWith("/policies")) return Response.json([]);
        return new Response(null, { status: 204 });
      },
    });
    render(<App client={client} configuration={configuration} />);
    expect(await screen.findByRole("link", { name: /Docs/i })).toHaveAttribute("href", "/docs/");
    fireEvent.click(screen.getByRole("button", { name: "Auth" }));
    fireEvent.click(await screen.findByRole("button", { name: "Test credentials" }));
    fireEvent.change(await screen.findByLabelText("Email"), { target: { value: "reader@example.test" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "secret-password" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByText("reader@example.test")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Sync" }));
    fireEvent.click(await screen.findByRole("button", { name: "Fetch snapshot" }));
    expect(await screen.findByText(/"cursor": "opaque"/u)).toBeInTheDocument();
    expect(calls.find((request) => request.url.endsWith("/snapshot"))?.headers.get("authorization"))
      .toBe("Bearer user-access");
  });

  it("uses the platform session for real management routes", async () => {
    const calls: Request[] = [];
    const developerSessionStore = new MemoryDeveloperSessionStore("portal-management-test");
    await developerSessionStore.set({ session_token: "platform-session", user_id: "user-1", email: "dev@example.test", expires_at_ms: 99_999 });
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerKey: "ffdb_dev_test.secret",
      developerSessionStore,
      fetch: async (input, init) => {
        const request = new Request(input, init); calls.push(request);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/v1/instance")) return Response.json(administratorStatus("owner"));
        if (request.url.endsWith("/v1/organizations")) return Response.json([{ id: "org-1", name: "Northstar", slug: "northstar", role: "owner", created_at_ms: 1 }]);
        if (request.url.endsWith("/v1/organizations/org-1/projects")) return Response.json([{ id: configuration.projectId, organization_id: "org-1", name: "Atlas", slug: "atlas", region: "local", state: "active", schema_version: 1, created_at_ms: 1 }]);
        if (request.url.endsWith("/v1/organizations/org-1/members")) return Response.json([{ organization_id: "org-1", user_id: "user-1", email: "dev@example.test", role: "owner", created_at_ms: 1 }]);
        if (request.url.endsWith("/api-keys") && request.method === "GET") return Response.json([]);
        if (request.url.endsWith("/api-keys")) return Response.json({ id: "key-1", name: "portal-generated", prefix: "ffdb_dev", secret: "one-time-secret", scopes: ["database_query"], expires_at_ms: null, created_at_ms: 1 });
        if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
        if (request.url.endsWith("/policies")) return Response.json([]);
        return Response.json({ status: "ok", version: 1 });
      },
    });
    render(<App client={client} configuration={configuration} />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
    fireEvent.change(await screen.findByLabelText("Key name"), { target: { value: "portal-generated" } });
    fireEvent.click(screen.getByRole("button", { name: "Issue one-time secret" }));
    expect(await screen.findByText("one-time-secret")).toBeInTheDocument();
    expect(calls.find((request) => request.url.endsWith("/v1/organizations"))?.headers.get("authorization")).toBe("Bearer platform-session");
    expect(calls.find((request) => request.url.endsWith("/api-keys"))?.headers.get("authorization")).toBe("Bearer platform-session");
    const issuedKeyRequest = calls.find((request) => request.url.endsWith("/api-keys") && request.method === "POST");
    await expect(issuedKeyRequest?.json()).resolves.toMatchObject({
      name: "portal-generated",
      scopes: ["database_query", "database_schema"],
    });
  });

  it("gates an unconfigured portal behind the real developer sign-in flow", async () => {
    const unconfigured: PortalConfiguration = {
      apiUrl: "https://ffdb.example.test",
      projectId: "",
      developerKey: undefined,
      projectName: "Unconfigured project",
      organizationName: "Self-hosted",
    };
    const calls: Request[] = [];
    const client = new FFDBClient({
      baseUrl: unconfigured.apiUrl,
      projectId: unconfigured.projectId,
      developerSessionStore: new MemoryDeveloperSessionStore("portal-access-gate-test"),
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/v1/developer/sign-in")) {
          return Response.json({
            session_token: "platform-session",
            user_id: "user-1",
            email: "dev@example.test",
            expires_at_ms: 99_999,
          });
        }
        if (request.url.endsWith("/v1/organizations")) {
          return Response.json([{ id: "org-1", name: "Northstar", slug: "northstar", role: "owner", created_at_ms: 1 }]);
        }
        if (request.url.endsWith("/v1/organizations/org-1/projects")) {
          return Response.json([{ id: "project-1", organization_id: "org-1", name: "Atlas", slug: "atlas", region: "local", state: "active", schema_version: 1, created_at_ms: 1 }]);
        }
        if (request.url.endsWith("/v1/organizations/org-1/members")) return Response.json([]);
        return Response.json({ error: { code: "route.missing", message: "missing", request_id: "request-access" } }, { status: 404 });
      },
    });

    render(<App client={client} configuration={unconfigured} />);

    expect(await screen.findByRole("heading", { name: "Welcome back" })).toBeInTheDocument();
    const themeToggle = screen.getByRole("button", { name: /Switch to (light|dark) mode/i });
    const nextTheme = themeToggle.getAttribute("aria-label")?.includes("light") === true ? "light" : "dark";
    fireEvent.click(themeToggle);
    await waitFor(() => expect(globalThis.document.documentElement.dataset.theme).toBe(nextTheme));
    expect(globalThis.localStorage.getItem("ffdb.portal.theme")).toBe(nextTheme);
    fireEvent.change(screen.getByLabelText("Email"), { target: { value: "dev@example.test" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "correct horse battery staple" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));

    expect(await screen.findByText("Atlas")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Projects" })).toHaveAttribute("aria-current", "page");
    const signIn = calls.find((request) => request.url.endsWith("/v1/developer/sign-in"));
    expect(signIn?.method).toBe("POST");
    await expect(signIn?.json()).resolves.toEqual({ email: "dev@example.test", password: "correct horse battery staple" });
    expect(calls.find((request) => request.url.endsWith("/v1/organizations"))?.headers.get("authorization"))
      .toBe("Bearer platform-session");
  });

  it("opens the compact mobile context trail as an accessible scope switcher", async () => {
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: configuration.projectId,
      developerKey: "ffdb_dev_test.secret",
      developerSessionStore: await signedInDeveloperStore("portal-mobile-scope-test"),
      fetch: async (input, init) => {
        const request = new Request(input, init);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (request.url.endsWith("/v1/instance")) return Response.json(administratorStatus("owner"));
        if (request.url.endsWith("/v1/organizations")) return Response.json([{ id: "org-1", name: "Northstar Labs", slug: "northstar", role: "owner", created_at_ms: 1 }]);
        if (request.url.endsWith("/v1/organizations/org-1/projects")) return Response.json([{ id: "project-1", organization_id: "org-1", name: "Atlas", slug: "atlas", region: "local", state: "active", schema_version: 1, created_at_ms: 1 }]);
        if (request.url.endsWith("/healthz")) return Response.json({ status: "ok" });
        if (request.url.endsWith("/readyz")) return Response.json({ status: "ready" });
        if (request.url.endsWith("/metrics")) return new Response("ffdb_http_requests_total 1\n");
        if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
        if (request.url.endsWith("/policies")) return Response.json([]);
        return Response.json([]);
      },
    });

    render(<App client={client} configuration={configuration} />);

    const trigger = await screen.findByRole("button", { name: "Change deployment, organization, and project" });
    fireEvent.click(trigger);
    const switcher = screen.getByRole("dialog", { name: "Change deployment, organization, and project" });
    expect(within(switcher).getAllByRole("combobox")).toHaveLength(3);
    expect(within(switcher).getByRole("combobox", { name: "Mobile active organization" })).toHaveValue("org-1");
    await waitFor(() => expect(within(switcher).getByRole("combobox", { name: "Mobile active project" })).toHaveValue("project-1"));

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Change deployment, organization, and project" })).not.toBeInTheDocument();
    await waitFor(() => expect(trigger).toHaveFocus());
  });
});

async function signedInDeveloperStore(key: string): Promise<MemoryDeveloperSessionStore> {
  const store = new MemoryDeveloperSessionStore(key);
  await store.set({ session_token: "platform-session", user_id: "developer-1", email: "developer@example.test", expires_at_ms: Date.now() + 86_400_000 });
  return store;
}

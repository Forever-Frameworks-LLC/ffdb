import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FFDBClient, MemoryDeveloperSessionStore } from "@ffdb/client";

import { App } from "./App.js";
import { InstancePanel, InstanceSetupWizard } from "./Instance.js";
import type { PortalConfiguration } from "./config.js";

const configuration: PortalConfiguration = {
  apiUrl: "https://ffdb.example.test",
  projectId: "",
  developerKey: undefined,
  projectName: "Unconfigured project",
  organizationName: "Self-hosted",
};

describe("instance onboarding and administration", () => {
  afterEach(() => {
    cleanup();
    globalThis.sessionStorage.clear();
  });

  it("bootstraps the first owner, persists the session, and completes private setup", async () => {
    const calls: Request[] = [];
    const sessionStore = new MemoryDeveloperSessionStore("instance-first-run");
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      projectId: "",
      developerSessionStore: sessionStore,
      fetch: async (input, init) => {
        const request = new Request(input, init); calls.push(request);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: true, setup_required: false, platform_byo_available: false, platform_connect_available: false });
        if (request.url.endsWith("/v1/developer/bootstrap")) return Response.json({ session_token: "owner-session", user_id: "owner-1", email: "owner@example.test", expires_at_ms: 99_999 }, { status: 201 });
        if (request.url.endsWith("/v1/instance") && request.method === "POST") return Response.json({ instance: instanceStatus("owner"), onboarding: null });
        if (request.url.endsWith("/v1/instance") && request.method === "GET") return Response.json(instanceStatus("owner"));
        if (request.url.endsWith("/v1/instance/administrators")) return Response.json([]);
        if (request.url.includes("/v1/instance/organizations")) return Response.json({ organizations: [], total: 0, limit: 25, offset: 0 });
        if (request.url.includes("/v1/instance/users")) return Response.json({ users: [], total: 0, limit: 25, offset: 0 });
        if (request.url.endsWith("/v1/instance/billing-exemptions") || request.url.endsWith("/v1/instance/plans")) return Response.json([]);
        return Response.json({ error: { code: "route.missing", message: "missing", request_id: "instance-test" } }, { status: 404 });
      },
    });

    render(<App client={client} configuration={configuration} />);
    expect(await screen.findByRole("heading", { name: "Create the instance owner" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("One-time bootstrap token"), { target: { value: "bootstrap-token" } });
    fireEvent.change(screen.getByLabelText("Owner email"), { target: { value: "owner@example.test" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "correct horse battery staple" } });
    fireEvent.change(screen.getByLabelText("Confirm password"), { target: { value: "correct horse battery staple" } });
    fireEvent.click(screen.getByRole("button", { name: "Create owner" }));

    expect(await screen.findByRole("heading", { name: "Choose how this instance operates" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /Platform with Stripe/i })).toBeDisabled();
    expect(screen.getByRole("radio", { name: /Connected platform/i })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Finish instance setup" }));

    await waitFor(() => expect(calls.some((request) => request.url.endsWith("/v1/instance") && request.method === "POST")).toBe(true));
    const bootstrap = calls.find((request) => request.url.endsWith("/v1/developer/bootstrap"));
    expect(bootstrap?.headers.get("x-ffdb-bootstrap-token")).toBe("bootstrap-token");
    await expect(bootstrap?.json()).resolves.toEqual({ email: "owner@example.test", password: "correct horse battery staple" });
    const configure = calls.find((request) => request.url.endsWith("/v1/instance") && request.method === "POST");
    expect(configure?.headers.get("authorization")).toBe("Bearer owner-session");
    await expect(configure?.json()).resolves.toEqual({ deployment_mode: "private", organization_creation_policy: "owner_only" });
    await expect(sessionStore.get()).resolves.toMatchObject({ session_token: "owner-session", user_id: "owner-1" });
  });

  it("submits Connect credentials without browser persistence and unlocks only after server refresh", async () => {
    const calls: Request[] = [];
    const store = new MemoryDeveloperSessionStore("instance-connect-onboarding");
    await store.set({ session_token: "owner-session", user_id: "owner-1", email: "owner@example.test", expires_at_ms: 99_999 });
    const pending = {
      ...instanceStatus("owner"),
      deployment_mode: "platform_connect" as const,
      setup_completed_at_ms: null,
      billing_enforcement_enabled: false,
      billing_account: { mode: "stripe_connect" as const, status: "onboarding" as const, provider_account_id: "acct_pending", charges_enabled: false, payouts_enabled: false, details_submitted: false, capabilities: [], credentials_configured: true, updated_at_ms: 1 },
    };
    const complete = { ...pending, setup_completed_at_ms: 2, billing_enforcement_enabled: true, billing_account: { ...pending.billing_account, status: "enabled" as const, charges_enabled: true, payouts_enabled: true, details_submitted: true } };
    const onComplete = vi.fn();
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      developerSessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init); calls.push(request);
        if (request.url.endsWith("/v1/instance") && request.method === "POST") return Response.json({ instance: pending, onboarding: { url: "https://connect.stripe.test/onboard", expires_at_ms: 999_999 } });
        if (request.url.endsWith("/v1/instance/billing/refresh")) return Response.json(complete);
        if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        return Response.json({ error: { code: "route.missing", message: "missing", request_id: "connect-onboarding" } }, { status: 404 });
      },
    });

    render(<InstanceSetupWizard apiUrl={configuration.apiUrl} capabilities={{ bootstrap_available: false, setup_required: true, platform_byo_available: true, platform_connect_available: true }} client={client} onComplete={onComplete} />);
    fireEvent.click(screen.getByRole("radio", { name: /Connected platform/i }));
    fireEvent.change(screen.getByLabelText("Stripe Connect secret key"), { target: { value: "sk_test_connect" } });
    fireEvent.change(screen.getByLabelText("Stripe Connect webhook secret"), { target: { value: "whsec_connect" } });
    fireEvent.change(screen.getByLabelText("Stripe account email"), { target: { value: "owner@example.test" } });
    fireEvent.click(screen.getByRole("button", { name: "Finish instance setup" }));

    expect(await screen.findByRole("heading", { name: "Finish payment setup" })).toBeInTheDocument();
    expect(screen.getByText(/organizations and projects remain locked/i)).toBeInTheDocument();
    expect(onComplete).not.toHaveBeenCalled();
    const configure = calls.find((request) => request.url.endsWith("/v1/instance") && request.method === "POST");
    await expect(configure?.clone().json()).resolves.toMatchObject({ deployment_mode: "platform_connect", secret_key: "sk_test_connect", webhook_secret: "whsec_connect" });
    expect(globalThis.localStorage.getItem("sk_test_connect")).toBeNull();
    expect(globalThis.sessionStorage.getItem("sk_test_connect")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Refresh Stripe status" }));
    await waitFor(() => expect(onComplete).toHaveBeenCalledWith(expect.objectContaining({ setup_completed_at_ms: 2 })));
  });

  it.each([
    ["owner", true],
    ["admin", false],
  ] as const)("shows deployment credential controls for %s = %s", async (role, canConfigure) => {
    const store = new MemoryDeveloperSessionStore(`instance-role-${role}`);
    await store.set({ session_token: `${role}-session`, user_id: `${role}-1`, email: `${role}@example.test`, expires_at_ms: 99_999 });
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      developerSessionStore: store,
      fetch: instanceAdminFetch(role),
    });
    render(<InstancePanel apiUrl={configuration.apiUrl} client={client} onNotice={() => undefined} view="billing" />);

    expect(await screen.findByRole("heading", { name: "Billing provider" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Provider" })).toHaveAttribute("aria-selected", "true");
    if (canConfigure) {
      fireEvent.click(screen.getByRole("button", { name: "Edit configuration" }));
      expect(screen.getByRole("button", { name: "Save deployment configuration" })).toBeInTheDocument();
      expect(screen.getByLabelText("Stripe secret key")).toBeInTheDocument();
    } else {
      expect(screen.queryByRole("button", { name: "Edit configuration" })).not.toBeInTheDocument();
      expect(screen.queryByLabelText("Stripe secret key")).not.toBeInTheDocument();
      expect(screen.getByText(/Only the owner can change deployment or payment credentials/i)).toBeInTheDocument();
    }
  });

  it("labels private-instance organization usage as analytics instead of enforced billing", async () => {
    const store = new MemoryDeveloperSessionStore("instance-private-analytics");
    await store.set({ session_token: "owner-session", user_id: "owner-1", email: "owner@example.test", expires_at_ms: 99_999 });
    const fallback = instanceAdminFetch("owner");
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      developerSessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        const url = new URL(request.url);
        if (url.pathname === "/v1/instance") return Response.json({ ...instanceStatus("owner"), deployment_mode: "private", billing_enforcement_enabled: false, billing_account: null });
        if (url.pathname === "/v1/instance/organizations") return Response.json({ organizations: [organization()], total: 1, limit: 25, offset: 0 });
        return fallback(input, init);
      },
    });

    render(<InstancePanel apiUrl={configuration.apiUrl} client={client} onNotice={() => undefined} view="billing" />);
    fireEvent.click(await screen.findByRole("tab", { name: "Organizations" }));
    expect(await screen.findByText("Analytics only")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Billing provider" })).not.toBeInTheDocument();
    expect(screen.queryByText("Plan enforced")).not.toBeInTheDocument();
  });

  it("keeps Connect pending when provider refresh fails and explains automatic catalog provisioning", async () => {
    const store = new MemoryDeveloperSessionStore("instance-connect-refresh");
    await store.set({ session_token: "owner-session", user_id: "owner-1", email: "owner@example.test", expires_at_ms: 99_999 });
    const fallback = instanceAdminFetch("owner");
    const notice = vi.fn();
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      developerSessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        if (request.url.endsWith("/v1/instance/billing/refresh")) return Response.json({ error: { code: "stripe.account_pending", message: "charges are still disabled", request_id: "instance-connect" } }, { status: 409 });
        if (request.url.endsWith("/v1/instance") && request.method === "GET") return Response.json({ ...instanceStatus("owner"), deployment_mode: "platform_connect", billing_enforcement_enabled: false, billing_account: { mode: "stripe_connect", status: "onboarding", provider_account_id: "acct_pending", charges_enabled: false, payouts_enabled: false, details_submitted: false, capabilities: [], credentials_configured: false, updated_at_ms: 1 } });
        return fallback(input, init);
      },
    });

    render(<InstancePanel apiUrl={configuration.apiUrl} client={client} onNotice={notice} view="billing" />);
    expect(await screen.findByText(/provisions or repairs the connected account's plan catalog automatically/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Refresh Stripe status" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/Billing has not been activated: charges are still disabled/i);
    expect(notice).not.toHaveBeenCalledWith(expect.stringMatching(/status refreshed/i));
  });

  it("audits global user and organization status controls and locks provider-bound plan prices", async () => {
    vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    const calls: Request[] = [];
    const store = new MemoryDeveloperSessionStore("instance-global-management");
    await store.set({ session_token: "admin-session", user_id: "admin-1", email: "admin@example.test", expires_at_ms: 99_999 });
    const client = new FFDBClient({
      baseUrl: configuration.apiUrl,
      developerSessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        const url = new URL(request.url);
        if (url.pathname === "/v1/instance/setup/status") return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
        if (url.pathname === "/v1/instance/administrators" && request.method === "POST") return Response.json({ user_id: "user-2", email: "member@example.test", role: "admin", granted_by: "admin-1", created_at_ms: 2 }, { status: 201 });
        if (url.pathname === "/v1/instance/administrators") return Response.json([{ user_id: "owner-1", email: "owner@example.test", role: "owner", granted_by: null, created_at_ms: 1 }]);
        if (url.pathname === "/v1/instance/organizations/org-1" && request.method === "PATCH") return Response.json({ ...organization(), disabled: true });
        if (url.pathname === "/v1/instance/users/user-2" && request.method === "PATCH") return Response.json({ ...platformUser(), disabled: true });
        if (url.pathname === "/v1/instance/organizations") return Response.json({ organizations: [organization()], total: 1, limit: 25, offset: 0 });
        if (url.pathname === "/v1/instance/users") return Response.json({ users: [{ ...platformUser(), id: "owner-1", email: "owner@example.test", instance_role: "owner" }, platformUser()], total: 2, limit: 25, offset: 0 });
        if (url.pathname === "/v1/instance/billing-exemptions") return Response.json([]);
        if (url.pathname === "/v1/instance/plans/pro" && request.method === "PUT") return Response.json(providerBoundPlan());
        if (url.pathname === "/v1/instance/plans") return Response.json([providerBoundPlan()]);
        if (url.pathname === "/v1/instance") return Response.json(instanceStatus("admin"));
        return Response.json({ error: { code: "route.missing", message: "missing", request_id: "instance-management" } }, { status: 404 });
      },
    });

    const billingView = render(<InstancePanel apiUrl={configuration.apiUrl} client={client} onNotice={() => undefined} view="billing" />);
    fireEvent.click(await screen.findByRole("tab", { name: "Plans" }));
    expect(await screen.findByText(/Stripe-bound pricing remains read-only/i)).toBeInTheDocument();
    expect(screen.getByLabelText("Billing unit")).toBeDisabled();
    expect(screen.getByLabelText("Base price (cents)")).toBeDisabled();
    expect(screen.getByLabelText("Currency")).toBeDisabled();
    expect(screen.getByLabelText("Monthly active users")).toBeDisabled();
    expect(screen.getByLabelText("Project limit")).not.toBeDisabled();
    fireEvent.change(screen.getByLabelText("Project limit"), { target: { value: "25" } });
    fireEvent.click(screen.getByRole("button", { name: "Save Pro" }));
    await waitFor(() => expect(calls.some((request) => new URL(request.url).pathname === "/v1/instance/plans/pro" && request.method === "PUT")).toBe(true));
    const planPut = calls.find((request) => new URL(request.url).pathname === "/v1/instance/plans/pro" && request.method === "PUT");
    const planBody = await planPut?.clone().json() as Record<string, unknown>;
    expect(planBody.project_limit).toBe(25);
    expect(planBody).not.toHaveProperty("provider_catalog_bound");

    fireEvent.click(screen.getByRole("tab", { name: "Organizations" }));
    fireEvent.click(screen.getByRole("button", { name: "Disable…" }));
    await waitFor(() => expect(calls.some((request) => new URL(request.url).pathname === "/v1/instance/organizations/org-1" && request.method === "PATCH")).toBe(true));
    billingView.unmount();

    render(<InstancePanel apiUrl={configuration.apiUrl} client={client} onNotice={() => undefined} view="users" />);
    expect(await screen.findByRole("heading", { name: "Instance administrators" })).toBeInTheDocument();
    expect(screen.queryByLabelText("User to make instance administrator")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Add administrator" }));
    fireEvent.change(screen.getByLabelText("User to make instance administrator"), { target: { value: "user-2" } });
    fireEvent.click(screen.getByRole("button", { name: "Grant access" }));
    await waitFor(() => expect(calls.some((request) => new URL(request.url).pathname === "/v1/instance/administrators" && request.method === "POST")).toBe(true));
    fireEvent.click(screen.getByRole("tab", { name: "All users" }));
    expect(await screen.findByRole("heading", { name: "Instance users" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Owner protected" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Disable…" }));
    await waitFor(() => expect(calls.some((request) => new URL(request.url).pathname === "/v1/instance/users/user-2" && request.method === "PATCH")).toBe(true));
    const organizationPatch = calls.find((request) => new URL(request.url).pathname === "/v1/instance/organizations/org-1" && request.method === "PATCH");
    const userPatch = calls.find((request) => new URL(request.url).pathname === "/v1/instance/users/user-2" && request.method === "PATCH");
    await expect(organizationPatch?.clone().json()).resolves.toEqual({ disabled: true });
    await expect(userPatch?.clone().json()).resolves.toEqual({ disabled: true });
  });
});

function instanceStatus(role: "owner" | "admin") {
  return {
    owner_user_id: "owner-1",
    current_user_role: role,
    deployment_mode: "platform_byo",
    organization_creation_policy: "owner_only",
    billing_enforcement_enabled: true,
    setup_completed_at_ms: 1,
    billing_account: { mode: "byo_keys", status: "enabled", provider_account_id: null, charges_enabled: true, payouts_enabled: true, details_submitted: true, capabilities: ["card_payments"], credentials_configured: true, updated_at_ms: 1 },
    administrator_count: 2,
    created_at_ms: 1,
    updated_at_ms: 1,
  };
}

function instanceAdminFetch(role: "owner" | "admin"): typeof fetch {
  return async (input, init) => {
    const request = new Request(input, init);
    if (request.url.endsWith("/v1/instance/setup/status")) return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
    if (request.url.endsWith("/v1/instance/administrators")) return Response.json([{ user_id: "owner-1", email: "owner@example.test", role: "owner", granted_by: null, created_at_ms: 1 }]);
    if (request.url.includes("/v1/instance/organizations")) return Response.json({ organizations: [], total: 0, limit: 25, offset: 0 });
    if (request.url.includes("/v1/instance/users")) return Response.json({ users: [], total: 0, limit: 25, offset: 0 });
    if (request.url.endsWith("/v1/instance/billing-exemptions") || request.url.endsWith("/v1/instance/plans")) return Response.json([]);
    if (request.url.endsWith("/v1/instance")) return Response.json(instanceStatus(role));
    return Response.json({ error: { code: "route.missing", message: "missing", request_id: "instance-admin" } }, { status: 404 });
  };
}

function organization() { return { id: "org-1", name: "Northstar Labs", slug: "northstar", disabled: false, member_count: 3, project_count: 2, billing_exempt: false, created_at_ms: 1 }; }
function platformUser() { return { id: "user-2", email: "member@example.test", email_verified: true, disabled: false, instance_role: null, organization_count: 1, created_at_ms: 1 }; }
function providerBoundPlan() { return { tier: "pro", display_name: "Pro", billing_unit: "organization", base_price_cents: 4900, currency: "usd", project_limit: null, storage_bytes: 100_000_000_000, monthly_reads: 100_000_000, monthly_writes: 10_000_000, monthly_active_users: 100_000, overage_enabled: true, reads_at_limit: "overage", writes_at_limit: "overage", signups_at_limit: "overage", requires_payment_method_for_overage: true, active: true, provider_catalog_bound: true, updated_at_ms: 1 }; }

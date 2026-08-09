import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ApiKeySummary,
  DeveloperSession,
  FFDBClient,
  OrganizationSummary,
  PlatformBillingSummary,
  PlatformInvoiceSummary,
  PlatformUsageSummary,
} from "@ffdb/client";

import { persistPortalInstance, persistPortalProject, portalProjectKey, type PortalConfiguration } from "../config.js";
import { PolishedAccountPanel, PolishedSettingsPanel, PolishedUsagePanel } from "./AccountAdmin.js";

const now = 1_725_000_000_000;
const configuration: PortalConfiguration = {
  apiUrl: "https://ffdb.example.test",
  instanceName: "Production",
  organizationId: "org-1",
  organizationName: "Northstar",
  projectId: "project-1",
  projectName: "Atlas",
  developerKey: "ffdb_dev_livekey.secret-value",
};
const owner: OrganizationSummary = { id: "org-1", name: "Northstar", slug: "northstar", role: "owner", created_at_ms: now };
const session: DeveloperSession = { session_token: "never-render-this-token", user_id: "user-owner", email: "owner@example.test", expires_at_ms: Date.now() + 3_600_000 };
const key: ApiKeySummary = { id: "key-1", name: "production", prefix: "livekey", scopes: ["database_query"], created_at_ms: now, expires_at_ms: null, revoked_at_ms: null };

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("polished account, security settings, and usage", () => {
  it("keeps saved deployment sessions isolated and never renders the developer session token", async () => {
    persistPortalInstance({ apiUrl: configuration.apiUrl, instanceName: "Production" });
    persistPortalInstance({ apiUrl: "https://staging.example.test", instanceName: "Staging" });
    const onInstanceChange = vi.fn();
    const client = { developerSession: vi.fn().mockResolvedValue(session), refreshDeveloperSession: vi.fn(), developerSignOut: vi.fn() } as unknown as FFDBClient;

    render(<PolishedAccountPanel client={client} configuration={configuration} onInstanceChange={onInstanceChange} onNotice={vi.fn()} />);

    expect(await screen.findByRole("heading", { name: "Developer session" })).toBeInTheDocument();
    expect(screen.getAllByText("owner@example.test")).toHaveLength(2);
    expect(screen.queryByText(session.session_token)).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Saved deployments" })).not.toBeInTheDocument();
    const accountTasks = screen.getByRole("tablist", { name: "Account tasks" });
    const profileTab = within(accountTasks).getByRole("tab", { name: "Profile & session" });
    const instancesTab = within(accountTasks).getByRole("tab", { name: "Deployments 2" });
    expect(profileTab).toHaveAttribute("aria-selected", "true");
    expect(instancesTab).toHaveAttribute("aria-selected", "false");
    fireEvent.click(instancesTab);

    expect(instancesTab).toHaveAttribute("aria-selected", "true");
    expect(screen.queryByRole("heading", { name: "Developer session" })).not.toBeInTheDocument();
    expect(screen.getByRole("tabpanel", { name: "Deployments 2" })).toBeInTheDocument();
    const table = screen.getByRole("heading", { name: "Saved deployments" }).closest("section");
    fireEvent.click(within(table!).getByRole("button", { name: "Switch" }));
    expect(onInstanceChange).toHaveBeenCalledWith(expect.objectContaining({ apiUrl: "https://staging.example.test", instanceName: "Staging" }));
  });

  it("offers a direct sign-in recovery action when the saved developer session is invalid", async () => {
    const onSignedOut = vi.fn();
    const developerSignOut = vi.fn().mockRejectedValue(new Error("developer credentials are invalid"));
    const client = { developerSession: vi.fn().mockRejectedValue(new Error("developer credentials are invalid")), developerSignOut } as unknown as FFDBClient;

    render(<PolishedAccountPanel client={client} configuration={configuration} onInstanceChange={vi.fn()} onNotice={vi.fn()} onSignedOut={onSignedOut} />);

    expect(await screen.findByText("developer credentials are invalid")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Sign in again" }));
    await waitFor(() => expect(onSignedOut).toHaveBeenCalledOnce());
    expect(developerSignOut).toHaveBeenCalledOnce();
  });

  it("issues a least-privilege key while showing its secret only in the one-time result", async () => {
    const createApiKey = vi.fn().mockResolvedValue({ ...key, name: "ci-deploy", secret: "ffdb_dev_created.one-time-secret" });
    const client = settingsClient({ createApiKey });

    render(<PolishedSettingsPanel client={client} configuration={configuration} onConfiguration={vi.fn()} onNotice={vi.fn()} />);

    const form = (await screen.findByRole("heading", { name: "Issue an API key" })).closest("section")!;
    expect(screen.queryByText("one-time-secret", { exact: false })).not.toBeInTheDocument();
    fireEvent.change(within(form).getByLabelText("Key name"), { target: { value: "ci-deploy" } });
    fireEvent.click(within(form).getByLabelText("Run database queries"));
    fireEvent.click(within(form).getByLabelText("Read audit logs"));
    fireEvent.click(within(form).getByRole("button", { name: "Issue one-time secret" }));

    await waitFor(() => expect(createApiKey).toHaveBeenCalledWith(expect.objectContaining({ name: "ci-deploy", scopes: ["database_schema", "logs_read"] })));
    expect(await screen.findByText("ffdb_dev_created.one-time-secret")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "I saved it" }));
    expect(screen.queryByText("ffdb_dev_created.one-time-secret")).not.toBeInTheDocument();
  });

  it("clears the matching browser credential when its exact key prefix is revoked", async () => {
    persistPortalProject(configuration.projectId, configuration.developerKey, configuration.organizationName, configuration.organizationId, configuration.projectName, configuration.apiUrl);
    const revokeApiKey = vi.fn().mockResolvedValue(undefined);
    const setDeveloperKey = vi.fn();
    const onConfiguration = vi.fn();
    vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    const client = settingsClient({ revokeApiKey, setDeveloperKey });

    render(<PolishedSettingsPanel client={client} configuration={configuration} onConfiguration={onConfiguration} onNotice={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke…" }));

    await waitFor(() => expect(revokeApiKey).toHaveBeenCalledWith("key-1"));
    expect(setDeveloperKey).toHaveBeenCalledWith(null);
    expect(onConfiguration).toHaveBeenCalledWith(expect.objectContaining({ developerKey: undefined }));
    expect(portalProjectKey(configuration.apiUrl, configuration.projectId)).toBeUndefined();
  });

  it("makes credential and billing mutations read-only for a viewer", async () => {
    const viewer = { ...owner, role: "viewer" as const };
    const createApiKey = vi.fn();
    const settings = settingsClient({ organizations: vi.fn().mockResolvedValue([viewer]), createApiKey });
    const billing = vi.fn().mockResolvedValue(billingSummary());
    const usageClient = usageTestClient(viewer, { organizationBilling: billing });

    const { unmount } = render(<PolishedSettingsPanel client={settings} configuration={configuration} onConfiguration={vi.fn()} onNotice={vi.fn()} />);
    const issue = await screen.findByRole("button", { name: "Issue one-time secret" });
    expect(issue).toBeDisabled();
    expect(screen.getByText(/require an organization owner or administrator/i)).toBeInTheDocument();
    expect(createApiKey).not.toHaveBeenCalled();
    unmount();

    render(<PolishedUsagePanel client={usageClient} configuration={configuration} onNotice={vi.fn()} />);
    expect(await screen.findByRole("progressbar", { name: "Storage allowance used" })).toBeInTheDocument();
    expect(screen.queryByText("Plan changes are read-only.")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Plan & payment" }));
    expect(await screen.findByText("Plan changes are read-only.")).toBeInTheDocument();
    expect(screen.queryByRole("progressbar", { name: "Storage allowance used" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Choose Pro" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Choose pay as you go" })).toBeDisabled();
  });

  it("renders exact usage reporting context and five-column searchable invoices", async () => {
    render(<PolishedUsagePanel client={usageTestClient(owner)} configuration={configuration} onNotice={vi.fn()} />);

    expect(await screen.findByRole("progressbar", { name: "Storage allowance used" })).toHaveAttribute("aria-valuenow", "50");
    expect(screen.queryByRole("heading", { name: "Invoices & reporting" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Invoices & reporting 1" }));
    expect(screen.getByText(/Last successful report/i)).toBeInTheDocument();
    expect(screen.queryByRole("progressbar", { name: "Storage allowance used" })).not.toBeInTheDocument();
    const invoiceTable = screen.getByRole("heading", { name: "Invoices & reporting" }).closest("section")!;
    expect(within(invoiceTable).getAllByRole("columnheader")).toHaveLength(5);
    expect(within(invoiceTable).getAllByRole("cell")).toHaveLength(5);
    fireEvent.change(within(invoiceTable).getByPlaceholderText("Search invoices"), { target: { value: "paid" } });
    expect(within(invoiceTable).getByText("paid")).toBeInTheDocument();
  });

  it("shows private-instance usage as analytics instead of plan limits", async () => {
    const privateSummary = { ...billingSummary(), billing_enforcement_enabled: false, provider_configured: false };
    render(<PolishedUsagePanel client={usageTestClient(owner, { organizationBilling: vi.fn().mockResolvedValue(privateSummary) })} configuration={configuration} onNotice={vi.fn()} />);

    expect(await screen.findByRole("heading", { name: "Operational usage" })).toBeInTheDocument();
    expect(screen.getByText("No billing limits apply.")).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Plan & payment" })).not.toBeInTheDocument();
    expect(screen.queryByText(/project limit/i)).not.toBeInTheDocument();
  });

  it("keeps signing rotation isolated to the advanced settings task", async () => {
    const rotateSigningKey = vi.fn().mockResolvedValue({ active_kid: "kid-next" });
    vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    render(<PolishedSettingsPanel client={settingsClient({ rotateSigningKey })} configuration={configuration} onConfiguration={vi.fn()} onNotice={vi.fn()} />);

    expect(await screen.findByRole("heading", { name: "Issue an API key" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Signing & advanced security" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Signing & advanced" }));

    expect(await screen.findByRole("heading", { name: "Signing & advanced security" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Issue an API key" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Rotate signing key…" }));
    await waitFor(() => expect(rotateSigningKey).toHaveBeenCalledOnce());
  });
});

function settingsClient(overrides: Record<string, unknown> = {}): FFDBClient {
  return {
    developerSession: vi.fn().mockResolvedValue(session),
    organizations: vi.fn().mockResolvedValue([owner]),
    apiKeys: vi.fn().mockResolvedValue([key]),
    createApiKey: vi.fn(),
    revokeApiKey: vi.fn(),
    rotateSigningKey: vi.fn(),
    setDeveloperKey: vi.fn(),
    ...overrides,
  } as unknown as FFDBClient;
}

function billingSummary(): PlatformBillingSummary {
  return {
    organization_id: owner.id,
    tier: "free",
    status: "free",
    billing_unit: "organization",
    seat_quantity: 1,
    project_limit: 2,
    usage_allowance: { storage_bytes: 1_000, monthly_reads: 100, monthly_writes: 50, monthly_active_users: 10, overage_enabled: false },
    current_period_start_ms: now,
    current_period_end_ms: now + 30 * 86_400_000,
    cancel_at_period_end: false,
    provider_configured: true,
    billing_enforcement_enabled: true,
    billing_exempt: false,
  };
}

function usageSummary(): PlatformUsageSummary {
  return {
    organization_id: owner.id,
    period_start_ms: now,
    period_end_ms: now + 30 * 86_400_000,
    reads: 25,
    writes: 10,
    storage_bytes: 500,
    storage_byte_hours: 1_200,
    monthly_active_users: 3,
    reporting_status: "healthy",
    reporting_last_success_ms: now + 1_000,
    as_of_ms: now + 2_000,
  };
}

function invoice(): PlatformInvoiceSummary {
  return {
    id: "invoice-1",
    organization_id: owner.id,
    status: "paid",
    currency: "usd",
    amount_due_minor: 1_250,
    amount_paid_minor: 1_250,
    period_start_ms: now,
    period_end_ms: now + 30 * 86_400_000,
    hosted_invoice_url: "https://billing.example.test/invoice/1",
    invoice_pdf_url: null,
    created_at_ms: now,
  };
}

function usageTestClient(organization: OrganizationSummary, overrides: Record<string, unknown> = {}): FFDBClient {
  return {
    organizations: vi.fn().mockResolvedValue([organization]),
    organizationBilling: vi.fn().mockResolvedValue(billingSummary()),
    organizationUsage: vi.fn().mockResolvedValue(usageSummary()),
    organizationInvoices: vi.fn().mockResolvedValue([invoice()]),
    createBillingCheckout: vi.fn(),
    createBillingPortal: vi.fn(),
    ...overrides,
  } as unknown as FFDBClient;
}

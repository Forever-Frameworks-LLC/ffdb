import { useCallback, useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";

import {
  FFDBClient,
  type CompleteInstanceSetupRequest,
  type CompleteInstanceSetupResponse,
  type InstanceAdministratorSummary,
  type InstanceBillingAccountSummary,
  type InstanceDeploymentMode,
  type InstanceOrganizationPage,
  type InstanceOrganizationSummary,
  type InstancePlanCatalogEntry,
  type InstanceStatus,
  type InstanceUserPage,
  type InstanceUserSummary,
  type OrganizationBillingExemptionSummary,
  type OrganizationCreationPolicy,
  type PlatformBillingTier,
  type PlatformBillingUnit,
  type PublicInstanceSetupStatus,
  type PutInstancePlanCatalogEntryRequest,
} from "@ffdb/client";

import { Icon } from "./icons.js";
import { ManagedTable } from "./polish/ManagedTable.js";
import { BrandMark } from "./ui.js";
import "./instance-admin.css";

type DeploymentMode = InstanceDeploymentMode;
type Administrator = InstanceAdministratorSummary;
type Organization = InstanceOrganizationSummary;
type User = InstanceUserSummary;
type BillingExemption = OrganizationBillingExemptionSummary;
type PlanCatalogEntry = InstancePlanCatalogEntry;
type PlanInput = PutInstancePlanCatalogEntryRequest;
type SetupRequest = CompleteInstanceSetupRequest;
type SetupResponse = CompleteInstanceSetupResponse;
type BillingTier = PlatformBillingTier;
type BillingUnit = PlatformBillingUnit;
export type InstanceSetupCapabilities = PublicInstanceSetupStatus & {
  readonly platform_byo_available?: boolean;
  readonly platform_connect_available?: boolean;
};

interface InstanceSetupWizardProps {
  readonly apiUrl: string;
  readonly client: FFDBClient;
  readonly embedded?: boolean;
  readonly initialMode?: DeploymentMode;
  readonly initialPolicy?: OrganizationCreationPolicy;
  readonly capabilities?: InstanceSetupCapabilities;
  onComplete(status: InstanceStatus): void;
  onCancel?(): void;
}

const deploymentOptions: readonly { readonly mode: Exclude<DeploymentMode, "unconfigured">; readonly name: string; readonly detail: string; readonly billing: string }[] = [
  { mode: "private", name: "Private workspace", detail: "One owner can create organizations and projects without tenant charges.", billing: "No billing" },
  { mode: "team", name: "Team installation", detail: "Invite teammates and run multiple organizations without charging them.", billing: "No billing" },
  { mode: "platform_byo", name: "Platform with Stripe", detail: "Charge organizations for FFDB plans using this installation's Stripe account.", billing: "Bring your own keys" },
  { mode: "platform_connect", name: "Connected platform", detail: "Onboard through Stripe Connect; after the account is ready, refresh to provision the plan catalog automatically.", billing: "Stripe Connect" },
];

export function InstanceSetupWizard({ apiUrl, client, embedded = false, initialMode = "private", initialPolicy = "owner_only", capabilities, onComplete, onCancel }: InstanceSetupWizardProps) {
  const [mode, setMode] = useState<Exclude<DeploymentMode, "unconfigured">>(initialMode === "unconfigured" ? "private" : initialMode);
  const [policy, setPolicy] = useState(initialPolicy);
  const [secretKey, setSecretKey] = useState("");
  const [webhookSecret, setWebhookSecret] = useState("");
  const [country, setCountry] = useState("US");
  const [email, setEmail] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<SetupResponse | null>(null);

  const completeWhenServerUnlocks = async (status: InstanceStatus): Promise<boolean> => {
    const setup = await client.instanceSetupStatus();
    if (status.setup_completed_at_ms !== null && setup.setup_required === false) {
      onComplete(status);
      return true;
    }
    setError("Setup is still locked. Stripe must enable charges and payouts and FFDB must finish provisioning the plan catalog before organizations or projects can be created.");
    return false;
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setSubmitting(true);
    setError(null);
    try {
      const common = { organization_creation_policy: policy } as const;
      const request: SetupRequest = mode === "platform_byo"
        ? { ...common, deployment_mode: mode, secret_key: secretKey, webhook_secret: webhookSecret }
        : mode === "platform_connect"
          ? { ...common, deployment_mode: mode, secret_key: secretKey, webhook_secret: webhookSecret, country, email, return_url: portalReturnUrl("return"), refresh_url: portalReturnUrl("refresh") }
          : { ...common, deployment_mode: mode };
      const response = await client.configureInstance(request, { idempotencyKey: newIdempotencyKey("instance-setup") });
      setSecretKey("");
      setWebhookSecret("");
      if (response.instance.setup_completed_at_ms !== null && await completeWhenServerUnlocks(response.instance)) return;
      setResult(response);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setSubmitting(false);
    }
  };

  const refreshCompletion = async () => {
    setRefreshing(true);
    setError(null);
    try {
      const status = await client.refreshInstanceBilling();
      await completeWhenServerUnlocks(status);
    } catch (cause) {
      setError(`Stripe status could not be refreshed. Setup remains locked: ${errorMessage(cause)}`);
    } finally {
      setRefreshing(false);
    }
  };

  const content = result === null ? (
    <section className={embedded ? "instance-wizard embedded" : "instance-wizard"} aria-labelledby="instance-setup-heading">
      <div className="instance-wizard-heading">
        <div>
          {embedded ? <h2 id="instance-setup-heading">Choose how this instance operates</h2> : <h1 id="instance-setup-heading">Choose how this instance operates</h1>}
          <p>You can run FFDB privately, share it with a team, or operate a metered platform. Project commerce stays separately configurable per project.</p>
        </div>
        {onCancel === undefined ? null : <button className="secondary-action" type="button" onClick={onCancel}>Cancel</button>}
      </div>
      {embedded ? null : <ol className="setup-progress setup-progress-wide" aria-label="Instance setup progress"><li className="complete"><span>1</span>Owner</li><li aria-current="step"><span>2</span>Instance type</li><li><span>3</span>Payments</li><li><span>4</span>First workspace</li></ol>}
      <form onSubmit={(event) => void submit(event)}>
        <fieldset className="mode-fieldset">
          <legend>Deployment mode</legend>
          <div className="mode-grid">
            {deploymentOptions.map((option) => {
              const available = option.mode === "platform_byo"
                ? capabilities?.platform_byo_available === true || initialMode === option.mode
                : option.mode === "platform_connect"
                  ? capabilities?.platform_connect_available === true || initialMode === option.mode
                  : true;
              return <label className={`${mode === option.mode ? "mode-option selected" : "mode-option"}${available ? "" : " unavailable"}`} key={option.mode}>
              <input checked={mode === option.mode} disabled={!available} name="deployment-mode" onChange={() => setMode(option.mode)} type="radio" value={option.mode} />
              <span className="mode-check"><Icon name="check" size={13} /></span>
              <strong>{option.name}</strong>
              <small>{available ? option.billing : "Not configured by this host"}</small>
              <p>{option.detail}</p>
            </label>})}
          </div>
        </fieldset>
        <label className="field wide-field"><span>Who can create organizations?</span><select value={policy} onChange={(event) => setPolicy(event.target.value as OrganizationCreationPolicy)}><option value="owner_only">Only the instance owner</option><option value="authenticated">Any authenticated user</option><option value="invitation_only">Users who joined by invitation</option></select></label>
        {mode === "platform_byo" ? <div className="provider-fields"><Field label="Stripe secret key" type="password" value={secretKey} onChange={setSecretKey} /><Field label="Stripe webhook secret" type="password" value={webhookSecret} onChange={setWebhookSecret} /><p className="field-note">Credentials are encrypted by the API and are never returned to the browser. FFDB provisions and verifies the Stripe plan catalog before activating tenant billing.</p></div> : null}
        {mode === "platform_connect" ? <div className="provider-fields"><Field label="Stripe Connect secret key" type="password" value={secretKey} onChange={setSecretKey} /><Field label="Stripe Connect webhook secret" type="password" value={webhookSecret} onChange={setWebhookSecret} /><Field label="Stripe account country" value={country} onChange={(value) => setCountry(value.toUpperCase())} /><Field label="Stripe account email" type="email" value={email} onChange={setEmail} /><p className="field-note">These credentials are sent directly to the FFDB API for encryption and are never saved by the portal. Continue to Stripe after saving; FFDB unlocks organizations and projects only after charges, payouts, and the platform plan catalog are ready.</p></div> : null}
        {error === null ? null : <div className="access-error" role="alert">{error}</div>}
        <div className="wizard-actions"><button className="primary-action" disabled={submitting} type="submit">{submitting ? "Saving…" : embedded ? "Save deployment configuration" : "Finish instance setup"}</button><span>Tenant billing is enabled only for the two platform modes. Once an organization has active billing, cancel and reconcile every subscription before changing the platform billing mode or Stripe account.</span></div>
      </form>
    </section>
  ) : (
    <section className={embedded ? "instance-wizard embedded setup-complete" : "instance-wizard setup-complete"} aria-labelledby="connect-ready-heading">
      <span className="pending-mark"><Icon name="lock" size={24} /></span>
      {embedded ? <h2 id="connect-ready-heading">Finish payment setup</h2> : <h1 id="connect-ready-heading">Finish payment setup</h1>}
      <p>The deployment choice is saved, but this instance remains locked until Stripe enables charges and payouts and FFDB provisions the platform plan catalog.</p>
      {error === null ? null : <div className="access-error" role="alert">{error}</div>}
      <div className="wizard-actions centered">{result.onboarding === null ? null : <a className="primary-action action-link" href={result.onboarding.url}>Continue to Stripe</a>}<button className="secondary-action" disabled={refreshing} type="button" onClick={() => void refreshCompletion()}>{refreshing ? "Checking Stripe…" : "Refresh Stripe status"}</button></div>
      <p className="setup-lock-message"><Icon name="lock" size={15} /> Setup incomplete — organizations and projects remain locked</p>
    </section>
  );

  return embedded ? content : <main className="setup-shell"><aside className="setup-sidebar"><a className="setup-brand" href="/" aria-label="FFDB home"><BrandMark /></a><nav aria-label="Instance setup"><span>Instance</span><strong>Overview</strong><strong aria-current="page">Onboarding</strong><span>Manage</span><strong>Settings</strong><strong>Audit logs</strong><strong>Users</strong></nav><div className="setup-sidebar-status"><Icon name="terminal" size={13} /><span>Control plane</span><code>{apiUrl}</code></div></aside><section className="setup-stage"><header className="setup-stage-heading"><div><span>INSTANCE SETUP</span><h1>Set up your FFDB instance</h1><p>Choose how this installation serves you, your team, or paying organizations.</p></div></header><div className="setup-stage-layout"><aside className="setup-guidance"><span>STEP 2 OF 4</span><h2>Choose your operating model</h2><p>Private and team installations do not charge organizations. Platform modes add first-class usage plans and tenant billing.</p><div><Icon name="shield" size={18} /><p><strong>Credentials stay server-side</strong><br />Payment secrets are sent directly to the API and never stored by this portal.</p></div></aside><div className="setup-primary">{content}</div><aside className="setup-preview"><span>FIRST WORKSPACE</span><h2>Ready after setup</h2><dl><div><dt>Organization</dt><dd>Your first tenant or team</dd></div><div><dt>Project</dt><dd>SQLite, auth, storage, sync, and commerce</dd></div><div><dt>Access</dt><dd>Owner-controlled</dd></div></dl><p><Icon name="lock" size={15} /> Locked until setup is verified</p></aside></div></section></main>;
}

interface InstanceData {
  readonly status: InstanceStatus;
  readonly capabilities: InstanceSetupCapabilities;
  readonly administrators: readonly Administrator[];
  readonly organizations: InstanceOrganizationPage;
  readonly users: InstanceUserPage;
  readonly exemptions: readonly BillingExemption[];
  readonly plans: readonly PlanCatalogEntry[];
}

export type InstancePanelView = "overview" | "billing" | "users";
type InstanceTask = "deployment" | "policy" | "provider" | "plans" | "organizations" | "administrators" | "users";

const instanceTasks: Readonly<Record<InstancePanelView, readonly { readonly id: InstanceTask; readonly label: string }[]>> = {
  overview: [{ id: "deployment", label: "Deployment" }, { id: "policy", label: "Organization access" }],
  billing: [{ id: "provider", label: "Provider" }, { id: "plans", label: "Plans" }, { id: "organizations", label: "Organizations" }],
  users: [{ id: "administrators", label: "Administrators" }, { id: "users", label: "All users" }],
};

export function InstancePanel({ apiUrl, client, onNotice, view = "overview" }: { readonly apiUrl: string; readonly client: FFDBClient; readonly view?: InstancePanelView; onNotice(value: string): void }) {
  const [revision, setRevision] = useState(0);
  const [organizationOffset, setOrganizationOffset] = useState(0);
  const [userOffset, setUserOffset] = useState(0);
  const [data, setData] = useState<InstanceData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reconfiguring, setReconfiguring] = useState(false);
  const [activeTask, setActiveTask] = useState<InstanceTask>(() => instanceTasks[view][0]!.id);
  const pageSize = 25;

  useEffect(() => {
    setData(null);
    setOrganizationOffset(0);
    setUserOffset(0);
    setReconfiguring(false);
    setActiveTask(instanceTasks[view][0]!.id);
  }, [view]);

  useEffect(() => {
    if (activeTask !== "deployment" && activeTask !== "provider") setReconfiguring(false);
  }, [activeTask]);

  const load = useCallback(async () => {
    setError(null);
    try {
      const [status, capabilities, administrators, organizations, users, exemptions, plans] = await Promise.all([
        client.instanceStatus(),
        client.instanceSetupStatus() as Promise<InstanceSetupCapabilities>,
        view === "users" ? client.instanceAdministrators() : Promise.resolve<readonly Administrator[]>([]),
        view === "billing" ? client.instanceOrganizations({ limit: pageSize, offset: organizationOffset }) : Promise.resolve<InstanceOrganizationPage>({ organizations: [], total: 0, limit: pageSize, offset: 0 }),
        view === "users" ? client.instanceUsers({ limit: pageSize, offset: userOffset }) : Promise.resolve<InstanceUserPage>({ users: [], total: 0, limit: pageSize, offset: 0 }),
        view === "billing" ? client.billingExemptions() : Promise.resolve<readonly BillingExemption[]>([]),
        view === "billing" ? client.instancePlans() : Promise.resolve<readonly PlanCatalogEntry[]>([]),
      ]);
      setData({ status, capabilities, administrators, organizations, users, exemptions, plans });
    } catch (cause) {
      setError(errorMessage(cause));
    }
  }, [client, organizationOffset, revision, userOffset, view]);

  useEffect(() => { void load(); }, [load]);

  const refresh = (message?: string) => {
    if (message !== undefined) onNotice(message);
    setRevision((value) => value + 1);
  };

  if (data === null && error === null) return <section className="management-panel"><div className="loading-line" /><h2>Loading instance administration</h2></section>;
  if (data === null) return <section className="management-panel"><ErrorMessage message={error ?? "Instance administration is unavailable."} retry={() => void load()} /></section>;

  const status = data.status;
  return <div className="instance-admin">
    {error === null ? null : <ErrorMessage message={error} retry={() => void load()} />}
    <section className="instance-status-band" aria-label="Instance status">
      <div><span>Deployment</span><strong>{deploymentLabel(status.deployment_mode)}</strong><small>{status.billing_enforcement_enabled ? "Tenant billing enforced" : "Tenant billing off"}</small></div>
      <div><span>Your role</span><strong>{capitalize(status.current_user_role, "User")}</strong><small>{status.administrator_count ?? 0} instance administrator{status.administrator_count === 1 ? "" : "s"}</small></div>
      <div><span>Organization access</span><strong>{policyLabel(status.organization_creation_policy)}</strong><small>Who may create a workspace</small></div>
      <div><span>Billing provider</span><strong>{status.billing_account == null ? "No provider" : capitalize(status.billing_account.status, "Pending")}</strong><small>{providerDescription(status.billing_account)}</small></div>
    </section>

    <InstanceTaskTabs activeTask={activeTask} onChange={setActiveTask} view={view} />

    {activeTask === "deployment" || activeTask === "provider" ? <section className="management-panel instance-configuration-panel instance-task-panel" id={`instance-${activeTask}-panel`} role="tabpanel" aria-labelledby={`instance-${activeTask}-tab`}>
      <div className="management-header"><div><h2>{activeTask === "provider" ? "Billing provider" : "Deployment configuration"}</h2><p>{activeTask === "provider" ? "Review tenant billing readiness and payment-provider onboarding for this installation." : "Choose whether this installation is private, collaborative, or a tenant-billed platform."}</p></div>{status.current_user_role === "owner" ? <button type="button" onClick={() => setReconfiguring((value) => !value)}>{reconfiguring ? "Close configuration" : "Edit configuration"}</button> : null}</div>
      {reconfiguring && status.current_user_role === "owner" ? <InstanceSetupWizard apiUrl={apiUrl} capabilities={data.capabilities} client={client} embedded initialMode={status.deployment_mode} initialPolicy={status.organization_creation_policy} onCancel={() => setReconfiguring(false)} onComplete={() => { setReconfiguring(false); refresh("Instance configuration updated"); }} /> : <BillingProviderSummary canConfigure={status.current_user_role === "owner"} status={status} client={client} onRefresh={refresh} />}
    </section> : null}

    {activeTask === "deployment" || activeTask === "provider" ? null : <div className="instance-task-panel" id={`instance-${activeTask}-panel`} role="tabpanel" aria-labelledby={`instance-${activeTask}-tab`}>
      {activeTask === "policy" ? <PolicyPanel client={client} policy={status.organization_creation_policy} onChanged={refresh} /> : null}
      {activeTask === "organizations" ? <OrganizationPanel billingEnforcementEnabled={status.billing_enforcement_enabled} client={client} exemptions={data.exemptions} page={data.organizations} onChanged={refresh} onPage={setOrganizationOffset} /> : null}
      {activeTask === "plans" ? <PlanCatalogPanel client={client} plans={data.plans} onChanged={refresh} /> : null}
      {activeTask === "administrators" ? <AdministratorPanel administrators={data.administrators} client={client} users={data.users.users} onChanged={refresh} /> : null}
      {activeTask === "users" ? <UserPanel client={client} page={data.users} onChanged={refresh} onPage={setUserOffset} /> : null}
    </div>}
  </div>;
}

function InstanceTaskTabs({ activeTask, onChange, view }: { readonly activeTask: InstanceTask; readonly view: InstancePanelView; onChange(task: InstanceTask): void }) {
  const label = view === "billing" ? "Instance billing tasks" : view === "users" ? "Instance user tasks" : "Instance overview tasks";
  const tasks = instanceTasks[view];
  return <nav className="instance-task-tabs" aria-label={label} role="tablist">
    {tasks.map((task, index) => <button aria-controls={`instance-${task.id}-panel`} aria-selected={activeTask === task.id} id={`instance-${task.id}-tab`} key={task.id} onClick={() => onChange(task.id)} onKeyDown={(event) => { const nextIndex = tabKeyIndex(event.key, index, tasks.length); if (nextIndex === null) return; event.preventDefault(); const next = tasks[nextIndex]!; onChange(next.id); queueMicrotask(() => globalThis.document.getElementById(`instance-${next.id}-tab`)?.focus()); }} role="tab" tabIndex={activeTask === task.id ? 0 : -1} type="button">{task.label}</button>)}
  </nav>;
}

function BillingProviderSummary({ status, client, canConfigure, onRefresh }: { readonly status: InstanceStatus; readonly client: FFDBClient; readonly canConfigure: boolean; onRefresh(message?: string): void }) {
  const [onboarding, setOnboarding] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [creatingLink, setCreatingLink] = useState(false);
  const account = status.billing_account;
  const reconnect = async () => {
    setCreatingLink(true);
    setError(null);
    try {
      const result = await client.createInstanceConnectOnboarding({ return_url: portalReturnUrl("return"), refresh_url: portalReturnUrl("refresh") }, { idempotencyKey: newIdempotencyKey("instance-connect") });
      setOnboarding(result.url);
      onRefresh("A fresh Stripe onboarding link is ready");
    } catch (cause) { setError(errorMessage(cause)); } finally { setCreatingLink(false); }
  };
  const refreshProvider = async () => {
    setRefreshing(true);
    setError(null);
    try {
      const updated = await client.refreshInstanceBilling();
      if (updated.setup_completed_at_ms === null) {
        setError("Stripe status was refreshed, but setup remains locked until charges, payouts, and the plan catalog are ready.");
        return;
      }
      onRefresh("Stripe status refreshed; the instance billing account and plan catalog are ready");
    } catch (cause) {
      setError(`Stripe status could not be refreshed. Billing has not been activated: ${errorMessage(cause)}`);
    } finally {
      setRefreshing(false);
    }
  };
  return <div className="provider-summary">
    <div><span className={account?.status === "enabled" ? "status-dot" : "status-dot attention"} /><div><strong>{account == null ? "No payment provider is required" : account.mode === "byo_keys" ? "Stripe · operator keys" : "Stripe Connect"}</strong><p>{account == null ? "Usage is still recorded for capacity planning, but organizations are never charged." : providerDescription(account)}</p></div></div>
    {account?.mode === "byo_keys" ? <p className="field-note">FFDB provisions and verifies the operator Stripe plan catalog automatically before BYO tenant billing becomes active.</p> : null}
    {account?.mode === "stripe_connect" ? <p className="field-note">After Stripe enables charges and payouts, refresh here; FFDB then provisions or repairs the connected account's plan catalog automatically.</p> : null}
    {!canConfigure ? <p className="field-note">Only the owner can change deployment or payment credentials.</p> : null}
    {account?.mode === "stripe_connect" && canConfigure ? <div className="action-row"><button disabled={refreshing || creatingLink} type="button" onClick={() => void reconnect()}>{creatingLink ? "Creating link…" : account.status === "enabled" ? "Create onboarding link" : "Continue Stripe onboarding"}</button><button disabled={refreshing || creatingLink} type="button" onClick={() => void refreshProvider()}>{refreshing ? "Refreshing…" : "Refresh Stripe status"}</button>{onboarding === null ? null : <a className="billing-redirect" href={onboarding}>Open Stripe <Icon name="external" size={15} /></a>}</div> : null}
    {error === null ? null : <div className="access-error" role="alert">{error}</div>}
  </div>;
}

function PolicyPanel({ client, policy, onChanged }: { readonly client: FFDBClient; readonly policy: OrganizationCreationPolicy; onChanged(message?: string): void }) {
  const [value, setValue] = useState(policy);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async (event: FormEvent) => { event.preventDefault(); setSaving(true); setError(null); try { await client.updateOrganizationCreationPolicy(value); onChanged("Organization access updated"); } catch (cause) { setError(errorMessage(cause)); } finally { setSaving(false); } };
  return <section className="management-panel form-panel instance-policy-panel"><div className="management-header"><div><h2>Organization access</h2><p>Choose who may create a new organization on this installation. Existing organizations and memberships are not changed.</p></div></div><form className="instance-policy-form" onSubmit={(event) => void submit(event)}><label className="field"><span>Who can create organizations?</span><select value={value} onChange={(event) => setValue(event.target.value as OrganizationCreationPolicy)}><option value="owner_only">Only the instance owner</option><option value="authenticated">Any authenticated user</option><option value="invitation_only">Users who joined by invitation</option></select></label><button className="primary-action" disabled={saving || value === policy} type="submit">{saving ? "Saving…" : "Save access policy"}</button></form>{error === null ? null : <div className="access-error" role="alert">{error}</div>}</section>;
}

function AdministratorPanel({ administrators, users, client, onChanged }: { readonly administrators: readonly Administrator[]; readonly users: readonly User[]; readonly client: FFDBClient; onChanged(message?: string): void }) {
  const candidates = users.filter((user) => user.instance_role === null);
  const [userId, setUserId] = useState("");
  const [adding, setAdding] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const grant = async (event: FormEvent) => { event.preventDefault(); if (userId === "") return; setPending("grant"); setError(null); try { await client.grantInstanceAdministrator(userId); setUserId(""); setAdding(false); onChanged("Administrator access granted"); } catch (cause) { setError(errorMessage(cause)); } finally { setPending(null); } };
  const revoke = async (administrator: Administrator) => { if (administrator.role === "owner" || !globalThis.confirm(`Remove instance administration from ${administrator.email}?`)) return; setPending(administrator.user_id); setError(null); try { await client.revokeInstanceAdministrator(administrator.user_id); onChanged("Administrator access removed"); } catch (cause) { setError(errorMessage(cause)); } finally { setPending(null); } };
  return <section className="management-panel instance-table-panel"><div className="management-header"><div><h2>Instance administrators</h2><p>Administrators manage accounts, organizations, billing plans, and exemptions. Deployment mode and provider credentials remain owner-only.</p></div>{candidates.length === 0 ? null : <button aria-expanded={adding} aria-controls="instance-add-administrator" type="button" onClick={() => setAdding((value) => !value)}>{adding ? "Cancel" : "Add administrator"}</button>}</div>{adding ? <form className="inline-admin-form" id="instance-add-administrator" onSubmit={(event) => void grant(event)}><div><strong>Grant administrator access</strong><p>Select an existing account. Access takes effect immediately and is recorded in the audit log.</p></div><label className="field"><span>User account</span><select aria-label="User to make instance administrator" value={userId} onChange={(event) => setUserId(event.target.value)} required><option value="">Select an account</option>{candidates.map((user) => <option key={user.id} value={user.id}>{user.email}</option>)}</select></label><button className="primary-action" disabled={userId === "" || pending !== null} type="submit">{pending === "grant" ? "Granting…" : "Grant access"}</button></form> : null}{error === null ? null : <div className="access-error" role="alert">{error}</div>}<DataTable empty="No instance administrators found." headings={["Account", "Role", "Granted", "Action"]} label="administrators" rows={administrators.map((administrator) => [administrator.email, <span className={`instance-status-pill is-${administrator.role}`} key={`${administrator.user_id}-role`}>{capitalize(administrator.role)}</span>, formatDate(administrator.created_at_ms), <button disabled={administrator.role === "owner" || pending !== null} key={administrator.user_id} type="button" onClick={() => void revoke(administrator)}>{administrator.role === "owner" ? "Owner protected" : pending === administrator.user_id ? "Removing…" : "Remove access…"}</button>])} />{candidates.length === 0 ? <p className="instance-inline-note">Every eligible account in this result already has instance-level access.</p> : null}</section>;
}

function OrganizationPanel({ page, exemptions, billingEnforcementEnabled, client, onPage, onChanged }: { readonly page: InstanceOrganizationPage; readonly exemptions: readonly BillingExemption[]; readonly billingEnforcementEnabled: boolean; readonly client: FFDBClient; onPage(offset: number): void; onChanged(message?: string): void }) {
  const [reason, setReason] = useState("Internal or operator-managed organization");
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const exemptionMap = useMemo(() => new Map(exemptions.map((item) => [item.organization_id, item])), [exemptions]);
  const toggleExemption = async (organization: Organization) => { const exemption = exemptionMap.get(organization.id); if (exemption !== undefined && !globalThis.confirm(`Resume billing enforcement for ${organization.name}?`)) return; setPending(`billing:${organization.id}`); setError(null); try { if (exemption === undefined) { await client.grantBillingExemption(organization.id, reason); onChanged(`${organization.name} is now exempt from instance billing`); } else { await client.revokeBillingExemption(organization.id); onChanged(`${organization.name} now follows the instance billing plan`); } } catch (cause) { setError(errorMessage(cause)); } finally { setPending(null); } };
  const toggleDisabled = async (organization: Organization) => { const verb = organization.disabled ? "Enable" : "Disable"; if (!globalThis.confirm(`${verb} organization ${organization.name}? This changes access for every project and member in it.`)) return; setPending(`status:${organization.id}`); setError(null); try { await client.setInstanceOrganizationDisabled(organization.id, !organization.disabled); onChanged(`${organization.name} ${organization.disabled ? "enabled" : "disabled"}`); } catch (cause) { setError(errorMessage(cause)); } finally { setPending(null); } };
  return <section className="management-panel instance-table-panel"><div className="management-header"><div><h2>Organizations</h2><p>Review tenant access and billing treatment. Disabling an organization blocks every project and member in it.</p></div><span className="instance-result-count">{compactNumber(page.total)} total</span></div><div className="instance-table-setting"><label className="field exemption-reason"><span>Reason for new exemptions</span><input value={reason} onChange={(event) => setReason(event.target.value)} /></label><p>This reason is stored with the exemption for future audits.</p></div>{error === null ? null : <div className="access-error" role="alert">{error}</div>}<DataTable empty="No organizations have been created on this instance." headings={["Organization", "Status", "Members", "Projects", "Billing", "Actions"]} label="organizations" rows={page.organizations.map((organization) => [<span className="entity-primary" key={organization.id}><strong>{organization.name}</strong><small>{organization.slug}</small></span>, <span className={`instance-status-pill is-${organization.disabled ? "disabled" : "enabled"}`} key={`${organization.id}-status`}>{organization.disabled ? "Disabled" : "Enabled"}</span>, compactNumber(organization.member_count), compactNumber(organization.project_count), exemptionMap.has(organization.id) ? "Exempt" : billingEnforcementEnabled ? "Plan enforced" : "Analytics only", <span className="instance-row-actions" key={organization.id}><button type="button" disabled={pending !== null || !exemptionMap.has(organization.id) && reason.trim() === ""} onClick={() => void toggleExemption(organization)}>{pending === `billing:${organization.id}` ? "Saving…" : exemptionMap.has(organization.id) ? "Resume billing…" : "Make exempt"}</button><button className="is-danger" disabled={pending !== null} type="button" onClick={() => void toggleDisabled(organization)}>{pending === `status:${organization.id}` ? "Saving…" : organization.disabled ? "Enable…" : "Disable…"}</button></span>])} /><Paginator label="organizations" limit={page.limit} offset={page.offset} total={page.total} onPage={onPage} /></section>;
}

function UserPanel({ page, client, onPage, onChanged }: { readonly page: InstanceUserPage; readonly client: FFDBClient; onPage(offset: number): void; onChanged(message?: string): void }) {
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const toggleDisabled = async (user: User) => { const verb = user.disabled ? "Enable" : "Disable"; if (!globalThis.confirm(`${verb} instance user ${user.email}?`)) return; setPending(user.id); setError(null); try { await client.setInstanceUserDisabled(user.id, !user.disabled); onChanged(`${user.email} ${user.disabled ? "enabled" : "disabled"}`); } catch (cause) { setError(errorMessage(cause)); } finally { setPending(null); } };
  return <section className="management-panel instance-table-panel"><div className="management-header"><div><h2>Instance users</h2><p>All accounts registered on this installation. Status changes apply across organizations and are recorded in the audit log.</p></div><span className="instance-result-count">{compactNumber(page.total)} total</span></div>{error === null ? null : <div className="access-error" role="alert">{error}</div>}<DataTable empty="No user accounts have registered on this instance." headings={["Account", "Status", "Email", "Organizations", "Instance role", "Joined", "Action"]} label="users" rows={page.users.map((user) => [user.email, <span className={`instance-status-pill is-${user.disabled ? "disabled" : "enabled"}`} key={`${user.id}-status`}>{user.disabled ? "Disabled" : "Enabled"}</span>, user.email_verified ? "Verified" : "Pending verification", compactNumber(user.organization_count), user.instance_role === null ? "User" : capitalize(user.instance_role), formatDate(user.created_at_ms), <button className={user.disabled ? undefined : "is-danger"} disabled={user.instance_role === "owner" || pending !== null} key={user.id} type="button" onClick={() => void toggleDisabled(user)}>{user.instance_role === "owner" ? "Owner protected" : pending === user.id ? "Saving…" : user.disabled ? "Enable…" : "Disable…"}</button>])} /><Paginator label="users" limit={page.limit} offset={page.offset} total={page.total} onPage={onPage} /></section>;
}

function PlanCatalogPanel({ plans, client, onChanged }: { readonly plans: readonly PlanCatalogEntry[]; readonly client: FFDBClient; onChanged(message?: string): void }) {
  const [selectedTier, setSelectedTier] = useState<BillingTier>(plans[0]?.tier ?? "free");
  const selected = plans.find((plan) => plan.tier === selectedTier);
  const tiers = ["free", "pay_as_you_go", "pro"] as const;
  return <section className="management-panel plan-catalog-panel"><div className="management-header"><div><h2>Plan catalog</h2><p>Configure checkout availability, included usage, and limit behavior for each organization plan.{plans.some((plan) => plan.provider_catalog_bound) ? " Stripe-bound pricing remains read-only." : ""}</p></div></div><div className="plan-tabs" role="tablist" aria-label="Instance plans">{tiers.map((tier, index) => <button aria-controls={`instance-plan-${tier}-panel`} aria-selected={selectedTier === tier} className={selectedTier === tier ? "selected" : ""} id={`instance-plan-${tier}-tab`} key={tier} onClick={() => setSelectedTier(tier)} onKeyDown={(event) => { const nextIndex = tabKeyIndex(event.key, index, tiers.length); if (nextIndex === null) return; event.preventDefault(); const next = tiers[nextIndex]!; setSelectedTier(next); queueMicrotask(() => globalThis.document.getElementById(`instance-plan-${next}-tab`)?.focus()); }} role="tab" tabIndex={selectedTier === tier ? 0 : -1} type="button">{tierLabel(tier)}</button>)}</div><div aria-labelledby={`instance-plan-${selectedTier}-tab`} id={`instance-plan-${selectedTier}-panel`} role="tabpanel"><PlanEditor key={`${selectedTier}:${selected?.updated_at_ms ?? "new"}`} client={client} plan={selected} tier={selectedTier} onChanged={onChanged} /></div></section>;
}

function PlanEditor({ tier, plan, client, onChanged }: { readonly tier: BillingTier; readonly plan: PlanCatalogEntry | undefined; readonly client: FFDBClient; onChanged(message?: string): void }) {
  const [form, setForm] = useState<PlanInput>(() => plan === undefined ? defaultPlan(tier) : editablePlan(plan));
  const [pending, setPending] = useState<"save" | "retire" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const providerCatalogBound = plan?.provider_catalog_bound === true;
  const set = <K extends keyof PlanInput>(key: K, value: PlanInput[K]) => setForm((current) => ({ ...current, [key]: value }));
  const submit = async (event: FormEvent) => { event.preventDefault(); setPending("save"); setError(null); try { await client.putInstancePlan(tier, form); onChanged(`${form.display_name} plan saved`); } catch (cause) { setError(errorMessage(cause)); } finally { setPending(null); } };
  const retire = async () => { if (!globalThis.confirm(`Retire the ${form.display_name} plan? Existing subscriptions keep their recorded tier state.`)) return; setPending("retire"); setError(null); try { await client.retireInstancePlan(tier); onChanged(`${form.display_name} plan retired`); } catch (cause) { setError(errorMessage(cause)); } finally { setPending(null); } };
  return <form className="plan-editor" onSubmit={(event) => void submit(event)}>
    {providerCatalogBound ? <p className="security-note provider-bound-note">This active plan is bound to verified immutable Stripe Prices. Billing unit, base price, currency, and metered allowances are locked here so FFDB enforcement cannot diverge from customer invoices.</p> : null}
    <section className="plan-editor-section" aria-labelledby={`plan-${tier}-identity`}><div className="plan-editor-section-heading"><h3 id={`plan-${tier}-identity`}>Plan and price</h3><p>Customer-facing plan identity and base billing model.</p></div><div className="plan-editor-fields plan-editor-fields-four"><Field label="Display name" value={form.display_name} onChange={(value) => set("display_name", value)} /><label className="field"><span>Billing unit</span><select disabled={providerCatalogBound} value={form.billing_unit} onChange={(event) => set("billing_unit", event.target.value as BillingUnit)}><option value="organization">Organization</option><option value="seat">Seat</option></select></label><OptionalNumberField disabled={providerCatalogBound} label="Base price (cents)" value={form.base_price_cents} onChange={(value) => set("base_price_cents", value)} /><Field disabled={providerCatalogBound} label="Currency" value={form.currency} onChange={(value) => set("currency", value.toLowerCase())} /></div></section>
    <section className="plan-editor-section" aria-labelledby={`plan-${tier}-allowances`}><div className="plan-editor-section-heading"><h3 id={`plan-${tier}-allowances`}>Included usage</h3><p>Monthly allowances applied before this plan reaches a limit.</p></div><div className="plan-editor-fields"><OptionalNumberField label="Project limit" value={form.project_limit} onChange={(value) => set("project_limit", value)} /><NumberField disabled={providerCatalogBound} label="Storage bytes" value={form.storage_bytes} onChange={(value) => set("storage_bytes", value)} /><NumberField disabled={providerCatalogBound} label="Monthly reads" value={form.monthly_reads} onChange={(value) => set("monthly_reads", value)} /><NumberField disabled={providerCatalogBound} label="Monthly writes" value={form.monthly_writes} onChange={(value) => set("monthly_writes", value)} /><NumberField disabled={providerCatalogBound} label="Monthly active users" value={form.monthly_active_users} onChange={(value) => set("monthly_active_users", value)} /></div></section>
    <section className="plan-editor-section" aria-labelledby={`plan-${tier}-limits`}><div className="plan-editor-section-heading"><h3 id={`plan-${tier}-limits`}>Limit behavior</h3><p>Choose whether activity continues, pauses, or becomes billable after an allowance is used.</p></div><div className="plan-editor-fields"><label className="field"><span>Reads at limit</span><select value={form.reads_at_limit} onChange={(event) => set("reads_at_limit", event.target.value as PlanInput["reads_at_limit"])}><option value="continue">Continue</option><option value="overage">Bill overage</option></select></label><label className="field"><span>Writes at limit</span><select value={form.writes_at_limit} onChange={(event) => set("writes_at_limit", event.target.value as PlanInput["writes_at_limit"])}><option value="pause">Pause</option><option value="overage">Bill overage</option></select></label><label className="field"><span>Signups at limit</span><select value={form.signups_at_limit} onChange={(event) => set("signups_at_limit", event.target.value as PlanInput["signups_at_limit"])}><option value="pause">Pause</option><option value="overage">Bill overage</option></select></label></div><div className="plan-editor-checks"><label className="check-field"><input checked={form.overage_enabled} onChange={(event) => set("overage_enabled", event.target.checked)} type="checkbox" /><span>Enable usage overages</span></label><label className="check-field"><input checked={form.requires_payment_method_for_overage} onChange={(event) => set("requires_payment_method_for_overage", event.target.checked)} type="checkbox" /><span>Require a payment method for overage</span></label><label className="check-field"><input checked={form.active} onChange={(event) => set("active", event.target.checked)} type="checkbox" /><span>Plan is available</span></label></div></section>
    {error === null ? null : <div className="access-error" role="alert">{error}</div>}
    <div className="plan-editor-actions"><button className="primary-action" disabled={pending !== null} type="submit">{pending === "save" ? "Saving…" : `Save ${tierLabel(tier)}`}</button>{plan === undefined ? null : <button className="danger-action" disabled={pending !== null} type="button" onClick={() => void retire()}>{pending === "retire" ? "Retiring…" : "Retire plan…"}</button>}</div>
  </form>;
}

function Field({ label, value, onChange, type = "text", disabled = false }: { readonly label: string; readonly value: string; readonly type?: string; readonly disabled?: boolean; onChange(value: string): void }) { return <label className="field"><span>{label}</span><input disabled={disabled} required type={type} value={value} onChange={(event) => onChange(event.target.value)} /></label>; }
function NumberField({ label, value, onChange, disabled = false }: { readonly label: string; readonly value: number; readonly disabled?: boolean; onChange(value: number): void }) { return <label className="field"><span>{label}</span><input disabled={disabled} min={0} required type="number" value={value} onChange={(event) => onChange(Number(event.target.value))} /></label>; }
function OptionalNumberField({ label, value, onChange, disabled = false }: { readonly label: string; readonly value: number | null; readonly disabled?: boolean; onChange(value: number | null): void }) { return <label className="field"><span>{label}</span><input disabled={disabled} min={0} placeholder="Unlimited / none" type="number" value={value ?? ""} onChange={(event) => onChange(event.target.value === "" ? null : Number(event.target.value))} /></label>; }

function DataTable({ headings, rows, label, empty }: { readonly headings: readonly string[]; readonly rows: readonly (readonly ReactNode[])[]; readonly label: string; readonly empty: string }) { return <ManagedTable empty={empty} headings={headings} label={label} pagination={false} rows={rows} />; }
function Paginator({ label, total, limit, offset, onPage }: { readonly label: string; readonly total: number; readonly limit: number; readonly offset: number; onPage(offset: number): void }) { if (total <= limit) return null; const start = total === 0 ? 0 : offset + 1; const end = Math.min(total, offset + limit); return <nav className="instance-pagination" aria-label={`${capitalize(label)} pagination`}><span>{start}–{end} of {total} {label}</span><div><button aria-label={`Previous page of ${label}`} disabled={offset === 0} onClick={() => onPage(Math.max(0, offset - limit))} type="button">Previous</button><button aria-label={`Next page of ${label}`} disabled={offset + limit >= total} onClick={() => onPage(offset + limit)} type="button">Next</button></div></nav>; }
function ErrorMessage({ message, retry }: { readonly message: string; retry(): void }) { return <div className="error-state" role="alert"><strong>Instance administration is unavailable</strong><span>{message}</span><button type="button" onClick={retry}>Try again</button></div>; }

function defaultPlan(tier: BillingTier): PlanInput {
  const pro = tier === "pro";
  return {
    display_name: tierLabel(tier),
    billing_unit: "organization",
    base_price_cents: pro ? 700 : tier === "free" ? 0 : null,
    currency: "usd",
    project_limit: tier === "free" ? 2 : null,
    storage_bytes: pro ? 10_000_000_000 : 1_000_000_000,
    monthly_reads: pro ? 15_000_000 : 1_000_000,
    monthly_writes: pro ? 750_000 : 50_000,
    monthly_active_users: pro ? 50_000 : 5_000,
    overage_enabled: tier !== "free",
    reads_at_limit: tier === "free" ? "continue" : "overage",
    writes_at_limit: tier === "free" ? "pause" : "overage",
    signups_at_limit: tier === "free" ? "pause" : "overage",
    requires_payment_method_for_overage: tier !== "free",
    active: true,
  };
}
function editablePlan(plan: PlanCatalogEntry): PlanInput {
  return {
    display_name: plan.display_name,
    billing_unit: plan.billing_unit,
    base_price_cents: plan.base_price_cents,
    currency: plan.currency,
    project_limit: plan.project_limit,
    storage_bytes: plan.storage_bytes,
    monthly_reads: plan.monthly_reads,
    monthly_writes: plan.monthly_writes,
    monthly_active_users: plan.monthly_active_users,
    overage_enabled: plan.overage_enabled,
    reads_at_limit: plan.reads_at_limit,
    writes_at_limit: plan.writes_at_limit,
    signups_at_limit: plan.signups_at_limit,
    requires_payment_method_for_overage: plan.requires_payment_method_for_overage,
    active: plan.active,
  };
}
function providerDescription(account: InstanceBillingAccountSummary | null | undefined): string { if (account == null) return "Not needed for this deployment"; if (account.mode === "byo_keys") return account.credentials_configured ? "Encrypted credentials configured" : "Credentials required"; if (account.status === "enabled" && account.charges_enabled && account.payouts_enabled) return "Charges and payouts enabled"; return "Stripe onboarding requires attention"; }
function tabKeyIndex(key: string, current: number, count: number): number | null { if (key === "Home") return 0; if (key === "End") return count - 1; if (key === "ArrowRight") return (current + 1) % count; if (key === "ArrowLeft") return (current - 1 + count) % count; return null; }
function deploymentLabel(mode: DeploymentMode | undefined): string { return deploymentOptions.find((option) => option.mode === mode)?.name ?? "Setup required"; }
function policyLabel(policy: OrganizationCreationPolicy | undefined): string { return policy === "owner_only" ? "Owner only" : policy === "authenticated" ? "Authenticated users" : policy === "invitation_only" ? "Invitation only" : "Setup required"; }
function tierLabel(tier: BillingTier): string { return tier === "pay_as_you_go" ? "Pay as you go" : capitalize(tier); }
function capitalize(value: string | undefined, fallback = "Unknown"): string { return value === undefined || value === "" ? fallback : value.replaceAll("_", " ").replace(/^./u, (character) => character.toUpperCase()); }
function compactNumber(value: number | undefined): string { return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(value ?? 0); }
function formatDate(value: number | undefined): string { return value === undefined ? "Unknown" : new Date(value).toLocaleDateString(); }
function portalReturnUrl(state: "return" | "refresh"): string { const url = new URL(import.meta.env.BASE_URL, globalThis.location.origin); url.searchParams.set("instance-connect", state); return url.href; }
function newIdempotencyKey(scope: string): string { return `${scope}:${globalThis.crypto.randomUUID()}`; }
function errorMessage(cause: unknown): string { return cause instanceof Error ? cause.message : "The request could not be completed."; }

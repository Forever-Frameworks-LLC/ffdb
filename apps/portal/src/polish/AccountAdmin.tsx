import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import {
  Activity,
  Check,
  CircleAlert,
  CircleCheck,
  Copy,
  CreditCard,
  ExternalLink,
  Gauge,
  HardDrive,
  KeyRound,
  LockKeyhole,
  LogOut,
  Pencil,
  Plus,
  RefreshCcw,
  Server,
  ShieldCheck,
  Trash2,
  UserRound,
  Users,
} from "lucide-react";
import {
  type ApiKeySummary,
  type DeveloperScope,
  type DeveloperSession,
  type FFDBClient,
  type OrganizationSummary,
  type PlatformBillingSummary,
  type PlatformBillingTier,
  type PlatformInvoiceSummary,
  type PlatformUsageSummary,
} from "@ffdb/client";

import {
  clearPortalProjectKey,
  forgetPortalInstance,
  persistPortalInstance,
  persistPortalProjectKeyMetadata,
  persistPortalProject,
  portalInstances,
  selectPortalInstance,
  type PortalConfiguration,
  type PortalInstanceRecord,
} from "../config.js";
import { ManagedTable } from "./ManagedTable.js";
import "./account-admin.css";

type Loadable<T> =
  | { readonly status: "loading" }
  | { readonly status: "ready"; readonly data: T }
  | { readonly status: "error"; readonly message: string };

type OrganizationRole = OrganizationSummary["role"];

export interface AccountPanelProps {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onInstanceChange(value: PortalConfiguration): void;
  onNotice(value: string): void;
  onSignedOut?(): void;
}

export function PolishedAccountPanel({ client, configuration, onInstanceChange, onNotice, onSignedOut }: AccountPanelProps) {
  const [view, setView] = useState<"profile" | "instances">("profile");
  const [session, setSession] = useState<Loadable<DeveloperSession | null>>({ status: "loading" });
  const [instances, setInstances] = useState<readonly PortalInstanceRecord[]>(() => portalInstances());
  const [instanceName, setInstanceName] = useState("");
  const [apiUrl, setApiUrl] = useState("");
  const [editingUrl, setEditingUrl] = useState<string | null>(null);
  const [editingName, setEditingName] = useState("");
  const [pending, setPending] = useState<"refresh" | "signout" | null>(null);
  const [formError, setFormError] = useState<string | null>(null);

  const loadSession = useCallback(async () => {
    setSession({ status: "loading" });
    try { setSession({ status: "ready", data: await client.developerSession() }); }
    catch (cause) { setSession({ status: "error", message: errorMessage(cause) }); }
  }, [client]);

  useEffect(() => { void loadSession(); }, [loadSession]);

  const addInstance = (event: FormEvent) => {
    event.preventDefault();
    setFormError(null);
    try {
      const parsed = new URL(apiUrl.trim());
      if (parsed.protocol !== "https:" && parsed.protocol !== "http:") throw new Error("Use an http or https URL.");
      if (parsed.username !== "" || parsed.password !== "") throw new Error("Do not put credentials in the deployment URL.");
      const name = instanceName.trim();
      if (name === "") throw new Error("Enter a name that will identify this deployment.");
      const record = { apiUrl: parsed.origin, instanceName: name };
      persistPortalInstance(record);
      setInstances(portalInstances());
      setInstanceName("");
      setApiUrl("");
      onInstanceChange(selectPortalInstance(record));
      onNotice(`${name} saved. Sign in to that deployment to continue.`);
    } catch (cause) {
      setFormError(errorMessage(cause, "Enter a valid http or https instance URL."));
    }
  };

  const renameInstance = (record: PortalInstanceRecord) => {
    const name = editingName.trim();
    if (name === "") return;
    persistPortalInstance({ ...record, instanceName: name });
    setInstances(portalInstances());
    setEditingUrl(null);
    onNotice(`${record.instanceName} renamed to ${name}.`);
    if (record.apiUrl === normalizedOrigin(configuration.apiUrl)) {
      onInstanceChange({ ...configuration, instanceName: name });
    }
  };

  const forgetInstance = (record: PortalInstanceRecord) => {
    if (record.apiUrl === normalizedOrigin(configuration.apiUrl)) return;
    if (!globalThis.confirm(`Forget ${record.instanceName}? Its browser-local sessions and project credentials will be removed. The remote deployment will not be changed.`)) return;
    forgetPortalInstance(record.apiUrl);
    setInstances(portalInstances());
    onNotice(`${record.instanceName} and its browser-local credentials were forgotten.`);
  };

  const refresh = async () => {
    setPending("refresh");
    try {
      const next = await client.refreshDeveloperSession();
      setSession({ status: "ready", data: next });
      onNotice("Developer session refreshed.");
    } catch (cause) {
      setSession({ status: "error", message: errorMessage(cause) });
    } finally { setPending(null); }
  };

  const signOut = async () => {
    if (!globalThis.confirm(`Sign out of ${configuration.instanceName ?? configuration.apiUrl}? Other saved deployments stay signed in.`)) return;
    setPending("signout");
    try {
      await client.developerSignOut();
      onNotice(`Signed out of ${configuration.instanceName ?? "this deployment"}.`);
    } catch (cause) {
      onNotice(`Signed out locally. The expired remote session could not be closed: ${errorMessage(cause)}`);
    } finally {
      setSession({ status: "ready", data: null });
      setPending(null);
      onSignedOut?.();
    }
  };

  const returnToSignIn = async () => {
    setPending("signout");
    try { await client.developerSignOut(); }
    catch { /* developerSignOut still clears the browser-local session */ }
    finally {
      setSession({ status: "ready", data: null });
      setPending(null);
      onSignedOut?.();
    }
  };

  const instanceRows = instances.map((record) => {
    const active = record.apiUrl === normalizedOrigin(configuration.apiUrl);
    const renameOpen = editingUrl === record.apiUrl;
    return [
      renameOpen ? (
        <form className="account-admin-inline-edit" key={record.apiUrl} onSubmit={(event) => { event.preventDefault(); renameInstance(record); }}>
          <label><span className="sr-only">Deployment name</span><input autoFocus value={editingName} onChange={(event) => setEditingName(event.target.value)} /></label>
          <button className="aa-icon-button" aria-label={`Save name for ${record.instanceName}`} type="submit"><Check size={14} /></button>
          <button className="aa-icon-button" aria-label="Cancel rename" type="button" onClick={() => setEditingUrl(null)}>×</button>
        </form>
      ) : <span className="aa-entity" key={record.apiUrl}><Server size={15} /><span><strong>{record.instanceName}</strong><small>{active ? "Current connection" : "Saved connection"}</small></span></span>,
      <code key={`${record.apiUrl}-origin`}>{record.apiUrl}</code>,
      <span className={`aa-status ${active ? "is-active" : ""}`} key={`${record.apiUrl}-status`}>{active ? "Active" : "Saved"}</span>,
      <div className="aa-table-actions" key={`${record.apiUrl}-actions`}>
        <button aria-label={`Rename ${record.instanceName}`} className="aa-icon-button" type="button" onClick={() => { setEditingUrl(record.apiUrl); setEditingName(record.instanceName); }}><Pencil size={14} /></button>
        <button disabled={active} type="button" onClick={() => onInstanceChange(selectPortalInstance(record))}>{active ? "Current" : "Switch"}</button>
        <button aria-label={`Forget ${record.instanceName}`} className="aa-icon-button aa-danger" disabled={active} type="button" onClick={() => forgetInstance(record)}><Trash2 size={14} /></button>
      </div>,
    ] as const;
  });

  const accountEmail = session.status === "ready" && session.data !== null ? session.data.email : "Developer account";
  return <div className="aa-page aa-account-page">
    <TaskBar
      active={view}
      idPrefix="aa-account"
      label="Account tasks"
      tabs={[{ id: "profile", label: "Profile & session", icon: <UserRound size={14} /> }, { id: "instances", label: "Deployments", icon: <Server size={14} />, count: instances.length }]}
      onChange={(next) => setView(next as typeof view)}
      context={<CompactIdentity initials={session.status === "ready" && session.data !== null ? initials(session.data.email) : "FF"} primary={accountEmail} secondary={configuration.instanceName ?? configuration.apiUrl} status={session.status === "ready" && session.data !== null ? "Signed in" : "Session unavailable"} positive={session.status === "ready" && session.data !== null} />}
    />

    {view === "profile" ? <div className="aa-tab-panel aa-profile-panel" id="aa-account-profile-panel" role="tabpanel" aria-labelledby="aa-account-profile-tab">
      <section className="aa-surface aa-session-card" aria-labelledby="developer-session-title">
        <SectionHeading icon={<ShieldCheck size={18} />} title="Developer session" description="This account session belongs to the active deployment only. Session tokens are never displayed." id="developer-session-title" />
        {session.status === "loading" ? <Loading label="Checking the current session" /> : null}
        {session.status === "error" ? <>
          <InlineError message={session.message} action="Try again" onAction={() => void loadSession()} />
          {onSignedOut === undefined ? null : <div className="aa-actions"><button className="aa-primary" disabled={pending !== null} type="button" onClick={() => void returnToSignIn()}><UserRound size={14} /> {pending === "signout" ? "Clearing session…" : "Sign in again"}</button></div>}
        </> : null}
        {session.status === "ready" && session.data === null ? <>
          <Empty icon={<UserRound size={20} />} title="Signed out" detail="Sign in again to access organizations, projects, and administrator actions on this deployment." />
          {onSignedOut === undefined ? null : <div className="aa-actions"><button className="aa-primary" disabled={pending !== null} type="button" onClick={() => void returnToSignIn()}><UserRound size={14} /> {pending === "signout" ? "Clearing session…" : "Sign in again"}</button></div>}
        </> : null}
        {session.status === "ready" && session.data !== null ? <>
          <dl className="aa-facts">
            <div><dt>Account</dt><dd>{session.data.email}</dd></div>
            <div><dt>User ID</dt><dd><code>{compactId(session.data.user_id)}</code></dd></div>
            <div><dt>Expires</dt><dd>{dateTime(session.data.expires_at_ms)}</dd></div>
            <div><dt>Time remaining</dt><dd>{relativeExpiry(session.data.expires_at_ms)}</dd></div>
          </dl>
          <div className="aa-actions">
            <button disabled={pending !== null} type="button" onClick={() => void refresh()}><RefreshCcw size={14} /> {pending === "refresh" ? "Refreshing…" : "Refresh session"}</button>
            <button className="aa-danger-button" disabled={pending !== null} type="button" onClick={() => void signOut()}><LogOut size={14} /> {pending === "signout" ? "Signing out…" : "Sign out of this deployment"}</button>
          </div>
        </> : null}
      </section>
      <section className="aa-surface aa-account-scope" aria-labelledby="account-scope-title">
        <SectionHeading icon={<LockKeyhole size={18} />} title="Current scope" description="The workspace context attached to this browser account." id="account-scope-title" />
        <dl className="aa-connection-grid aa-account-scope-grid"><div><dt>Deployment</dt><dd>{configuration.instanceName ?? configuration.apiUrl}</dd><small>{configuration.apiUrl}</small></div><div><dt>Organization</dt><dd>{configuration.organizationName}</dd><small>{configuration.organizationId ?? "No organization selected"}</small></div><div><dt>Project</dt><dd>{configuration.projectName}</dd><small>{configuration.projectId || "No project selected"}</small></div><div><dt>Project credential</dt><dd>{configuration.developerKey === undefined ? "Not configured" : "Configured"}</dd><small>Secret remains hidden</small></div></dl>
      </section>
    </div> : <div className="aa-tab-panel aa-instances-panel" id="aa-account-instances-panel" role="tabpanel" aria-labelledby="aa-account-instances-tab">
      <section className="aa-surface aa-wide" aria-labelledby="saved-instances-title">
        <SectionHeading icon={<Server size={18} />} title="Saved deployments" description="Each FFDB origin has its own account session and project credentials. Switching here never mixes authentication between servers." id="saved-instances-title" />
        <ManagedTable headings={["Deployment", "Origin", "Status", "Actions"]} rows={instanceRows} label="deployments" empty="No saved deployments." pageSizes={[5, 10, 25]} />
      </section>
      <section className="aa-surface aa-add-instance" aria-labelledby="add-instance-title">
        <SectionHeading icon={<Plus size={18} />} title="Connect another deployment" description="Add a self-hosted or managed FFDB origin. You will sign in separately after switching to it." id="add-instance-title" />
        <form className="aa-form" onSubmit={addInstance}>
          <label><span>Deployment name</span><input required value={instanceName} onChange={(event) => setInstanceName(event.target.value)} placeholder="Production US" /></label>
          <label><span>FFDB URL</span><input required type="url" value={apiUrl} onChange={(event) => setApiUrl(event.target.value)} placeholder="https://db.example.com" /></label>
          {formError === null ? null : <InlineError message={formError} />}
          <button className="aa-primary" type="submit"><Plus size={14} /> Save and connect</button>
        </form>
      </section>
    </div>}
  </div>;
}

export interface SettingsPanelProps {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onNotice(value: string): void;
  onConfiguration(value: PortalConfiguration): void;
}

interface SettingsData {
  readonly session: DeveloperSession | null;
  readonly role: OrganizationRole | null;
  readonly keys: readonly ApiKeySummary[];
}

const AVAILABLE_SCOPES: readonly { readonly value: DeveloperScope; readonly label: string; readonly group: string }[] = [
  { value: "projects_read", label: "Read projects", group: "Workspace" },
  { value: "projects_write", label: "Manage projects", group: "Workspace" },
  { value: "database_query", label: "Run database queries", group: "Database" },
  { value: "database_schema", label: "Read database schema", group: "Database" },
  { value: "database_migrate", label: "Apply migrations", group: "Database" },
  { value: "auth_manage", label: "Manage authentication", group: "Services" },
  { value: "storage_manage", label: "Manage storage", group: "Services" },
  { value: "email_manage", label: "Manage email", group: "Services" },
  { value: "commerce_manage", label: "Manage commerce", group: "Services" },
  { value: "backups_manage", label: "Manage backups", group: "Operations" },
  { value: "logs_read", label: "Read audit logs", group: "Operations" },
  { value: "keys_rotate", label: "Rotate signing keys", group: "Security" },
];

export function PolishedSettingsPanel({ client, configuration, onNotice, onConfiguration }: SettingsPanelProps) {
  const [view, setView] = useState<"keys" | "advanced">("keys");
  const [resource, setResource] = useState<Loadable<SettingsData>>({ status: "loading" });
  const [revision, setRevision] = useState(0);
  const [keyName, setKeyName] = useState("");
  const [selectedScopes, setSelectedScopes] = useState<readonly DeveloperScope[]>(["database_query", "database_schema"]);
  const [expiry, setExpiry] = useState("2592000000");
  const [issuedKey, setIssuedKey] = useState<{
    readonly name: string;
    readonly secret: string;
    readonly expiresAtMs: number | null;
  } | null>(null);
  const [copied, setCopied] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setResource({ status: "loading" });
    try {
      const [session, organizations] = await Promise.all([client.developerSession(), client.organizations()]);
      const organization = organizations.find((item) => item.id === configuration.organizationId)
        ?? organizations.find((item) => item.name === configuration.organizationName)
        ?? null;
      const keys = session === null || configuration.projectId === "" ? [] : await client.apiKeys();
      setResource({ status: "ready", data: { session, role: organization?.role ?? null, keys } });
    } catch (cause) { setResource({ status: "error", message: errorMessage(cause) }); }
  }, [client, configuration.organizationId, configuration.organizationName, configuration.projectId, revision]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => { setIssuedKey(null); setCopied(false); }, [configuration.apiUrl, configuration.projectId]);

  const canManage = resource.status === "ready" && (resource.data.role === "owner" || resource.data.role === "admin");

  const toggleScope = (scope: DeveloperScope) => {
    setSelectedScopes((current) => current.includes(scope) ? current.filter((item) => item !== scope) : [...current, scope]);
  };

  const issueKey = async (event: FormEvent) => {
    event.preventDefault();
    if (!canManage || selectedScopes.length === 0) return;
    setPending("issue"); setActionError(null); setIssuedKey(null); setCopied(false);
    try {
      const expiresAt = expiry === "never" ? null : Date.now() + Number(expiry);
      const value = await client.createApiKey({ name: keyName.trim(), scopes: selectedScopes, expires_at_ms: expiresAt });
      setIssuedKey({ name: value.name, secret: value.secret, expiresAtMs: value.expires_at_ms });
      setKeyName("");
      setRevision((value_) => value_ + 1);
      onNotice(`${value.name} issued. Copy it before dismissing the one-time secret.`);
    } catch (cause) { setActionError(errorMessage(cause)); }
    finally { setPending(null); }
  };

  const useIssuedKey = () => {
    if (issuedKey === null) return;
    client.setDeveloperKey(issuedKey.secret);
    persistPortalProject(configuration.projectId, issuedKey.secret, configuration.organizationName, configuration.organizationId, configuration.projectName, configuration.apiUrl);
    persistPortalProjectKeyMetadata(configuration.apiUrl, configuration.projectId, {
      expiresAtMs: issuedKey.expiresAtMs,
      managed: false,
    });
    onConfiguration({
      ...configuration,
      developerKey: issuedKey.secret,
      developerKeyExpiresAtMs: issuedKey.expiresAtMs,
      developerKeyManaged: false,
    });
    onNotice(`${issuedKey.name} is now the browser credential for ${configuration.projectName}.`);
  };

  const copySecret = async () => {
    if (issuedKey === null) return;
    try {
      await globalThis.navigator.clipboard.writeText(issuedKey.secret);
      setCopied(true);
      onNotice("API key copied to the clipboard.");
    } catch { setActionError("Clipboard access was blocked. Select and copy the key manually before dismissing it."); }
  };

  const revoke = async (key: ApiKeySummary) => {
    if (!canManage || !globalThis.confirm(`Revoke ${key.name}? Applications using this key will lose access immediately.`)) return;
    setPending(key.id); setActionError(null);
    try {
      await client.revokeApiKey(key.id);
      const activePrefix = configuration.developerKey?.split(".", 1)[0];
      if (activePrefix === `ffdb_dev_${key.prefix}`) {
        clearPortalProjectKey(configuration.apiUrl, configuration.projectId);
        client.setDeveloperKey(null);
        onConfiguration({
          ...configuration,
          developerKey: undefined,
          developerKeyExpiresAtMs: undefined,
          developerKeyManaged: undefined,
        });
      }
      setRevision((value) => value + 1);
      onNotice(`${key.name} revoked.`);
    } catch (cause) { setActionError(errorMessage(cause)); }
    finally { setPending(null); }
  };

  const rotateSigningKey = async () => {
    if (!canManage || !globalThis.confirm("Rotate the JWT signing key? Existing tokens remain valid only for the server-defined overlap window.")) return;
    setPending("rotate"); setActionError(null);
    try {
      const value = await client.rotateSigningKey();
      onNotice(`Signing key rotated to ${value.active_kid}.`);
    } catch (cause) { setActionError(errorMessage(cause)); }
    finally { setPending(null); }
  };

  const clearLocalCredential = () => {
    if (configuration.projectId === "" || configuration.developerKey === undefined) return;
    clearPortalProjectKey(configuration.apiUrl, configuration.projectId);
    client.setDeveloperKey(null);
    onConfiguration({
      ...configuration,
      developerKey: undefined,
      developerKeyExpiresAtMs: undefined,
      developerKeyManaged: undefined,
    });
    onNotice(`Browser credential cleared for ${configuration.projectName}. No server-side key was revoked.`);
  };

  if (resource.status === "loading") return <section className="aa-surface"><Loading label="Loading connection and key security" /></section>;
  if (resource.status === "error") return <section className="aa-surface"><InlineError message={resource.message} action="Try again" onAction={() => void load()} /></section>;
  const { session, role, keys } = resource.data;
  const roleLabel = role === null ? "No organization selected" : `${capitalize(role)} access`;
  const keyRows = keys.map((key) => [
    <span className="aa-entity" key={key.id}><KeyRound size={15} /><span><strong>{key.name}</strong><small>{key.prefix}…</small></span></span>,
    <span className="aa-scope-list" key={`${key.id}-scopes`}>{key.scopes.map(scopeLabel).join(", ")}</span>,
    dateTime(key.created_at_ms),
    key.expires_at_ms === null ? "Never" : dateTime(key.expires_at_ms),
    <span className={`aa-status ${key.revoked_at_ms === null ? "is-active" : "is-revoked"}`} key={`${key.id}-status`}>{key.revoked_at_ms === null ? "Active" : "Revoked"}</span>,
    <button className="aa-danger-link" disabled={!canManage || key.revoked_at_ms !== null || pending !== null} key={`${key.id}-action`} type="button" onClick={() => void revoke(key)}>{pending === key.id ? "Revoking…" : "Revoke…"}</button>,
  ] as const);

  return <div className="aa-page aa-settings-page">
    <TaskBar
      active={view}
      idPrefix="aa-settings"
      label="Settings tasks"
      tabs={[{ id: "keys", label: "Connection & keys", icon: <KeyRound size={14} />, count: keys.filter((key) => key.revoked_at_ms === null).length }, { id: "advanced", label: "Signing & advanced", icon: <ShieldCheck size={14} /> }]}
      onChange={(next) => setView(next as typeof view)}
      context={<CompactIdentity initials={initials(configuration.projectName)} primary={configuration.projectName} secondary={`${configuration.instanceName ?? configuration.apiUrl} · ${configuration.organizationName}`} status={roleLabel} positive={canManage} />}
    />

    {view === "keys" ? <div className="aa-tab-panel aa-settings-keys-panel" id="aa-settings-keys-panel" role="tabpanel" aria-labelledby="aa-settings-keys-tab">
      <section className="aa-surface aa-wide" aria-labelledby="connection-keys-title">
        <SectionHeading icon={<Server size={18} />} title="Connection & project keys" description="Confirm the active project, then review and revoke its least-privilege credentials." id="connection-keys-title" />
        <dl className="aa-connection-grid">
          <div><dt>Instance</dt><dd>{configuration.instanceName ?? configuration.apiUrl}</dd><small>{configuration.apiUrl}</small></div>
          <div><dt>Organization</dt><dd>{configuration.organizationName}</dd><small>{roleLabel}</small></div>
          <div><dt>Project</dt><dd>{configuration.projectName}</dd><small>{configuration.projectId || "No project selected"}</small></div>
          <div><dt>Browser project session</dt><dd>{configuration.developerKey === undefined ? "Not active" : "Active"}</dd><small>The temporary secret is intentionally hidden</small></div>
        </dl>
        <div className="aa-local-credential"><LockKeyhole size={16} /><p><strong>Temporary portal credential</strong><span>Created from your signed-in account, scoped to this project, stored only in this tab, and limited to 12 hours or the remaining account-session lifetime.</span></p><button disabled={configuration.developerKey === undefined} type="button" onClick={clearLocalCredential}>End project session</button></div>
        {!canManage ? <div className="aa-permission-note"><ShieldCheck size={16} /><span><strong>{roleLabel}</strong>API key and signing-key changes require an organization owner or administrator.</span></div> : null}
        <div className="aa-section-divider" />
        <div className="aa-subsection-heading"><div><h3 id="api-keys-title">Project API keys</h3><p>Search the full key lifecycle without exposing secret material.</p></div><span>{keys.filter((key) => key.revoked_at_ms === null).length} active</span></div>
        {configuration.projectId === "" ? <Empty icon={<KeyRound size={20} />} title="Choose a project first" detail="API keys always belong to one project." /> : <ManagedTable headings={["Key", "Scopes", "Created", "Expires", "Status", "Action"]} rows={keyRows} label="API keys" empty="No API keys have been issued for this project." pageSizes={[10, 25, 50]} />}
      </section>

      <section className="aa-surface aa-key-issuer" aria-labelledby="issue-api-key-title">
      <SectionHeading icon={<Plus size={18} />} title="Issue an API key" description="The secret is returned once. Start with only the scopes this integration needs." id="issue-api-key-title" />
      <form className="aa-form" onSubmit={(event) => void issueKey(event)}>
        <label><span>Key name</span><input disabled={!canManage || configuration.projectId === ""} required value={keyName} onChange={(event) => setKeyName(event.target.value)} placeholder="production-api" /></label>
        <label><span>Expiration</span><select disabled={!canManage || configuration.projectId === ""} value={expiry} onChange={(event) => setExpiry(event.target.value)}><option value="86400000">1 day</option><option value="604800000">7 days</option><option value="2592000000">30 days</option><option value="7776000000">90 days</option><option value="31536000000">1 year</option><option value="never">Never</option></select></label>
        <fieldset className="aa-scope-picker" disabled={!canManage || configuration.projectId === ""}><legend>Scopes</legend>{scopeGroups().map(([group, scopes]) => <div key={group}><strong>{group}</strong>{scopes.map((scope) => <label key={scope.value}><input checked={selectedScopes.includes(scope.value)} type="checkbox" onChange={() => toggleScope(scope.value)} /><span>{scope.label}</span></label>)}</div>)}</fieldset>
        <button className="aa-primary" disabled={!canManage || configuration.projectId === "" || keyName.trim() === "" || selectedScopes.length === 0 || pending !== null} type="submit">{pending === "issue" ? "Issuing…" : "Issue one-time secret"}</button>
      </form>
      {issuedKey === null ? null : <div className="aa-one-time-secret" role="alert"><div><ShieldCheck size={18} /><p><strong>Copy {issuedKey.name} now</strong><span>This secret cannot be shown again after you dismiss it or leave this page.</span></p></div><code>{issuedKey.secret}</code><div className="aa-actions"><button type="button" onClick={() => void copySecret()}>{copied ? <Check size={14} /> : <Copy size={14} />}{copied ? "Copied" : "Copy secret"}</button><button type="button" onClick={useIssuedKey}>Use in this browser</button><button type="button" onClick={() => setIssuedKey(null)}>I saved it</button></div></div>}
      </section>
    </div> : <div className="aa-tab-panel aa-settings-advanced-panel" id="aa-settings-advanced-panel" role="tabpanel" aria-labelledby="aa-settings-advanced-tab">
      <section className="aa-surface aa-security-actions" aria-labelledby="signing-key-title">
        <SectionHeading icon={<ShieldCheck size={18} />} title="Signing & advanced security" description="Review the developer session and rotate JWT signing material during a planned lifecycle event." id="signing-key-title" />
        {session === null ? <Empty icon={<CircleAlert size={20} />} title="Session unavailable" detail="Sign in again to manage this instance." /> : <dl className="aa-facts aa-security-session"><div><dt>Developer account</dt><dd>{session.email}</dd></div><div><dt>Session expires</dt><dd>{dateTime(session.expires_at_ms)}</dd></div></dl>}
        {!canManage ? <div className="aa-permission-note"><ShieldCheck size={16} /><span><strong>{roleLabel}</strong>Signing-key changes require an organization owner or administrator.</span></div> : null}
        <div className="aa-section-divider" />
        <div className="aa-risk-action"><div><strong>Rotate active signing key</strong><p>Applications should accept both current and previous key IDs during the server-defined overlap window.</p></div><button className="aa-danger-button" disabled={!canManage || pending !== null || configuration.projectId === ""} type="button" onClick={() => void rotateSigningKey()}>{pending === "rotate" ? "Rotating…" : "Rotate signing key…"}</button></div>
        <nav className="aa-reference-links" aria-label="Security references"><a href="/docs/client">Client reference</a><a href="/docs/cli">CLI reference</a><a href="/docs/security">Security guide</a></nav>
      </section>
    </div>}
    {actionError === null ? null : <div className="aa-floating-error"><InlineError message={actionError} action="Dismiss" onAction={() => setActionError(null)} /></div>}
  </div>;
}

export interface UsagePanelProps {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onNotice(value: string): void;
}

interface UsageData {
  readonly organizations: readonly OrganizationSummary[];
  readonly organization: OrganizationSummary;
  readonly summary: PlatformBillingSummary;
  readonly usage: PlatformUsageSummary;
  readonly invoices: readonly PlatformInvoiceSummary[];
}

export function PolishedUsagePanel({ client, configuration, onNotice }: UsagePanelProps) {
  const [view, setView] = useState<"current" | "billing" | "reporting">("current");
  const [organizations, setOrganizations] = useState<readonly OrganizationSummary[]>([]);
  const [organizationId, setOrganizationId] = useState(configuration.organizationId ?? "");
  const [resource, setResource] = useState<Loadable<UsageData | null>>({ status: "loading" });
  const [revision, setRevision] = useState(0);
  const [pending, setPending] = useState<PlatformBillingTier | "portal" | null>(null);
  const [redirect, setRedirect] = useState<{ readonly label: string; readonly url: string } | null>(null);

  const load = useCallback(async () => {
    setResource({ status: "loading" });
    try {
      const available = await client.organizations();
      setOrganizations(available);
      const organization = available.find((item) => item.id === organizationId)
        ?? available.find((item) => item.id === configuration.organizationId)
        ?? available.find((item) => item.name === configuration.organizationName)
        ?? available[0];
      if (organization === undefined) { setResource({ status: "ready", data: null }); return; }
      if (organizationId !== organization.id) setOrganizationId(organization.id);
      const [summary, usage, invoices] = await Promise.all([
        client.organizationBilling(organization.id),
        client.organizationUsage(organization.id),
        client.organizationInvoices(organization.id),
      ]);
      setResource({ status: "ready", data: { organizations: available, organization, summary, usage, invoices } });
    } catch (cause) { setResource({ status: "error", message: errorMessage(cause) }); }
  }, [client, configuration.organizationId, configuration.organizationName, organizationId, revision]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => { setRedirect(null); }, [organizationId]);
  useEffect(() => {
    if (resource.status === "ready" && resource.data !== null && !resource.data.summary.billing_enforcement_enabled && view !== "current") setView("current");
  }, [resource, view]);

  const checkout = async (tier: PlatformBillingTier) => {
    if (resource.status !== "ready" || resource.data === null) return;
    setPending(tier); setRedirect(null);
    try {
      const result = await client.createBillingCheckout(resource.data.organization.id, { tier }, { idempotencyKey: `portal-billing:${resource.data.organization.id}:${tier}:${globalThis.crypto.randomUUID()}` });
      setRedirect({ label: `Continue to ${tierLabel(tier)} checkout`, url: result.url });
    } catch (cause) { onNotice(errorMessage(cause)); }
    finally { setPending(null); }
  };

  const openBillingPortal = async () => {
    if (resource.status !== "ready" || resource.data === null) return;
    setPending("portal"); setRedirect(null);
    try {
      const result = await client.createBillingPortal(resource.data.organization.id, { idempotencyKey: `portal-customer:${resource.data.organization.id}:${globalThis.crypto.randomUUID()}` });
      setRedirect({ label: "Open secure billing portal", url: result.url });
    } catch (cause) { onNotice(errorMessage(cause)); }
    finally { setPending(null); }
  };

  if (resource.status === "loading") return <section className="aa-surface"><Loading label="Loading usage and billing" /></section>;
  if (resource.status === "error") return <section className="aa-surface"><InlineError message={resource.message} action="Try again" onAction={() => setRevision((value) => value + 1)} /></section>;
  if (resource.data === null) return <section className="aa-surface"><Empty icon={<Users size={20} />} title="No organization yet" detail="Create an organization before choosing a plan or reviewing metered usage." /></section>;

  const { organization, summary, usage, invoices } = resource.data;
  const canManage = organization.role === "owner" || organization.role === "admin";
  const providerReady = summary.provider_configured && summary.billing_enforcement_enabled;
  const paid = summary.tier !== "free";
  const unmetered = !summary.billing_enforcement_enabled || summary.billing_exempt;
  const usageTabs = summary.billing_enforcement_enabled
    ? [{ id: "current", label: "Current usage", icon: <Gauge size={14} /> }, { id: "billing", label: "Plan & payment", icon: <CreditCard size={14} /> }, { id: "reporting", label: "Invoices & reporting", icon: <Activity size={14} />, count: invoices.length }]
    : [{ id: "current", label: "Usage analytics", icon: <Gauge size={14} /> }];
  const invoiceRows = invoices.map((invoice) => [
    dateTime(invoice.created_at_ms),
    <span className={`aa-status ${invoice.status === "paid" ? "is-active" : ""}`} key={invoice.id}>{humanize(invoice.status)}</span>,
    money(invoice.amount_due_minor, invoice.currency),
    invoice.period_start_ms === null || invoice.period_end_ms === null ? "—" : `${shortDate(invoice.period_start_ms)} – ${shortDate(invoice.period_end_ms)}`,
    invoice.hosted_invoice_url === null ? "—" : <a className="aa-inline-link" href={invoice.hosted_invoice_url} key={invoice.id} rel="noreferrer" target="_blank">View receipt <ExternalLink size={13} /></a>,
  ] as const);

  return <div className="aa-page aa-usage-page">
    <TaskBar
      active={view}
      idPrefix="aa-usage"
      label="Usage tasks"
      tabs={usageTabs}
      onChange={(next) => setView(next as typeof view)}
      context={<div className="aa-usage-context">{organizations.length > 1 ? <label className="aa-organization-switcher"><span>Organization</span><select value={organization.id} onChange={(event) => setOrganizationId(event.target.value)}>{organizations.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label> : <CompactIdentity initials={initials(organization.name)} primary={organization.name} secondary={`${summary.billing_enforcement_enabled ? tierLabel(summary.tier) : "Private · no billing"} · ${capitalize(organization.role)}`} />}<span className={`aa-reporting-status is-${usage.reporting_status}`}>{usage.reporting_status === "healthy" ? <CircleCheck size={14} /> : <CircleAlert size={14} />} Reporting {usage.reporting_status}</span></div>}
    />

    {view === "current" ? <div className="aa-tab-panel" id="aa-usage-current-panel" role="tabpanel" aria-labelledby="aa-usage-current-tab"><section className="aa-surface aa-wide" aria-labelledby="current-usage-title">
      <SectionHeading icon={<Gauge size={18} />} title={unmetered ? "Operational usage" : "Current period"} description={`${shortDate(usage.period_start_ms)} – ${shortDate(usage.period_end_ms)} · values as of ${dateTime(usage.as_of_ms)}`} id="current-usage-title" />
      {unmetered ? <div className="aa-usage-mode-note"><ShieldCheck size={16} /><span><strong>No billing limits apply.</strong>These values are retained for capacity planning and diagnostics; they are not quotas and do not generate charges.</span></div> : null}
      <div className="aa-meter-grid">
        <UsageMeter icon={<HardDrive size={17} />} label="Storage" used={usage.storage_bytes} limit={unmetered ? undefined : summary.usage_allowance.storage_bytes} value={unmetered ? bytes(usage.storage_bytes) : `${bytes(usage.storage_bytes)} of ${bytes(summary.usage_allowance.storage_bytes)}`} />
        <UsageMeter icon={<Activity size={17} />} label="Reads" used={usage.reads} limit={unmetered ? undefined : summary.usage_allowance.monthly_reads} value={unmetered ? number(usage.reads) : `${number(usage.reads)} of ${number(summary.usage_allowance.monthly_reads)}`} />
        <UsageMeter icon={<Activity size={17} />} label="Writes" used={usage.writes} limit={unmetered ? undefined : summary.usage_allowance.monthly_writes} value={unmetered ? number(usage.writes) : `${number(usage.writes)} of ${number(summary.usage_allowance.monthly_writes)}`} />
        <UsageMeter icon={<Users size={17} />} label="Monthly active users" used={usage.monthly_active_users} limit={unmetered ? undefined : summary.usage_allowance.monthly_active_users} value={unmetered ? number(usage.monthly_active_users) : `${number(usage.monthly_active_users)} of ${number(summary.usage_allowance.monthly_active_users)}`} />
      </div>
      <div className="aa-usage-facts"><span><strong>{number(usage.storage_byte_hours)}</strong> storage byte-hours</span>{unmetered ? <><span><strong>Disabled</strong> billing enforcement</span><span><strong>Analytics</strong> tracking mode</span></> : <><span><strong>{summary.project_limit === null ? "Unlimited" : number(summary.project_limit)}</strong> project limit</span><span><strong>{summary.billing_unit === "seat" ? number(summary.seat_quantity) : "1"}</strong> {summary.billing_unit === "seat" ? "billable seats" : "organization"}</span></>}</div>
    </section></div> : null}

    {view === "billing" ? <div className="aa-tab-panel" id="aa-usage-billing-panel" role="tabpanel" aria-labelledby="aa-usage-billing-tab"><section className="aa-surface aa-wide" aria-labelledby="plans-title">
      <SectionHeading icon={<CreditCard size={18} />} title="Plan and payment" description="Choose how this organization is billed. Checkout and payment details are handled by the configured payment provider." id="plans-title" />
      <div className="aa-plan-grid">
        <PlanOption current={summary.tier === "free"} title="Free" price="Included" detail={`${summary.project_limit === null ? "Operator-defined" : number(summary.project_limit)} project allowance with no enabled overage charges.`} />
        <PlanOption current={summary.tier === "pay_as_you_go"} title="Pay as you go" price="Usage-priced" detail="Operator-configured rates for storage, reads, writes, and active users beyond the included allowance; checkout shows the current price." action="Choose pay as you go" disabled={!providerReady || !canManage || pending !== null} pending={pending === "pay_as_you_go"} onAction={() => void checkout("pay_as_you_go")} />
        <PlanOption current={summary.tier === "pro"} title="Pro" price="Instance-priced" detail="Operator-configured recurring pricing with larger allowances; checkout shows the current price before payment." action="Choose Pro" disabled={!providerReady || !canManage || pending !== null} pending={pending === "pro"} onAction={() => void checkout("pro")} />
      </div>
      {!summary.billing_enforcement_enabled ? <div className="aa-permission-note"><ShieldCheck size={16} /><span><strong>Billing is not enforced on this instance.</strong>Usage remains visible for capacity planning; an instance owner can enable organization billing.</span></div> : !summary.provider_configured ? <div className="aa-permission-note"><CircleAlert size={16} /><span><strong>Payment provider setup is incomplete.</strong>The instance owner must finish provider onboarding before a paid plan can be selected.</span></div> : !canManage ? <div className="aa-permission-note"><LockKeyhole size={16} /><span><strong>Plan changes are read-only.</strong>Only an organization owner or administrator can change billing.</span></div> : null}
      <div className="aa-actions"><button disabled={!providerReady || !canManage || !paid || pending !== null} type="button" onClick={() => void openBillingPortal()}>{pending === "portal" ? "Preparing portal…" : "Manage payment method"}</button>{summary.current_period_end_ms === null ? null : <span>Current period ends {shortDate(summary.current_period_end_ms)}{summary.cancel_at_period_end ? " · cancellation scheduled" : ""}</span>}</div>
      {redirect === null ? null : <a className="aa-redirect" href={redirect.url} rel="noreferrer" target="_blank">{redirect.label}<ExternalLink size={15} /></a>}
    </section></div> : null}

    {view === "reporting" ? <div className="aa-tab-panel" id="aa-usage-reporting-panel" role="tabpanel" aria-labelledby="aa-usage-reporting-tab"><section className="aa-surface aa-wide" aria-labelledby="invoices-title">
      <SectionHeading icon={<Activity size={18} />} title="Invoices & reporting" description="Reporting health, finalized provider invoices, and receipts for this organization." id="invoices-title" />
      <UsageExplanation summary={summary} usage={usage} />
      <div className="aa-section-divider" />
      <ManagedTable headings={["Created", "Status", "Amount", "Period", "Receipt"]} rows={invoiceRows} label="invoices" empty="No invoices have been issued for this organization." pageSizes={[10, 25, 50]} />
    </section></div> : null}
  </div>;
}

export const AccountPanel = PolishedAccountPanel;
export const SettingsPanel = PolishedSettingsPanel;
export const UsagePanel = PolishedUsagePanel;

interface TaskTab {
  readonly id: string;
  readonly label: string;
  readonly icon: React.ReactNode;
  readonly count?: number;
}

function TaskBar({ active, context, idPrefix, label, onChange, tabs }: {
  readonly active: string;
  readonly context: React.ReactNode;
  readonly idPrefix: string;
  readonly label: string;
  readonly tabs: readonly TaskTab[];
  onChange(value: string): void;
}) {
  return <header className="aa-taskbar aa-surface">
    <nav aria-label={label} className="aa-task-tabs" role="tablist">{tabs.map((tab) => <button aria-controls={`${idPrefix}-${tab.id}-panel`} aria-selected={active === tab.id} id={`${idPrefix}-${tab.id}-tab`} key={tab.id} role="tab" type="button" onClick={() => onChange(tab.id)}>{tab.icon}<span>{tab.label}</span>{tab.count === undefined ? null : <small>{tab.count}</small>}</button>)}</nav>
    <div className="aa-task-context">{context}</div>
  </header>;
}

function CompactIdentity({ initials: initialsValue, positive, primary, secondary, status }: {
  readonly initials: string;
  readonly positive?: boolean;
  readonly primary: string;
  readonly secondary: string;
  readonly status?: string;
}) {
  return <div className="aa-compact-identity"><i aria-hidden="true">{initialsValue}</i><span><strong>{primary}</strong><small>{secondary}</small></span>{status === undefined ? null : <em className={positive ? "is-positive" : ""}>{positive ? <CircleCheck size={12} /> : <CircleAlert size={12} />}{status}</em>}</div>;
}

function SectionHeading({ icon, title, description, id }: { readonly icon: React.ReactNode; readonly title: string; readonly description: string; readonly id: string }) {
  return <header className="aa-section-heading"><span className="aa-section-icon" aria-hidden="true">{icon}</span><div><h3 id={id}>{title}</h3><p>{description}</p></div></header>;
}

function Loading({ label }: { readonly label: string }) { return <div className="aa-loading" role="status"><RefreshCcw size={17} className="aa-spin" /><span>{label}…</span></div>; }

function InlineError({ message, action, onAction }: { readonly message: string; readonly action?: string; onAction?(): void }) {
  return <div className="aa-error" role="alert"><CircleAlert size={17} /><span>{message}</span>{action === undefined ? null : <button type="button" onClick={onAction}>{action}</button>}</div>;
}

function Empty({ icon, title, detail }: { readonly icon: React.ReactNode; readonly title: string; readonly detail: string }) {
  return <div className="aa-empty"><span aria-hidden="true">{icon}</span><div><strong>{title}</strong><p>{detail}</p></div></div>;
}

function UsageMeter({ icon, label, used, limit, value }: { readonly icon: React.ReactNode; readonly label: string; readonly used: number; readonly limit: number | undefined; readonly value: string }) {
  const percentage = limit === undefined || limit <= 0 ? 0 : Math.min(100, Math.max(0, (used / limit) * 100));
  return <article className={`aa-meter ${limit === undefined ? "is-unmetered" : ""}`}><header><span>{icon}{label}</span><strong>{limit === undefined ? "Tracked" : `${Math.round(percentage)}%`}</strong></header><p>{value}</p>{limit === undefined ? <span className="aa-meter-caption">No enforced allowance</span> : <div className="aa-meter-track" role="progressbar" aria-label={`${label} allowance used`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(percentage)}><span style={{ width: `${percentage}%` }} /></div>}</article>;
}

function UsageExplanation({ summary, usage }: { readonly summary: PlatformBillingSummary; readonly usage: PlatformUsageSummary }) {
  const copy = !summary.billing_enforcement_enabled || summary.billing_exempt
    ? "This organization is unmetered for billing. Usage remains visible for capacity planning."
    : summary.usage_allowance.overage_enabled
      ? "Usage above the included allowance is reported automatically for invoicing."
      : "Writes, new active users, and storage growth stop at the included limits; reads remain available.";
  return <div className={`aa-reporting-note is-${usage.reporting_status}`}><Gauge size={16} /><p><strong>{copy}</strong><span>{usage.reporting_status === "healthy" ? `Last successful report ${usage.reporting_last_success_ms === null ? "is not available" : dateTime(usage.reporting_last_success_ms)}.` : `Reporting is ${usage.reporting_status}. Billable writes pause if reporting reaches blocked status.`}</span></p></div>;
}

function PlanOption({ current, title, price, detail, action, disabled, pending, onAction }: { readonly current: boolean; readonly title: string; readonly price: string; readonly detail: string; readonly action?: string; readonly disabled?: boolean; readonly pending?: boolean; onAction?(): void }) {
  return <article className={`aa-plan ${current ? "is-current" : ""}`}><header><span>{current ? "Current plan" : "Available plan"}</span>{current ? <CircleCheck size={17} /> : null}</header><h4>{title}</h4><strong>{price}</strong><p>{detail}</p>{action === undefined ? null : <button className={current ? "" : "aa-primary"} disabled={disabled || current} type="button" onClick={onAction}>{current ? "Current plan" : pending ? "Preparing checkout…" : action}</button>}</article>;
}

function scopeGroups(): readonly [string, readonly (typeof AVAILABLE_SCOPES)[number][]][] {
  return [...new Set(AVAILABLE_SCOPES.map((item) => item.group))].map((group) => [group, AVAILABLE_SCOPES.filter((item) => item.group === group)] as const);
}

function scopeLabel(value: DeveloperScope): string { return AVAILABLE_SCOPES.find((item) => item.value === value)?.label ?? humanize(value); }
function tierLabel(value: PlatformBillingTier): string { return value === "pay_as_you_go" ? "Pay as you go" : capitalize(value); }
function humanize(value: string): string { return value.replaceAll("_", " "); }
function capitalize(value: string): string { return value === "" ? value : `${value[0]?.toLocaleUpperCase()}${value.slice(1)}`; }
function initials(email: string): string { return email.split("@")[0]?.split(/[._-]/u).slice(0, 2).map((part) => part[0]?.toLocaleUpperCase()).join("") || "FF"; }
function compactId(value: string): string { return value.length <= 20 ? value : `${value.slice(0, 9)}…${value.slice(-7)}`; }
function normalizedOrigin(value: string): string { try { return new URL(value).origin; } catch { return value.replace(/\/$/u, ""); } }
function dateTime(value: number): string { return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(value); }
function shortDate(value: number): string { return new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(value); }
function relativeExpiry(value: number): string { const minutes = Math.max(0, Math.floor((value - Date.now()) / 60_000)); if (minutes < 1) return "Expired"; if (minutes < 60) return `${minutes} min`; const hours = Math.floor(minutes / 60); if (hours < 48) return `${hours} hr`; return `${Math.floor(hours / 24)} days`; }
function number(value: number): string { return new Intl.NumberFormat(undefined, { notation: value >= 10_000 ? "compact" : "standard", maximumFractionDigits: 1 }).format(value); }
function bytes(value: number): string { if (value === 0) return "0 B"; const base = 1_000; const units = ["B", "KB", "MB", "GB", "TB"]; const index = Math.min(Math.floor(Math.log(value) / Math.log(base)), units.length - 1); return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value / (base ** index))} ${units[index]}`; }
function money(value: number, currency: string): string { return new Intl.NumberFormat(undefined, { style: "currency", currency: currency.toUpperCase() }).format(value / 100); }
function errorMessage(cause: unknown, fallback = "The request could not be completed."): string { return cause instanceof Error && cause.message.trim() !== "" ? cause.message : fallback; }

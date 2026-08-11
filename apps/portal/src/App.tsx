import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";

import {
  FFDBClient,
  FFDBError,
  type ApiKeySummary,
  type AuditLogEntry,
  type AuthSettings,
  type AuthUser,
  type BackupSummary,
  type DeveloperSession,
  type EmailTemplateArtifactInput,
  type EmailTemplateVersion,
  type InstanceStatus,
  type MigrationSummary,
  type PlatformBillingSummary,
  type PlatformBillingTier,
  type PlatformInvoiceSummary,
  type PlatformUsageSummary,
  type PolicyDefinition,
  type QueryResult,
  type SchemaSnapshot,
  type StorageBucket,
} from "@ffdb/client";

import {
  clearPortalProjectKey,
  createPortalClient,
  issuePortalProjectCredential,
  persistPortalProjectKeyMetadata,
  persistPortalInstance,
  persistPortalProject,
  PORTAL_PROJECT_CREDENTIAL_REFRESH_LEAD_MS,
  portalProjectKey,
  portalProjectKeyMetadata,
  portalInstances,
  portalConfiguration,
  selectPortalInstance,
  type PortalConfiguration,
} from "./config.js";
import { navigationGroups, pathForRoute, routeFromLocation, type PortalRoute } from "./data.js";
import { Icon, type IconName } from "./icons.js";
import { InstancePanel, InstanceSetupWizard } from "./Instance.js";
import { CommercePanel } from "./Commerce.js";
import { BrandMark } from "./ui.js";
import { ManagedTable } from "./polish/ManagedTable.js";
import { AuthRoute, SyncRoute, type AuthRouteTab } from "./polish/AuthSync.js";
import {
  ActivityPanel as PolishedActivityPanel,
  DatabasePanel as PolishedDatabasePanel,
  MigrationsPanel,
  SqlEditorPanel,
} from "./polish/DatabaseActivity.js";
import {
  OverviewPanel as ProductionOverviewPanel,
  WorkspacePanel as ProductionWorkspacePanel,
} from "./polish/OverviewWorkspace.js";
import {
  AccountPanel as ProductionAccountPanel,
  SettingsPanel as ProductionSettingsPanel,
  UsagePanel as ProductionUsagePanel,
} from "./polish/AccountAdmin.js";
import {
  BackupsPanel as ProductionBackupsPanel,
  EmailPanel as ProductionEmailPanel,
  PoliciesPanel as ProductionPoliciesPanel,
  StoragePanel as ProductionStoragePanel,
} from "./polish/OperateRoutes.js";
import { ObservabilityPanel } from "./polish/Observability.js";
import { InstanceUpdatesPanel } from "./polish/InstanceUpdates.js";
import { ConnectPanel } from "./polish/Connect.js";

export interface AppProps {
  readonly client?: FFDBClient;
  readonly configuration?: PortalConfiguration;
}

export function App({ client: suppliedClient, configuration: suppliedConfiguration }: AppProps = {}) {
  const initialConfiguration = useMemo(() => suppliedConfiguration ?? portalConfiguration(), [suppliedConfiguration]);
  const [configuration, setConfiguration] = useState(initialConfiguration);
  const client = useMemo(
    () => suppliedClient ?? createPortalClient(configuration),
    [configuration, suppliedClient],
  );
  const [selected, setSelected] = useState<PortalRoute>(() => routeFromLocation(globalThis.location?.pathname ?? "") ?? (initialConfiguration.projectId === "" ? "Projects" : "Overview"));
  const [authInitialTab, setAuthInitialTab] = useState<AuthRouteTab>("users");
  const [sqlDraft, setSqlDraft] = useState("SELECT sqlite_version() AS version");
  const [createOpen, setCreateOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [developerAccess, setDeveloperAccess] = useState<DeveloperSession | null | undefined>(undefined);
  const [instanceSetup, setInstanceSetup] = useState<Awaited<ReturnType<FFDBClient["instanceSetupStatus"]>> | null | undefined>(undefined);
  const [instanceStatus, setInstanceStatus] = useState<InstanceStatus | null | undefined>(undefined);
  const [hostUpdateAvailable, setHostUpdateAvailable] = useState(false);
  const [mobileNavigationOpen, setMobileNavigationOpen] = useState(false);
  const [projectCredentialError, setProjectCredentialError] = useState<string | null>(null);
  const [projectCredentialReady, setProjectCredentialReady] = useState<{
    readonly apiUrl: string;
    readonly projectId: string;
    readonly developerKey: string;
  } | null>(null);
  const [projectCredentialRevision, setProjectCredentialRevision] = useState(0);

  useEffect(() => {
    let current = true;
    setDeveloperAccess(undefined);
    setInstanceSetup(undefined);
    void Promise.all([
      client.developerSession().catch(() => null),
      client.instanceSetupStatus().catch(() => null),
    ])
      .then(([session, setup]) => {
        if (current) {
          setDeveloperAccess(session);
          setInstanceSetup(setup);
        }
      })
      .catch(() => {
        if (current) {
          setDeveloperAccess(null);
          setInstanceSetup(null);
        }
      });
    return () => {
      current = false;
    };
  }, [client]);

  useEffect(() => {
    if (developerAccess === null || developerAccess === undefined || instanceSetup?.setup_required === true) {
      setInstanceStatus(null);
      return;
    }
    let current = true;
    setInstanceStatus(undefined);
    void client.instanceStatus().then(
      (status) => { if (current) setInstanceStatus(status); },
      () => { if (current) setInstanceStatus(null); },
    );
    return () => { current = false; };
  }, [client, developerAccess, instanceSetup?.setup_required]);

  useEffect(() => {
    if (!isInstanceAdministrator(instanceStatus)) {
      setHostUpdateAvailable(false);
      return;
    }
    let current = true;
    void client.hostUpdateStatus({ retry: false }).then(
      (value) => { if (current) setHostUpdateAvailable(value.supported && value.update_available); },
      () => { if (current) setHostUpdateAvailable(false); },
    );
    return () => { current = false; };
  }, [client, instanceStatus]);

  useEffect(() => {
    if (developerAccess === null || developerAccess === undefined || instanceSetup?.setup_required === true || configuration.projectId === "") {
      setProjectCredentialReady(null);
      setProjectCredentialError(null);
      return;
    }
    let current = true;
    let refreshTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
    const projectId = configuration.projectId;
    const apiUrl = configuration.apiUrl;
    setProjectCredentialReady(null);
    setProjectCredentialError(null);
    client.setProjectId(projectId);
    void (async () => {
      let developerKey = configuration.developerKey;
      let metadata = configuration.developerKeyManaged === undefined
        ? portalProjectKeyMetadata(apiUrl, projectId)
        : {
          managed: configuration.developerKeyManaged,
          expiresAtMs: configuration.developerKeyExpiresAtMs ?? null,
        };

      if (developerKey !== undefined) {
        client.setDeveloperKey(developerKey);
        try {
          await client.schema({ retry: false });
        } catch (cause) {
          if (isRejectedPortalProjectCredential(cause)) {
            clearPortalProjectKey(apiUrl, projectId);
            client.setDeveloperKey(null);
            developerKey = undefined;
            metadata = undefined;
          }
        }
      }

      if (developerKey !== undefined && metadata === undefined) {
        try {
          const tokenPrefix = developerKey.split(".", 1)[0];
          const matchingKey = (await client.apiKeys({ retry: false }))
            .find((key) => `ffdb_dev_${key.prefix}` === tokenPrefix && key.revoked_at_ms === null);
          if (matchingKey !== undefined) {
            metadata = {
              expiresAtMs: matchingKey.expires_at_ms,
              managed: matchingKey.name === "portal-session",
            };
            persistPortalProjectKeyMetadata(apiUrl, projectId, metadata);
          }
        } catch {
          // A valid explicitly supplied key may belong to a role that cannot list
          // project keys. Keep using it without claiming that the portal manages it.
        }
      }

      const shouldRefresh = developerKey === undefined
        || metadata?.managed === true
          && metadata.expiresAtMs !== null
          && metadata.expiresAtMs <= Date.now() + PORTAL_PROJECT_CREDENTIAL_REFRESH_LEAD_MS;

      if (shouldRefresh) {
        const credential = await issuePortalProjectCredential(client);
        developerKey = credential.secret;
        metadata = { expiresAtMs: credential.expiresAtMs, managed: credential.managed };
        client.setDeveloperKey(credential.secret);
        persistPortalProject(
          projectId,
          credential.secret,
          configuration.organizationName,
          configuration.organizationId,
          configuration.projectName,
          apiUrl,
        );
        persistPortalProjectKeyMetadata(apiUrl, projectId, metadata);
        if (current) {
          setConfiguration((value) => value.projectId === projectId && value.apiUrl === apiUrl
            ? {
              ...value,
              developerKey: credential.secret,
              developerKeyExpiresAtMs: credential.expiresAtMs,
              developerKeyManaged: true,
            }
            : value);
        }
      } else if (
        current
        && metadata !== undefined
        && (configuration.developerKeyExpiresAtMs !== metadata.expiresAtMs
          || configuration.developerKeyManaged !== metadata.managed)
      ) {
        setConfiguration((value) => value.projectId === projectId && value.apiUrl === apiUrl
          ? {
            ...value,
            developerKeyExpiresAtMs: metadata?.expiresAtMs,
            developerKeyManaged: metadata?.managed,
          }
          : value);
      }

      if (!current || developerKey === undefined) return;
      setProjectCredentialReady({ apiUrl, projectId, developerKey });
      if (metadata?.managed === true && metadata.expiresAtMs !== null) {
        const delay = Math.max(
          1_000,
          metadata.expiresAtMs - Date.now() - PORTAL_PROJECT_CREDENTIAL_REFRESH_LEAD_MS,
        );
        refreshTimer = globalThis.setTimeout(
          () => setProjectCredentialRevision((value) => value + 1),
          Math.min(delay, 2_147_483_647),
        );
      }
    })().catch((cause) => {
      if (!current) return;
      setProjectCredentialReady(null);
      setProjectCredentialError(errorMessage(cause));
    });
    return () => {
      current = false;
      if (refreshTimer !== undefined) globalThis.clearTimeout(refreshTimer);
    };
  }, [
    client,
    configuration.apiUrl,
    configuration.developerKey,
    configuration.developerKeyExpiresAtMs,
    configuration.developerKeyManaged,
    configuration.organizationId,
    configuration.organizationName,
    configuration.projectId,
    configuration.projectName,
    developerAccess,
    instanceSetup?.setup_required,
    projectCredentialRevision,
  ]);

  useEffect(() => {
    const restoreRoute = () => {
      const route = routeFromLocation(globalThis.location.pathname);
      if (route !== null) setSelected(route);
    };
    globalThis.addEventListener("popstate", restoreRoute);
    return () => globalThis.removeEventListener("popstate", restoreRoute);
  }, []);

  const open = (route: PortalRoute, authTab: AuthRouteTab = "users") => {
    if (route === "Auth") setAuthInitialTab(authTab);
    setSelected(route);
    setCreateOpen(false);
    setMobileNavigationOpen(false);
    const path = pathForRoute(route, configuration.projectId, configuration.organizationId);
    if (globalThis.location.pathname !== path) globalThis.history.pushState({}, "", path);
  };

  const returnToRequiredSetup = () => {
    setInstanceSetup((current) => current === null || current === undefined ? current : { ...current, setup_required: true });
    setSelected("Instance");
    setCreateOpen(false);
    setMobileNavigationOpen(false);
    globalThis.history.replaceState({}, "", "/app/setup/instance");
  };

  if (developerAccess === undefined || instanceSetup === undefined) return <PortalAccessLoading />;

  if (instanceSetup?.bootstrap_available === true) {
    return <FirstRunOwnerScreen
      apiUrl={configuration.apiUrl}
      client={client}
      onBootstrapped={(session) => {
        setDeveloperAccess(session);
        setInstanceSetup({ ...instanceSetup, bootstrap_available: false, setup_required: true });
      }}
    />;
  }

  if (developerAccess === null) {
    return (
      <DeveloperAccessScreen
        apiUrl={configuration.apiUrl}
        client={client}
        onSignedIn={(session) => {
          setDeveloperAccess(session);
          setSelected("Projects");
        }}
      />
    );
  }

  if (instanceSetup?.setup_required === true && developerAccess !== null) {
    return <InstanceSetupWizard
      apiUrl={configuration.apiUrl}
      capabilities={instanceSetup}
      client={client}
      onComplete={(status) => {
        if (status.setup_completed_at_ms === null) return;
        setInstanceSetup({ ...instanceSetup, bootstrap_available: false, setup_required: false });
        setSelected("Projects");
        globalThis.history.replaceState({}, "", "/app/organizations/current/projects");
      }}
    />;
  }

  const canAdministerInstance = isInstanceAdministrator(instanceStatus);
  const instanceAuthorizationPending = developerAccess !== null && instanceStatus === undefined;
  const projectCredentialIsReady = configuration.developerKey !== undefined
    && projectCredentialReady?.apiUrl === configuration.apiUrl
    && projectCredentialReady.projectId === configuration.projectId
    && projectCredentialReady.developerKey === configuration.developerKey;

  return (
    <div className="app-shell">
      <Sidebar client={client} configuration={configuration} instanceStatus={instanceStatus} selected={selected} canAdministerInstance={canAdministerInstance} hostUpdateAvailable={hostUpdateAvailable} onConfiguration={setConfiguration} onInstanceChange={(next) => { setConfiguration(next); setDeveloperAccess(undefined); setInstanceSetup(undefined); setInstanceStatus(undefined); }} onNotice={setNotice} onSelect={open} />
      <div className="app-body">
        <ProjectBar
          client={client}
          configuration={configuration}
          createOpen={createOpen}
          instanceStatus={instanceStatus}
          mobileNavigationOpen={mobileNavigationOpen}
          onConfiguration={setConfiguration}
          onInstanceChange={(next) => {
            setConfiguration(next);
            setDeveloperAccess(undefined);
            setInstanceSetup(undefined);
            setInstanceStatus(undefined);
          }}
          onToggleNavigation={() => setMobileNavigationOpen((value) => !value)}
          onToggleCreate={() => setCreateOpen((value) => !value)}
          onOpen={open}
          onNotice={setNotice}
        />
        <main className={selected === "SQL Editor" || selected === "Database" || selected === "Observability" || selected === "Migrations" ? "main-content main-content--workbench" : "main-content"}>
          {isInstanceAdministrationRoute(selected) && instanceAuthorizationPending ? <InstanceAuthorizationLoading /> : isInstanceAdministrationRoute(selected) && !canAdministerInstance ? <InstanceAccessDenied /> : configuration.projectId === "" && !(["Projects", "Members", "Usage", "Instance", "Instance Billing", "Instance Users", "Updates", "Settings", "Account"] as readonly PortalRoute[]).includes(selected) ? <ConfigurationRequired /> : requiresProjectCredential(selected) && projectCredentialError !== null ? <ProjectCredentialUnavailable detail={projectCredentialError} onRetry={() => setProjectCredentialRevision((value) => value + 1)} /> : requiresProjectCredential(selected) && !projectCredentialIsReady ? <ProjectCredentialLoading /> : (
            <RoutePanel
              route={selected}
              authInitialTab={authInitialTab}
              client={client}
              configuration={configuration}
              sqlDraft={sqlDraft}
              onSqlDraft={setSqlDraft}
              onOpen={open}
              onNotice={setNotice}
              onConfiguration={setConfiguration}
              onSetupRequired={returnToRequiredSetup}
              onAuthenticationRequired={() => setDeveloperAccess(null)}
              canAdministerInstance={canAdministerInstance}
              onHostUpdateAvailability={setHostUpdateAvailable}
            />
          )}
        </main>
      </div>
      {notice === null ? null : (
        <div className="command-toast" role="status">
          <span className="status-dot" />{notice}
          <button type="button" aria-label="Dismiss" onClick={() => setNotice(null)}>×</button>
        </div>
      )}
      {mobileNavigationOpen ? <div className="mobile-navigation-drawer"><button className="mobile-drawer-close" type="button" aria-label="Close navigation" onClick={() => setMobileNavigationOpen(false)}>×</button><Sidebar client={client} configuration={configuration} instanceStatus={instanceStatus} selected={selected} canAdministerInstance={canAdministerInstance} hostUpdateAvailable={hostUpdateAvailable} onConfiguration={setConfiguration} onInstanceChange={(next) => { setConfiguration(next); setDeveloperAccess(undefined); setInstanceSetup(undefined); setInstanceStatus(undefined); }} onNotice={setNotice} onSelect={open} /></div> : null}
    </div>
  );
}

function PortalAccessLoading() {
  return (
    <main className="access-shell">
      <div className="access-loading" role="status">
        <span className="access-spinner" />
        <span>Checking session…</span>
      </div>
    </main>
  );
}

function ProjectCredentialLoading() {
  return <ResourceState resource={{ status: "loading" }} title="Opening secure project session" />;
}

function ProjectCredentialUnavailable({ detail, onRetry }: { readonly detail: string; onRetry(): void }) {
  return <section className="management-panel setup-panel" role="alert"><Icon name="shield" size={28} /><h2>Project access could not be established</h2><p>{detail}</p><button type="button" onClick={onRetry}><Icon name="sync" size={15} />Try again</button></section>;
}

function FirstRunOwnerScreen({ apiUrl, client, onBootstrapped }: {
  readonly apiUrl: string;
  readonly client: FFDBClient;
  onBootstrapped(session: DeveloperSession): void;
}) {
  const [bootstrapToken, setBootstrapToken] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (password !== confirmation) {
      setError("Passwords do not match.");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      onBootstrapped(await client.developerBootstrap(bootstrapToken, email, password));
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setSubmitting(false);
      setPassword("");
      setConfirmation("");
    }
  };

  return (
    <main className="access-shell first-run-shell">
      <PortalThemeToggle className="access-theme-toggle" />
      <div className="access-content first-run-content">
        <a className="access-brand" href="/" aria-label="FFDB home"><BrandMark /></a>
        <section className="access-card" aria-labelledby="first-run-heading">
          <div className="access-card-heading">
            <h1 id="first-run-heading">Create the instance owner</h1>
            <p>The first account controls this FFDB installation, its organizations, users, plans, and billing.</p>
          </div>
          <SetupProgress active={1} />
          <form onSubmit={(event) => void submit(event)}>
            <AccessField label="One-time bootstrap token" autoComplete="off" icon="lock" type="password" value={bootstrapToken} onChange={setBootstrapToken} />
            <AccessField label="Owner email" autoComplete="email" icon="mail" type="email" value={email} onChange={setEmail} />
            <AccessField label="Password" autoComplete="new-password" icon="lock" minLength={12} type="password" value={password} onChange={setPassword} />
            <AccessField label="Confirm password" autoComplete="new-password" icon="lock" minLength={12} type="password" value={confirmation} onChange={setConfirmation} />
            {error === null ? null : <div className="access-error" role="alert">{error}</div>}
            <button className="access-submit" disabled={submitting} type="submit">{submitting ? "Creating owner…" : "Create owner"}</button>
          </form>
          <p className="access-help">The token is read from your installation environment. It is checked once and is never stored by this portal.</p>
        </section>
        <ControlPlaneEndpoint apiUrl={apiUrl} />
      </div>
    </main>
  );
}

function AccessField({ label, icon, value, onChange, type, autoComplete, minLength }: {
  readonly label: string;
  readonly icon: IconName;
  readonly value: string;
  readonly type: string;
  readonly autoComplete: string;
  readonly minLength?: number;
  onChange(value: string): void;
}) {
  return (
    <label className="access-field">
      <span>{label}</span>
      <span className="access-input-wrap">
        <Icon name={icon} size={17} />
        <input autoComplete={autoComplete} minLength={minLength} onChange={(event) => onChange(event.target.value)} required type={type} value={value} />
      </span>
    </label>
  );
}

function ControlPlaneEndpoint({ apiUrl }: { readonly apiUrl: string }) {
  return <div className="access-endpoint"><Icon name="terminal" size={13} /><span>Control plane</span><code>{apiUrl}</code></div>;
}

function DeveloperAccessScreen({ apiUrl, client, onSignedIn }: {
  readonly apiUrl: string;
  readonly client: FFDBClient;
  onSignedIn(session: DeveloperSession): void;
}) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setSubmitting(true);
    setError(null);
    try {
      onSignedIn(await client.developerSignIn(email, password));
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setSubmitting(false);
      setPassword("");
    }
  };

  return (
    <main className="access-shell">
      <PortalThemeToggle className="access-theme-toggle" />
      <div className="access-content">
        <a className="access-brand" href="/" aria-label="FFDB home"><BrandMark /></a>
        <section className="access-card" aria-labelledby="developer-access-heading">
          <div className="access-card-heading">
            <h1 id="developer-access-heading">Welcome back</h1>
            <p>Sign in to this FFDB deployment, then choose an organization and project.</p>
          </div>
          <form onSubmit={(event) => void submit(event)}>
            <label className="access-field">
              <span>Email</span>
              <span className="access-input-wrap">
                <Icon name="mail" size={17} />
                <input
                  autoComplete="email"
                  name="email"
                  onChange={(event) => setEmail(event.target.value)}
                  placeholder="you@example.com"
                  required
                  type="email"
                  value={email}
                />
              </span>
            </label>
            <label className="access-field">
              <span>Password</span>
              <span className="access-input-wrap">
                <Icon name="lock" size={17} />
                <input
                  autoComplete="current-password"
                  minLength={12}
                  name="password"
                  onChange={(event) => setPassword(event.target.value)}
                  placeholder="••••••••••••"
                  required
                  type={showPassword ? "text" : "password"}
                  value={password}
                />
                <button
                  aria-label={showPassword ? "Hide password" : "Show password"}
                  className="password-toggle"
                  onClick={() => setShowPassword((value) => !value)}
                  type="button"
                >
                  <Icon name={showPassword ? "eyeOff" : "eye"} size={16} />
                </button>
              </span>
            </label>
            {error === null ? null : <div className="access-error" role="alert">{error}</div>}
            <button className="access-submit" disabled={submitting} type="submit">
              {submitting ? "Signing in…" : "Sign in"}
            </button>
            <a className="access-secondary" href="/openapi.json">Open API setup reference</a>
          </form>
          <p className="access-help">
            First installation? Use the one-time bootstrap token to create the owner, then sign in here.
          </p>
        </section>
        <ControlPlaneEndpoint apiUrl={apiUrl} />
        <p className="access-footer">© {new Date().getFullYear()} Forever Frameworks LLC. All rights reserved.</p>
      </div>
    </main>
  );
}

function RoutePanel(props: {
  readonly route: PortalRoute;
  readonly authInitialTab: AuthRouteTab;
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  readonly sqlDraft: string;
  onSqlDraft(value: string): void;
  onOpen(value: PortalRoute, authTab?: AuthRouteTab): void;
  onNotice(value: string): void;
  onConfiguration(value: PortalConfiguration): void;
  onSetupRequired(): void;
  onAuthenticationRequired(): void;
  readonly canAdministerInstance: boolean;
  onHostUpdateAvailability(available: boolean): void;
}) {
  switch (props.route) {
    case "Overview": return <ProductionOverviewPanel client={props.client} configuration={props.configuration} onNavigate={props.onOpen} />;
    case "Connect": return <ConnectPanel configuration={props.configuration} onNotice={props.onNotice} onOpenAuth={() => props.onOpen("Auth", "policy")} />;
    case "Projects": return <ProductionWorkspacePanel view="projects" client={props.client} configuration={props.configuration} onConfiguration={props.onConfiguration} onNotice={props.onNotice} onNavigate={props.onOpen} onSetupRequired={props.onSetupRequired} />;
    case "Members": return <ProductionWorkspacePanel view="members" client={props.client} configuration={props.configuration} onConfiguration={props.onConfiguration} onNotice={props.onNotice} onNavigate={props.onOpen} onSetupRequired={props.onSetupRequired} />;
    case "SQL Editor": return <SqlEditorPanel client={props.client} sql={props.sqlDraft} onSqlChange={props.onSqlDraft} />;
    case "Migrations": return <MigrationsPanel client={props.client} />;
    case "Database": return <PolishedDatabasePanel client={props.client} onOpenMigrations={() => props.onOpen("Migrations")} />;
    case "Policies": return <ProductionPoliciesPanel client={props.client} onEdit={(sql) => { props.onSqlDraft(sql); props.onOpen("SQL Editor"); }} />;
    case "Auth": return <AuthRoute client={props.client} initialTab={props.authInitialTab} onNotice={props.onNotice} />;
    case "Storage": return <ProductionStoragePanel client={props.client} onManageSession={() => props.onOpen("Auth")} />;
    case "Sync": return <SyncRoute client={props.client} onManageSession={() => props.onOpen("Auth")} onNotice={props.onNotice} />;
    case "Email": return <ProductionEmailPanel client={props.client} />;
    case "Activity": return <PolishedActivityPanel client={props.client} />;
    case "Observability": return <ObservabilityPanel client={props.client} canViewInstance={props.canAdministerInstance} />;
    case "Backups": return <ProductionBackupsPanel client={props.client} onNotice={props.onNotice} />;
    case "Usage": return <ProductionUsagePanel client={props.client} configuration={props.configuration} onNotice={props.onNotice} />;
    case "Products": return <CommercePanel client={props.client} onNotice={props.onNotice} view="products" />;
    case "Orders": return <CommercePanel client={props.client} onNotice={props.onNotice} view="orders" />;
    case "Subscriptions": return <CommercePanel client={props.client} onNotice={props.onNotice} view="subscriptions" />;
    case "Settings": return <ProductionSettingsPanel client={props.client} configuration={props.configuration} onNotice={props.onNotice} onConfiguration={props.onConfiguration} />;
    case "Instance": return <InstancePanel apiUrl={props.configuration.apiUrl} client={props.client} onNotice={props.onNotice} view="overview" />;
    case "Instance Billing": return <InstancePanel apiUrl={props.configuration.apiUrl} client={props.client} onNotice={props.onNotice} view="billing" />;
    case "Instance Users": return <InstancePanel apiUrl={props.configuration.apiUrl} client={props.client} onNotice={props.onNotice} view="users" />;
    case "Updates": return <InstanceUpdatesPanel client={props.client} onNotice={props.onNotice} onUpdateAvailability={props.onHostUpdateAvailability} />;
    case "Account": return <ProductionAccountPanel client={props.client} configuration={props.configuration} onInstanceChange={props.onConfiguration} onNotice={props.onNotice} onSignedOut={props.onAuthenticationRequired} />;
    default: return <EmptyState title="Unknown section" detail="Choose a project section from the navigation." />;
  }
}

function ConfigurationRequired() {
  return (
    <section className="management-panel setup-panel">
      <Icon name="settings" size={28} />
      <h2>Connect this portal to FFDB</h2>
      <p>Open Settings, sign in with a platform developer account, and choose a project.</p>
      <p className="security-note">Project keys are accepted at runtime and kept in sessionStorage. They are never embedded in production JavaScript bundles.</p>
    </section>
  );
}

function InstanceAuthorizationLoading() {
  return <section className="management-panel setup-panel" role="status"><span className="access-spinner" /><h2>Checking instance access</h2><p>FFDB is verifying your instance administrator role.</p></section>;
}

function InstanceAccessDenied() {
  return <section className="management-panel setup-panel"><Icon name="shield" size={28} /><h2>Instance administration unavailable</h2><p>Your signed-in account is not an instance owner or administrator.</p><p className="security-note">Organization and project access does not grant instance-wide billing, user, or deployment administration.</p></section>;
}

function isInstanceAdministrationRoute(route: PortalRoute): boolean {
  return route === "Instance" || route === "Instance Billing" || route === "Instance Users" || route === "Updates";
}

function requiresProjectCredential(route: PortalRoute): boolean {
  return ([
    "Overview",
    "SQL Editor",
    "Migrations",
    "Database",
    "Policies",
    "Auth",
    "Storage",
    "Sync",
    "Email",
    "Activity",
    "Backups",
  ] as readonly PortalRoute[]).includes(route);
}

function isInstanceAdministrator(status: InstanceStatus | null | undefined): status is InstanceStatus {
  return status?.current_user_role === "owner" || status?.current_user_role === "admin";
}

function Sidebar({ client, configuration, instanceStatus, selected, canAdministerInstance, hostUpdateAvailable, onConfiguration, onInstanceChange, onNotice, onSelect }: {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  readonly instanceStatus: InstanceStatus | null | undefined;
  readonly selected: PortalRoute;
  readonly canAdministerInstance: boolean;
  readonly hostUpdateAvailable: boolean;
  onConfiguration(value: PortalConfiguration): void;
  onInstanceChange(value: PortalConfiguration): void;
  onNotice(value: string): void;
  onSelect(value: PortalRoute): void;
}) {
  const activeGroup = navigationGroups.find((group) => group.items.some((item) => item.label === selected))?.label;
  const [expandedGroups, setExpandedGroups] = useState<ReadonlySet<string>>(
    () => new Set(["Workspace", ...(activeGroup === undefined ? [] : [activeGroup])]),
  );

  useEffect(() => {
    if (activeGroup === undefined) return;
    setExpandedGroups((current) => current.has(activeGroup) ? current : new Set([...current, activeGroup]));
  }, [activeGroup]);

  const toggleGroup = (label: string) => {
    setExpandedGroups((current) => {
      const next = new Set(current);
      if (next.has(label)) next.delete(label);
      else next.add(label);
      return next;
    });
  };

  return (
    <aside className="sidebar">
      <a className="brand" href="/" aria-label="FFDB home"><BrandMark /></a>
      <ScopeSelectors client={client} configuration={configuration} instanceStatus={instanceStatus} onConfiguration={onConfiguration} onInstanceChange={onInstanceChange} onNotice={onNotice} />
      <nav className="primary-navigation" aria-label="Project">
        {navigationGroups.map((group) => {
          const items = group.items.filter((item) => (!item.administratorOnly || canAdministerInstance) && (!item.requiresProject || configuration.projectId !== ""));
          if (items.length === 0) return null;
          const expanded = expandedGroups.has(group.label);
          return <div className="nav-group" key={group.label}><button className="nav-group-trigger" type="button" aria-expanded={expanded} onClick={() => toggleGroup(group.label)}><span>{group.label}</span><Icon name="chevronDown" size={12} /></button><div className={expanded ? "nav-group-items" : "nav-group-items is-collapsed"}>{items.map((item) => <button
              type="button"
              className={selected === item.label ? "nav-item selected" : "nav-item"}
              aria-current={selected === item.label ? "page" : undefined}
              key={item.label}
              onClick={() => onSelect(item.label)}
            >
              <Icon name={item.icon} /><span>{item.label === "Instance Billing" ? "Billing" : item.label === "Instance Users" ? "Users" : item.label}</span>{item.label === "Updates" && hostUpdateAvailable ? <span className="nav-update-badge" aria-label="Update available">New</span> : null}
            </button>)}</div></div>;
        })}
      </nav>
      <button className="profile" type="button" aria-label="Open account" onClick={() => onSelect("Account")}>
        <span className="profile-avatar">SC</span>
        <span className="profile-copy"><strong>Account</strong><small>{accountContextLabel(instanceStatus)}</small></span>
        <Icon name="chevronDown" size={14} />
      </button>
    </aside>
  );
}

function ScopeSelectors({ client, configuration, instanceStatus, onConfiguration, onInstanceChange, onNotice, mobile = false }: {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  readonly instanceStatus: InstanceStatus | null | undefined;
  readonly mobile?: boolean;
  onConfiguration(value: PortalConfiguration): void;
  onInstanceChange(value: PortalConfiguration): void;
  onNotice(value: string): void;
}) {
  const [organizations, setOrganizations] = useState<Awaited<ReturnType<FFDBClient["organizations"]>>>([]);
  const [projects, setProjects] = useState<Awaited<ReturnType<FFDBClient["projects"]>>>([]);
  const [mobileScopeOpen, setMobileScopeOpen] = useState(false);
  const mobileScopeTriggerRef = useRef<HTMLButtonElement>(null);
  const mobileScopeDialogRef = useRef<HTMLDivElement>(null);
  const activeOrganizationId = configuration.organizationId ?? organizations.find((organization) => organization.name === configuration.organizationName)?.id ?? "";

  const closeMobileScope = () => {
    setMobileScopeOpen(false);
    globalThis.setTimeout(() => mobileScopeTriggerRef.current?.focus(), 0);
  };

  useEffect(() => {
    if (!mobileScopeOpen) return;
    const focusTimer = globalThis.setTimeout(() => mobileScopeDialogRef.current?.querySelector("select")?.focus(), 0);
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      closeMobileScope();
    };
    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node) || mobileScopeDialogRef.current?.contains(target) || mobileScopeTriggerRef.current?.contains(target)) return;
      closeMobileScope();
    };
    globalThis.document.addEventListener("keydown", handleKeyDown);
    globalThis.document.addEventListener("pointerdown", handlePointerDown);
    return () => {
      globalThis.clearTimeout(focusTimer);
      globalThis.document.removeEventListener("keydown", handleKeyDown);
      globalThis.document.removeEventListener("pointerdown", handlePointerDown);
    };
  }, [mobileScopeOpen]);

  useEffect(() => {
    let current = true;
    void client.organizations().then((values) => { if (current) setOrganizations(values); }, () => { if (current) setOrganizations([]); });
    return () => { current = false; };
  }, [client]);

  useEffect(() => {
    if (activeOrganizationId === "") { setProjects([]); return; }
    let current = true;
    void client.projects(activeOrganizationId).then((values) => { if (current) setProjects(values); }, () => { if (current) setProjects([]); });
    return () => { current = false; };
  }, [activeOrganizationId, client]);

  const selectOrganization = (organizationId: string) => {
    const organization = organizations.find((value) => value.id === organizationId);
    if (organization === undefined) return;
    persistPortalProject("", undefined, organization.name, organization.id, "Choose a project", configuration.apiUrl);
    client.setProjectId("");
    client.setDeveloperKey(null);
    onConfiguration({
      ...configuration,
      organizationId: organization.id,
      organizationName: organization.name,
      projectId: "",
      projectName: "Choose a project",
      developerKey: undefined,
      developerKeyExpiresAtMs: undefined,
      developerKeyManaged: undefined,
    });
    onNotice(`${organization.name} is now the active organization`);
  };

  const selectProject = (projectId: string) => {
    const project = projects.find((value) => value.id === projectId);
    const organization = organizations.find((value) => value.id === project?.organization_id);
    if (project === undefined) return;
    const developerKey = portalProjectKey(configuration.apiUrl, project.id);
    client.setProjectId(project.id);
    client.setDeveloperKey(developerKey ?? null);
    persistPortalProject(project.id, undefined, organization?.name, organization?.id, project.name, configuration.apiUrl);
    onConfiguration({ ...configuration, organizationId: organization?.id, organizationName: organization?.name ?? configuration.organizationName, projectId: project.id, projectName: project.name, developerKey });
    onNotice(developerKey === undefined ? `${project.name} is active; opening a secure project session` : `${project.name} is now the active project`);
    if (mobile) closeMobileScope();
  };

  const instances = portalInstances();
  const activeInstanceName = instances.find((instance) => instance.apiUrl === configuration.apiUrl)?.instanceName ?? configuration.instanceName ?? "FFDB";
  const activeOrganizationName = organizations.find((organization) => organization.id === activeOrganizationId)?.name ?? "Choose an organization";
  const activeProjectName = projects.find((project) => project.id === configuration.projectId)?.name ?? "Choose a project";
  if (mobile) return <div className="mobile-scope-switcher">
    <button
      ref={mobileScopeTriggerRef}
      type="button"
      className="mobile-scope-trail"
      aria-label="Change deployment, organization, and project"
      aria-expanded={mobileScopeOpen}
      aria-haspopup="dialog"
      onClick={() => setMobileScopeOpen((current) => !current)}
    >
      <span className="mobile-scope-crumb" title={activeInstanceName}><Icon name="database" size={15} /><strong>{activeInstanceName}</strong></span>
      <Icon name="chevronRight" size={12} />
      <span className="mobile-scope-crumb" title={activeOrganizationName}><strong>{activeOrganizationName}</strong></span>
      <Icon name="chevronRight" size={12} />
      <span className="mobile-scope-crumb" title={activeProjectName}><strong>{activeProjectName}</strong></span>
      <Icon name="chevronDown" size={13} />
    </button>
    {mobileScopeOpen ? <div className="mobile-scope-popover" role="dialog" aria-label="Change deployment, organization, and project" ref={mobileScopeDialogRef}>
      <header><div><strong>Change context</strong><span>Choose where portal actions run.</span></div><button type="button" aria-label="Close context switcher" onClick={closeMobileScope}>×</button></header>
      <div className="mobile-scope-fields">
        <label><span>Deployment</span><select aria-label="Mobile active FFDB deployment" value={configuration.apiUrl} onChange={(event) => { const instance = instances.find((value) => value.apiUrl === event.target.value); if (instance !== undefined) { onInstanceChange(selectPortalInstance(instance)); closeMobileScope(); } }}>{instances.map((instance) => <option key={instance.apiUrl} value={instance.apiUrl}>{instance.instanceName}</option>)}</select><small>{deploymentContextLabel(instanceStatus, instances.length)}</small></label>
        <label><span>Organization</span><select aria-label="Mobile active organization" value={activeOrganizationId} onChange={(event) => selectOrganization(event.target.value)}><option value="">Choose an organization</option>{organizations.map((organization) => <option key={organization.id} value={organization.id}>{organization.name}</option>)}</select></label>
        <label><span>Project</span><select aria-label="Mobile active project" value={configuration.projectId} disabled={activeOrganizationId === ""} onChange={(event) => selectProject(event.target.value)}><option value="">Choose a project</option>{projects.map((project) => <option key={project.id} value={project.id}>{project.name}</option>)}</select></label>
      </div>
    </div> : null}
  </div>;
  return <div className={mobile ? "scope-selectors mobile" : "scope-selectors"}>
    <label className="scope-selector"><span>Deployment</span><span className="scope-select-wrap"><Icon name="database" size={18} /><strong>{activeInstanceName}</strong><Icon name="chevronDown" size={13} /></span><small>{deploymentContextLabel(instanceStatus, instances.length)}</small><select className="scope-native-select" aria-label="Active FFDB deployment" value={configuration.apiUrl} onChange={(event) => { const instance = instances.find((value) => value.apiUrl === event.target.value); if (instance !== undefined) onInstanceChange(selectPortalInstance(instance)); }}>{instances.map((instance) => <option key={instance.apiUrl} value={instance.apiUrl}>{instance.instanceName}</option>)}</select></label>
    <label className="scope-selector"><span>Organization</span><span className="scope-select-wrap"><Icon name="archive" size={18} /><strong>{activeOrganizationName}</strong><Icon name="chevronDown" size={13} /></span><select className="scope-native-select" aria-label="Active organization" value={activeOrganizationId} onChange={(event) => selectOrganization(event.target.value)}><option value="">Choose an organization</option>{organizations.map((organization) => <option key={organization.id} value={organization.id}>{organization.name}</option>)}</select></label>
    <label className="scope-selector"><span>Project</span><span className="scope-select-wrap"><Icon name="database" size={18} /><strong>{activeProjectName}</strong><Icon name="chevronDown" size={13} /></span><select className="scope-native-select" aria-label="Active project" value={configuration.projectId} disabled={activeOrganizationId === ""} onChange={(event) => selectProject(event.target.value)}><option value="">Choose a project</option>{projects.map((project) => <option key={project.id} value={project.id}>{project.name}</option>)}</select></label>
  </div>;
}

function deploymentContextLabel(status: InstanceStatus | null | undefined, deploymentCount: number): string {
  const session = deploymentCount === 1 ? "isolated session" : `${deploymentCount} saved · isolated sessions`;
  if (status === undefined) return `Checking access · ${session}`;
  if (status === null) return `Developer account · ${session}`;
  if (status.deployment_mode === "private") return `Private mode · ${session}`;
  if (status.deployment_mode === "team") return `Team mode · ${session}`;
  if (status.deployment_mode === "platform_byo" || status.deployment_mode === "platform_connect") return `Platform mode · ${session}`;
  return `Setup required · ${session}`;
}

function accountContextLabel(status: InstanceStatus | null | undefined): string {
  if (status === undefined) return "Checking instance role…";
  if (status === null) return "Developer account";
  if (status.deployment_mode === "private") return "Self-hosted owner · Private mode";
  if (status.deployment_mode === "team") return "Self-hosted admin · Team mode";
  if (status.deployment_mode === "platform_byo" || status.deployment_mode === "platform_connect") return "Platform administrator · Billing enabled";
  return "Setup required";
}

function ProjectBar({ client, configuration, createOpen, instanceStatus, mobileNavigationOpen, onConfiguration, onInstanceChange, onToggleNavigation, onToggleCreate, onOpen, onNotice }: {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  readonly createOpen: boolean;
  readonly instanceStatus: InstanceStatus | null | undefined;
  readonly mobileNavigationOpen: boolean;
  onConfiguration(value: PortalConfiguration): void;
  onInstanceChange(value: PortalConfiguration): void;
  onToggleNavigation(): void;
  onToggleCreate(): void;
  onOpen(value: PortalRoute): void;
  onNotice(value: string): void;
}) {
  return (
    <>
      <header className="project-bar">
        <button type="button" className="mobile-menu-button" aria-expanded={mobileNavigationOpen} aria-label="Open navigation" onClick={onToggleNavigation}><Icon name="list" size={21} /></button>
        <a className="mobile-brand" href="/app" aria-label="FFDB portal"><BrandMark compact /></a>
        <div className="project-tools">
          <a href="/docs/"><Icon name="book" size={18} />Docs</a>
          <button type="button" onClick={() => void copyCli(configuration, onNotice)}><Icon name="terminal" size={18} />CLI</button>
          <PortalThemeToggle />
          <button type="button" aria-label="Open account" onClick={() => onOpen("Account")}><Icon name="users" size={18} /><span className="project-tool-label">Account</span></button>
          <div className="create-wrap">
            <button type="button" className="create-button" aria-expanded={createOpen} onClick={onToggleCreate}><Icon name="plus" size={18} /><span className="project-create-label">Create</span><span className="create-divider" /><Icon name="chevronDown" size={15} /></button>
            {createOpen ? (
              <div className="create-menu" role="menu">
                {([ ["New project", "Projects", "database"], ["New migration", "Migrations", "code"], ["Storage bucket", "Storage", "archive"], ["Invite member", "Members", "users"] ] as const).map(([label, route, icon]) => (
                  <button type="button" role="menuitem" key={label} onClick={() => onOpen(route)}><Icon name={icon} size={17} />{label}</button>
                ))}
              </div>
            ) : null}
          </div>
        </div>
      </header>
      <div className="mobile-scope-context">
        <ScopeSelectors mobile client={client} configuration={configuration} instanceStatus={instanceStatus} onConfiguration={onConfiguration} onInstanceChange={onInstanceChange} onNotice={onNotice} />
      </div>
    </>
  );
}

type PortalTheme = "light" | "dark";

function currentPortalTheme(): PortalTheme {
  const stored = (() => {
    try { return globalThis.localStorage.getItem("ffdb.portal.theme"); } catch { return null; }
  })();
  if (stored === "light" || stored === "dark") return stored;
  const painted = globalThis.document?.documentElement.dataset.theme;
  if (painted === "light" || painted === "dark") return painted;
  return typeof globalThis.matchMedia === "function" && globalThis.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

function applyPortalTheme(theme: PortalTheme) {
  const root = globalThis.document?.documentElement;
  if (root === undefined) return;
  root.dataset.theme = theme;
  root.classList.toggle("dark", theme === "dark");
  root.style.colorScheme = theme;
  globalThis.document.querySelector('meta[name="theme-color"]')?.setAttribute("content", theme === "dark" ? "#121214" : "#f6f6f7");
  try { globalThis.localStorage.setItem("ffdb.portal.theme", theme); } catch { /* Storage can be unavailable in hardened browsers. */ }
}

function PortalThemeToggle({ className = "" }: { readonly className?: string }) {
  const [theme, setTheme] = useState<PortalTheme>(currentPortalTheme);
  useEffect(() => applyPortalTheme(theme), [theme]);
  const next = theme === "dark" ? "light" : "dark";
  return <button className={["portal-theme-toggle", className].filter(Boolean).join(" ")} type="button" aria-label={`Switch to ${next} mode`} title={`Switch to ${next} mode`} onClick={() => setTheme(next)}><Icon name={theme === "dark" ? "sun" : "moon"} size={17} /></button>;
}

async function copyCli(configuration: PortalConfiguration, notify: (value: string) => void) {
  const command = `ffdb --url ${configuration.apiUrl} --project ${configuration.projectId} schema`;
  try {
    await globalThis.navigator.clipboard.writeText(command);
    notify("CLI command copied");
  } catch {
    notify(command);
  }
}

interface OverviewData {
  readonly health: string | null;
  readonly ready: string | null;
  readonly metrics: string | null;
  readonly schema: SchemaSnapshot;
  readonly policies: readonly PolicyDefinition[];
  readonly logs: readonly AuditLogEntry[];
  readonly backups: readonly BackupSummary[];
  readonly buckets: readonly StorageBucket[];
  readonly availability: {
    readonly health: boolean;
    readonly readiness: boolean;
    readonly metrics: boolean;
    readonly activity: boolean;
    readonly backups: boolean;
    readonly storage: boolean;
  };
  readonly unavailable: readonly string[];
}

async function optionalOverviewRead<T>(label: string, read: () => Promise<T>, fallback: T): Promise<{
  readonly label: string;
  readonly value: T;
  readonly available: boolean;
}> {
  try {
    return { label, value: await read(), available: true };
  } catch {
    return { label, value: fallback, available: false };
  }
}

function OverviewPanel({ client, configuration, onOpen, onNotice }: {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onOpen(value: string): void;
  onNotice(value: string): void;
}) {
  const loader = useCallback(async (): Promise<OverviewData> => {
    const [health, ready, metrics, schema, policies, logs, backups, buckets] = await Promise.all([
      optionalOverviewRead<Awaited<ReturnType<FFDBClient["health"]>> | null>("health", () => client.health(), null),
      optionalOverviewRead<Awaited<ReturnType<FFDBClient["readiness"]>> | null>("readiness", () => client.readiness(), null),
      optionalOverviewRead<string | null>("metrics", () => client.metrics(), null),
      client.schema(), client.policies(),
      optionalOverviewRead("activity", () => client.logs({ limit: 25 }), [] as readonly AuditLogEntry[]),
      optionalOverviewRead("backups", () => client.backups(), [] as readonly BackupSummary[]),
      optionalOverviewRead("storage", () => client.storage.buckets(), [] as readonly StorageBucket[]),
    ]);
    const normalizedLogs = Array.isArray(logs.value) ? logs.value : [];
    const normalizedBackups = Array.isArray(backups.value) ? backups.value : [];
    const normalizedBuckets = Array.isArray(buckets.value) ? buckets.value : [];
    const optional = [
      health,
      ready,
      metrics,
      { ...logs, available: logs.available && Array.isArray(logs.value) },
      { ...backups, available: backups.available && Array.isArray(backups.value) },
      { ...buckets, available: buckets.available && Array.isArray(buckets.value) },
    ];
    return {
      health: health.value?.status ?? null,
      ready: ready.value?.status ?? null,
      metrics: metrics.value,
      schema,
      policies,
      logs: normalizedLogs,
      backups: normalizedBackups,
      buckets: normalizedBuckets,
      availability: {
        health: health.available,
        readiness: ready.available,
        metrics: metrics.available,
        activity: logs.available && Array.isArray(logs.value),
        backups: backups.available && Array.isArray(backups.value),
        storage: buckets.available && Array.isArray(buckets.value),
      },
      unavailable: optional.filter((value) => !value.available).map((value) => value.label),
    };
  }, [client]);
  const resource = useResource(loader);
  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading project status" />;
  const data = resource.data;
  return (
    <>
      {data.unavailable.length === 0 ? null : (
        <div className="route-notice" role="status">
          <Icon name="shield" size={18} />
          <div><strong>Some overview details are unavailable.</strong><span>Check service availability and access for {data.unavailable.join(", ")}.</span></div>
          <button type="button" onClick={() => onOpen("Settings")}>Review access</button>
        </div>
      )}
      <div className="dashboard-grid">
        <HealthRail data={data} />
        <UsageSummary data={data} />
        <SyncChart available={data.availability.activity} logs={data.logs} />
        <QuickActions onOpen={onOpen} onNotice={onNotice} />
        <RecentActivity available={data.availability.activity} logs={data.logs} onOpen={onOpen} onNotice={onNotice} />
        <ProjectTelemetry data={data} configuration={configuration} />
      </div>
    </>
  );
}

function HealthRail({ data }: { readonly data: OverviewData }) {
  const requests = overviewMetric(data, "ffdb_http_requests_total");
  const inflight = overviewMetric(data, "ffdb_http_requests_inflight");
  const services = [
    { name: "API", icon: "terminal" as const, state: data.health === null ? "Health unavailable" : `Health: ${data.health}`, available: data.availability.health, values: [["Requests", compactNumber(requests)], ["Inflight", inflight]] },
    { name: "Database schema", icon: "database" as const, state: "Schema reported", available: true, values: [["Schema", `v${data.schema.version}`], ["Tables", String(data.schema.tables.length)]] },
    { name: "Readiness", icon: "shield" as const, state: data.ready === null ? "Readiness unavailable" : `State: ${data.ready}`, available: data.availability.readiness, values: [["Policies", String(data.policies.length)], ["Source", "API"]] },
    { name: "Object storage", icon: "archive" as const, state: data.availability.storage ? "Inventory reported" : "Inventory unavailable", available: data.availability.storage, values: [["Buckets", data.availability.storage ? String(data.buckets.length) : "Unavailable"], ["Backups", data.availability.backups ? String(data.backups.length) : "Unavailable"]] },
    { name: "Email audit activity", icon: "mail" as const, state: data.availability.activity ? "Audit data reported" : "Audit data unavailable", available: data.availability.activity, values: [["Events", data.availability.activity ? String(data.logs.filter((entry) => entry.action.includes("email")).length) : "Unavailable"], ["Source", data.availability.activity ? "Audit log" : "Unavailable"]] },
  ] as const;
  return (
    <section className="panel health-panel" aria-label="Project signals">
      {services.map((service) => (
        <article className="health-service" key={service.name}>
          <div className="health-name"><span className={service.available ? "signal-icon available" : "signal-icon unavailable"}><Icon name={service.icon} size={13} /></span><strong>{service.name}</strong></div>
          <p>{service.state}</p>
          <dl>{service.values.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>
        </article>
      ))}
    </section>
  );
}

function UsageSummary({ data }: { readonly data: OverviewData }) {
  const requestValue = overviewMetric(data, "ffdb_http_requests_total");
  const rows: readonly { readonly icon: IconName; readonly label: string; readonly value: string }[] = [
    { icon: "database", label: "Database", value: `${data.schema.tables.length} tables · v${data.schema.version}` },
    { icon: "archive", label: "Storage", value: data.availability.storage ? `${data.buckets.length} buckets` : "Unavailable" },
    { icon: "sync", label: "Backups", value: data.availability.backups ? `${data.backups.length} retained` : "Unavailable" },
    { icon: "list", label: "Requests", value: compactNumber(requestValue) },
  ];
  return (
    <section className="panel usage-panel" aria-label="Project usage">
      {rows.map((row) => (
        <div className="usage-row" key={row.label}>
          <Icon name={row.icon} />
          <span>{row.label}</span><strong>{row.value}</strong>
        </div>
      ))}
    </section>
  );
}

type SyncKind = "pull" | "push" | "resnapshot";

function SyncChart({ available, logs }: { readonly available: boolean; readonly logs: readonly AuditLogEntry[] }) {
  const [visible, setVisible] = useState<Readonly<Record<SyncKind, boolean>>>({ pull: true, push: true, resnapshot: true });
  const series = useMemo(() => syncActivitySeries(logs), [logs]);
  const toggle = (kind: SyncKind) => setVisible((current) => ({ ...current, [kind]: !current[kind] }));
  const labels = ["00:00", "04:00", "08:00", "12:00", "16:00", "20:00", "Now"];
  if (!available) return <section className="panel sync-panel"><div className="panel-header chart-header"><h2>Sync operations</h2></div><div className="activity-empty">Sync activity unavailable because audit logs could not be read.</div></section>;
  return (
    <section className="panel sync-panel">
      <div className="panel-header chart-header">
        <h2>Sync operations</h2>
        <div className="chart-controls">
          {(["pull", "push", "resnapshot"] as const).map((kind) => (
            <button type="button" className={`legend legend-${kind}`} aria-pressed={visible[kind]} key={kind} onClick={() => toggle(kind)}>
              <span>{visible[kind] ? "✓" : ""}</span>{kind[0]?.toUpperCase()}{kind.slice(1)}
            </button>
          ))}
          <span className="control-rule" />
          <button type="button" className="time-control">Last 24 hours<Icon name="chevronDown" size={14} /></button>
          <button type="button" className="more-control" aria-label="More chart options">⋮</button>
        </div>
      </div>
      <div className="chart-wrap">
        <svg className="chart" viewBox="0 0 920 190" role="img" aria-label="Sync activity during the last 24 hours" preserveAspectRatio="none">
          {[24, 62, 100, 138].map((y) => <line className="grid-line" x1="43" x2="904" y1={y} y2={y} key={`h-${y}`} />)}
          {[43, 186, 329, 472, 615, 758, 904].map((x) => <line className="grid-line" x1={x} x2={x} y1="18" y2="151" key={`v-${x}`} />)}
          <text x="7" y="28">max</text><text x="17" y="105">mid</text><text x="26" y="151">0</text>
          {visible.pull ? <polyline className="chart-line pull" points={series.pull} /> : null}
          {visible.push ? <polyline className="chart-line push" points={series.push} /> : null}
          {visible.resnapshot ? <polyline className="chart-line resnapshot" points={series.resnapshot} /> : null}
          {labels.map((label, index) => <text x={43 + index * 143} y="178" textAnchor={index === 0 ? "start" : index === labels.length - 1 ? "end" : "middle"} key={label}>{label}</text>)}
        </svg>
      </div>
    </section>
  );
}

function syncActivitySeries(logs: readonly AuditLogEntry[]): Readonly<Record<SyncKind, string>> {
  const now = Date.now();
  const buckets: Record<SyncKind, number[]> = { pull: Array(13).fill(0), push: Array(13).fill(0), resnapshot: Array(13).fill(0) };
  for (const entry of logs) {
    const age = now - entry.occurred_at_ms;
    if (age < 0 || age > 24 * 60 * 60 * 1_000) continue;
    const bucket = Math.min(12, 12 - Math.floor(age / (2 * 60 * 60 * 1_000)));
    const action = entry.action.toLowerCase();
    const kind: SyncKind | null = action.includes("resnapshot") ? "resnapshot" : action.includes("sync.pull") ? "pull" : action.includes("sync.push") ? "push" : null;
    if (kind !== null) buckets[kind][bucket] = (buckets[kind][bucket] ?? 0) + 1;
  }
  const max = Math.max(1, ...buckets.pull, ...buckets.push, ...buckets.resnapshot);
  const points = (values: readonly number[]) => values.map((value, index) => `${43 + index * 71.75},${148 - (value / max) * 112}`).join(" ");
  return { pull: points(buckets.pull), push: points(buckets.push), resnapshot: points(buckets.resnapshot) };
}

function QuickActions({ onOpen, onNotice }: { onOpen(value: string): void; onNotice(value: string): void }) {
  const actions: readonly { readonly label: string; readonly route: string; readonly icon: IconName; readonly primary?: boolean }[] = [
    { label: "Run SQL", route: "SQL Editor", icon: "terminal", primary: true },
    { label: "New migration", route: "Migrations", icon: "code" },
    { label: "Create bucket", route: "Storage", icon: "archive" },
    { label: "Invite member", route: "Members", icon: "users" },
  ];
  return (
    <section className="panel quick-panel">
      <h2>Quick actions</h2>
      {actions.map((action) => (
        <button type="button" className={action.primary === true ? "quick-action primary" : "quick-action"} key={action.label} onClick={() => { onNotice(`${action.label} opened`); onOpen(action.route); }}>
          <Icon name={action.icon} />{action.label}
        </button>
      ))}
    </section>
  );
}

function RecentActivity({ available, logs, onOpen, onNotice }: {
  readonly available: boolean;
  readonly logs: readonly AuditLogEntry[];
  onOpen(value: string): void;
  onNotice(value: string): void;
}) {
  const [page, setPage] = useState(1);
  const pageSize = 5;
  const pages = Math.max(1, Math.ceil(logs.length / pageSize));
  const visible = logs.slice((page - 1) * pageSize, page * pageSize);
  return (
    <section className="panel activity-panel">
      <div className="panel-header activity-header"><h2>Recent activity</h2><button type="button" className="link-button" onClick={() => onOpen("Activity")}>View all logs<Icon name="external" size={14} /></button></div>
      {!available ? <div className="activity-empty">Activity unavailable because audit logs could not be read.</div> : visible.length === 0 ? <div className="activity-empty">No recorded activity yet.</div> : (
        <div className="activity-scroll portal-table-scroll" role="region" aria-label="Recent activity records" tabIndex={0}><table><thead><tr><th>Time</th><th>Actor</th><th>Action</th><th>Resource</th><th>Result</th><th><span className="sr-only">Open</span></th></tr></thead>
          <tbody>{visible.map((entry, index) => (
            <tr tabIndex={0} key={entry.id} onClick={() => onNotice(`${friendlyAction(entry.action)} selected`)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") onNotice(`${friendlyAction(entry.action)} selected`); }}>
              <td>{new Date(entry.occurred_at_ms).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}</td>
              <td><span className="activity-cell"><span className={`table-avatar avatar-${index}`}>{initials(entry.actor)}</span>{friendlyActor(entry.actor)}</span></td>
              <td>{friendlyAction(entry.action)}</td>
              <td><span className="activity-cell"><Icon name={resourceIcon(entry.resource)} size={16} />{entry.resource}</span></td>
              <td><span className="activity-cell"><span className={`result-check outcome-${entry.outcome}`}>{entry.outcome === "success" ? <Icon name="check" size={11} /> : "!"}</span>{friendlyOutcome(entry.outcome)}</span></td>
              <td><Icon name="chevronRight" size={15} /></td>
            </tr>
          ))}</tbody>
        </table></div>
      )}
      {!available ? null : <div className="table-footer"><span>Showing {logs.length === 0 ? 0 : (page - 1) * pageSize + 1}–{Math.min(page * pageSize, logs.length)} of {logs.length}</span>
        <div className="pagination"><button type="button" aria-label="Previous page" disabled={page === 1} onClick={() => setPage((value) => Math.max(1, value - 1))}><Icon name="chevronRight" className="flip" size={14} /></button>
          {Array.from({ length: Math.min(pages, 5) }, (_, index) => index + 1).map((value) => <button type="button" className={value === page ? "current" : ""} key={value} onClick={() => setPage(value)}>{value}</button>)}
          <button type="button" aria-label="Next page" disabled={page === pages} onClick={() => setPage((value) => Math.min(pages, value + 1))}><Icon name="chevronRight" size={14} /></button>
          <button type="button" className="per-page">5 / page<Icon name="chevronDown" size={12} /></button>
        </div>
      </div>}
    </section>
  );
}

function ProjectTelemetry({ data, configuration }: { readonly data: OverviewData; readonly configuration: PortalConfiguration }) {
  const inflight = overviewMetric(data, "ffdb_http_requests_inflight");
  const requests = overviewMetric(data, "ffdb_http_requests_total");
  const coverage = data.schema.tables.length === 0 ? null : Math.round((data.schema.tables.filter((table) => table.rls_enabled).length / data.schema.tables.length) * 100);
  return (
    <section className="panel worker-panel">
      <h2>Project telemetry</h2>
      <div className="worker-name"><Icon name="database" size={15} /><span>FFDB API</span><em>{data.ready === null ? "Readiness unavailable" : `Readiness: ${data.ready}`}</em></div>
      <dl className="worker-stats"><div><dt>Active project</dt><dd>{configuration.projectName}</dd></div><div><dt>Total requests</dt><dd>{compactNumber(requests)}</dd></div><div><dt>Inflight requests</dt><dd>{inflight}</dd></div></dl>
      <div className="meter"><div><span>RLS coverage</span><strong>{coverage === null ? "Not applicable" : `${coverage}%`}</strong></div>{coverage === null ? null : <div className="progress"><i style={{ width: `${coverage}%` }} /></div>}</div>
      <button type="button" className="link-button worker-link" onClick={() => globalThis.location.assign("/metrics")}>View raw metrics<Icon name="external" size={14} /></button>
    </section>
  );
}

function overviewMetric(data: OverviewData, name: string): string {
  return data.availability.metrics && data.metrics !== null ? prometheusTotal(data.metrics, name) : "Unavailable";
}

function compactNumber(value: string | number): string {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) return String(value);
  return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(numeric);
}

function initials(actor: string): string {
  const parts = actor.split(/[\s:@._-]+/u).filter(Boolean);
  const value = parts.slice(0, 2).map((part) => part[0]?.toUpperCase() ?? "").join("");
  return value || "FF";
}

function friendlyActor(actor: string): string {
  if (actor === "system") return "System";
  return actor.length > 24 ? `${actor.slice(0, 21)}…` : actor;
}

function friendlyAction(action: string): string {
  const parts = action.split(/[._]/u).filter(Boolean).slice(-2);
  return parts.map((part, index) => index === 0
    ? `${part[0]?.toUpperCase() ?? ""}${part.slice(1)}`
    : part).join(" ");
}

function friendlyOutcome(outcome: AuditLogEntry["outcome"]): string {
  return `${outcome[0]?.toUpperCase() ?? ""}${outcome.slice(1)}`;
}

function resourceIcon(resource: string): IconName {
  if (resource.includes("storage")) return "archive";
  if (resource.includes("auth") || resource.includes("user")) return "users";
  if (resource.includes("policy")) return "shield";
  if (resource.includes("email")) return "mail";
  return "code";
}

function SqlPanel({ client, sql, onSql }: { readonly client: FFDBClient; readonly sql: string; onSql(value: string): void }) {
  const [result, setResult] = useState<QueryResult | null>(null);
  const [state, setState] = useState<"idle" | "running">("idle");
  const [error, setError] = useState<string | null>(null);
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setState("running"); setError(null);
    try { setResult(await client.query({ sql, options: { max_rows: 250 } })); }
    catch (cause) { setError(errorMessage(cause)); }
    finally { setState("idle"); }
  };
  return (
    <section className="management-panel editor-panel">
      <form onSubmit={(event) => void submit(event)}>
        <label htmlFor="sql">Parameterized SQL</label>
        <textarea id="sql" value={sql} onChange={(event) => onSql(event.target.value)} spellCheck={false} />
        <div className="form-actions"><span>Server parsing and SQLite authorization are authoritative.</span><button className="primary-action" disabled={state === "running"} type="submit">{state === "running" ? "Running…" : "Run SQL"}</button></div>
      </form>
      {error === null ? null : <ErrorState message={error} />}
      {result === null ? <EmptyState title="No result yet" detail="Run a statement to inspect typed rows and column metadata." /> : <QueryResultTable result={result} />}
    </section>
  );
}

function DatabasePanel({ client }: { readonly client: FFDBClient }) {
  const resource = useResource(useCallback(async () => {
    const [schema, migrations] = await Promise.all([client.schema(), client.migrationHistory()]);
    return { schema, migrations };
  }, [client]));
  const [selectedTable, setSelectedTable] = useState("");
  const [rows, setRows] = useState<QueryResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const browse = async (table: string) => { setSelectedTable(table); setError(null); try { setRows(await client.query({ sql: `SELECT * FROM "${table.replaceAll('"', '""')}" LIMIT 100` })); } catch (cause) { setError(errorMessage(cause)); } };
  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading schema" />;
  const { schema, migrations } = resource.data;
  return <div className="management-grid"><section className="management-panel span-two"><PanelHeader title={`Schema version ${schema.version}`} />{schema.tables.length === 0 ? <EmptyState title="No tables" detail="This project has no application tables yet." /> : <SimpleTable headings={["Table", "RLS", "Force", "Definition", "Data"]} rows={schema.tables.map((table) => [table.name, table.rls_enabled ? "Enabled" : "Disabled", table.rls_forced ? "Yes" : "No", table.sql, <button type="button" key={table.name} onClick={() => void browse(table.name)}>Browse</button>])} />}</section><section className="management-panel span-two"><PanelHeader title="Migration history" />{migrations.length === 0 ? <EmptyState title="No migrations" detail="Applied and rolled-back migrations will appear here." /> : <SimpleTable headings={["Migration", "Status", "Schema", "Applied"]} rows={migrations.map(migrationRow)} />}</section>{selectedTable === "" ? null : <section className="management-panel span-two"><PanelHeader title={`Rows · ${selectedTable}`} />{error === null ? null : <ErrorState message={error} />}{rows === null ? <p>Loading table rows…</p> : <QueryResultTable result={rows} />}</section>}</div>;
}

function migrationRow(migration: MigrationSummary): readonly ReactNode[] { return [migration.name, migration.status, `${migration.schema_version_before} → ${migration.schema_version_after}`, migration.applied_at_ms === null ? "Pending" : new Date(migration.applied_at_ms).toLocaleString()]; }

function PoliciesPanel({ client, onEdit }: { readonly client: FFDBClient; onEdit(sql: string): void }) {
  const resource = useResource(useCallback(() => client.policies(), [client]));
  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading policies" />;
  return <section className="management-panel"><PanelHeader title="Row-level security policies" action="Create with SQL" onAction={() => onEdit("CREATE POLICY policy_name\nON table_name\nFOR SELECT\nTO authenticated\nUSING (owner_id = auth.uid());")} />{resource.data.length === 0 ? <EmptyState title="No policies" detail="RLS-enabled tables are default-deny until an applicable policy exists." /> : <SimpleTable headings={["Policy", "Table", "Command", "Roles", "Status"]} rows={resource.data.map((policy) => [policy.name, policy.table, policy.command, policy.roles.join(", "), policy.enabled ? "Enabled" : "Disabled"])} />}</section>;
}

function AuthPanel({ client }: { readonly client: FFDBClient }) {
  const [email, setEmail] = useState(""); const [password, setPassword] = useState("");
  const [session, setSession] = useState<string | null | undefined>(undefined); const [error, setError] = useState<string | null>(null);
  useEffect(() => { let active = true; void client.currentSession().then((value) => { if (active) setSession(value?.user.email ?? null); }, (cause: unknown) => { if (active) { setSession(null); setError(errorMessage(cause)); } }); return () => { active = false; }; }, [client]);
  const signIn = async (event: FormEvent) => { event.preventDefault(); setError(null); try { const value = await client.auth.signIn(email, password); setSession(value.user.email); } catch (cause) { setError(errorMessage(cause)); } finally { setPassword(""); } };
  const signOut = async () => { setError(null); try { await client.auth.signOut(); setSession(null); } catch (cause) { setError(errorMessage(cause)); } };
  return <div className="management-grid"><section className="management-panel form-panel"><PanelHeader title="End-user authentication" />{session === undefined ? <p>Loading end-user session…</p> : session === null ? <form onSubmit={(event) => void signIn(event)}><Field label="Email" type="email" value={email} onChange={setEmail} /><Field label="Password" type="password" value={password} onChange={setPassword} /><button className="primary-action" type="submit">Sign in</button></form> : <div className="signed-in"><span className="check-dot"><Icon name="check" size={14} /></span><div><strong>{session}</strong><p>User session is kept only for this browser session.</p></div><button type="button" onClick={() => void signOut()}>Sign out</button></div>}{error === null ? null : <ErrorState message={error} />}</section><AuthSettingsEditor client={client} /><AdminAuthUsers client={client} /></div>;
}

function AuthSettingsEditor({ client }: { readonly client: FFDBClient }) {
  const resource = useResource(useCallback(() => client.authSettings(), [client]));
  const [draft, setDraft] = useState<AuthSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { if (resource.status === "ready") setDraft(resource.data); }, [resource]);
  const save = async (event: FormEvent) => { event.preventDefault(); if (draft === null) return; setError(null); try { setDraft(await client.updateAuthSettings(draft)); } catch (cause) { setError(errorMessage(cause)); } };
  if (resource.status !== "ready" || draft === null) return <ResourceState resource={resource} title="Loading auth settings" />;
  return <section className="management-panel form-panel"><PanelHeader title="Auth settings" /><form onSubmit={(event) => void save(event)}><label className="field"><span><input type="checkbox" checked={draft.registration_enabled} onChange={(event) => setDraft({ ...draft, registration_enabled: event.target.checked })} /> Registration enabled</span></label><label className="field"><span><input type="checkbox" checked={draft.email_verification_required} onChange={(event) => setDraft({ ...draft, email_verification_required: event.target.checked })} /> Require email verification</span></label><NumberField label="Minimum password length" value={draft.password_min_length} onChange={(value) => setDraft({ ...draft, password_min_length: value })} /><NumberField label="Access token TTL seconds" value={draft.access_token_ttl_seconds} onChange={(value) => setDraft({ ...draft, access_token_ttl_seconds: value })} /><NumberField label="Refresh token TTL seconds" value={draft.refresh_token_ttl_seconds} onChange={(value) => setDraft({ ...draft, refresh_token_ttl_seconds: value })} /><button className="primary-action" type="submit">Save auth settings</button></form>{error === null ? null : <ErrorState message={error} />}</section>;
}

function AdminAuthUsers({ client }: { readonly client: FFDBClient }) {
  const [revision, setRevision] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const resource = useResource(useCallback(() => client.authUsers(), [client, revision]));
  const toggle = async (user: AuthUser) => {
    const disabling = !user.disabled;
    if (disabling && !globalThis.confirm(`Disable ${user.email}? Existing sessions will be rejected.`)) return;
    setError(null);
    try { await client.setAuthUserDisabled(user.id, disabling); setRevision((value) => value + 1); }
    catch (cause) { setError(errorMessage(cause)); }
  };
  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading project auth users" />;
  return <section className="management-panel span-two"><PanelHeader title="Project auth users" />{error === null ? null : <ErrorState message={error} />}{resource.data.length === 0 ? <EmptyState title="No end users" detail="Registered end users will appear here without password hashes or token material." /> : <SimpleTable headings={["Email", "Role", "Verified", "Status", "Action"]} rows={resource.data.map((user) => [user.email, user.role, user.email_verified ? "Verified" : "Pending", user.disabled ? "Disabled" : "Active", <button type="button" key={user.id} onClick={() => void toggle(user)}>{user.disabled ? "Enable" : "Disable"}</button>])} />}</section>;
}

function StoragePanel({ client }: { readonly client: FFDBClient }) {
  const [revision, setRevision] = useState(0); const [name, setName] = useState(""); const [error, setError] = useState<string | null>(null); const [selectedBucket, setSelectedBucket] = useState("");
  const resource = useResource(useCallback(() => client.storage.buckets(), [client, revision]));
  const create = async (event: FormEvent) => { event.preventDefault(); setError(null); try { await client.storage.createBucket({ name, public: false, max_object_bytes: null, versioning: false }); setName(""); setRevision((value) => value + 1); } catch (cause) { setError(errorMessage(cause)); } };
  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading buckets" />;
  const active = resource.data.find((bucket) => bucket.name === selectedBucket) ?? resource.data[0];
  return <div className="management-grid"><section className="management-panel span-two"><PanelHeader title="Storage buckets" /><form className="inline-form" onSubmit={(event) => void create(event)}><Field label="Bucket name" value={name} onChange={setName} /><button className="primary-action" type="submit" disabled={name === ""}>Create private bucket</button></form>{error === null ? null : <ErrorState message={error} />}{resource.data.length === 0 ? <EmptyState title="No buckets" detail="Create a private bucket; object access will be evaluated through project RLS." /> : <SimpleTable headings={["Bucket", "Visibility", "Versioning", "Max object", "Objects"]} rows={resource.data.map((bucket) => [...bucketRow(bucket), <button type="button" key={bucket.id} onClick={() => setSelectedBucket(bucket.name)}>{active?.name === bucket.name ? "Selected" : "Browse"}</button>])} />}</section>{active === undefined ? null : <BucketObjects client={client} bucket={active} />}</div>;
}

function bucketRow(bucket: StorageBucket): readonly ReactNode[] { return [bucket.name, bucket.public ? "Public" : "Private", bucket.versioning ? "Enabled" : "Disabled", formatBytes(bucket.max_object_bytes)]; }

function BucketObjects({ client, bucket }: { readonly client: FFDBClient; readonly bucket: StorageBucket }) {
  const [revision, setRevision] = useState(0); const [file, setFile] = useState<File | null>(null); const [key, setKey] = useState(""); const [error, setError] = useState<string | null>(null);
  const resource = useResource(useCallback(() => client.storage.list(bucket.name), [bucket.name, client, revision]));
  const upload = async (event: FormEvent) => { event.preventDefault(); if (file === null || key === "") return; setError(null); try { await client.storage.upload(bucket.name, key, file, { sizeBytes: file.size, contentType: file.type || "application/octet-stream" }); setFile(null); setKey(""); setRevision((value) => value + 1); } catch (cause) { setError(errorMessage(cause)); } };
  const download = async (objectKey: string) => { setError(null); try { const signed = await client.storage.downloadUrl(bucket.name, objectKey); globalThis.open(signed.url, "_blank", "noopener,noreferrer"); } catch (cause) { setError(errorMessage(cause)); } };
  const remove = async (objectKey: string) => { if (!globalThis.confirm(`Delete ${objectKey} from ${bucket.name}?`)) return; setError(null); try { await client.storage.delete(bucket.name, objectKey); setRevision((value) => value + 1); } catch (cause) { setError(errorMessage(cause)); } };
  if (resource.status !== "ready") return <ResourceState resource={resource} title={`Loading ${bucket.name} objects`} />;
  const trackedBytes = resource.data.items.reduce((total, item) => total + item.size_bytes, 0);
  return <section className="management-panel span-two"><PanelHeader title={`Objects · ${bucket.name}`} /><p className="panel-copy">Tracked page usage: {formatBytes(trackedBytes)} · project quota {formatBytes(bucket.project_quota_bytes)}.</p><form className="inline-form" onSubmit={(event) => void upload(event)}><label className="field"><span>Object file</span><input type="file" onChange={(event) => { const selected = event.target.files?.[0] ?? null; setFile(selected); if (selected !== null) setKey(selected.name); }} required /></label><Field label="Object key" value={key} onChange={setKey} /><button className="primary-action" type="submit" disabled={file === null || key === ""}>Upload</button></form>{error === null ? null : <ErrorState message={error} />}{resource.data.items.length === 0 ? <EmptyState title="No visible objects" detail="Sign in as an end user with an applicable RLS policy, then upload or list objects." /> : <SimpleTable headings={["Key", "Size", "Type", "Updated", "Actions"]} rows={resource.data.items.map((item) => [item.object_key, formatBytes(item.size_bytes), item.content_type ?? "—", new Date(item.updated_at_ms).toLocaleString(), <span className="action-row" key={item.id}><button type="button" onClick={() => void download(item.object_key)}>Download</button><button type="button" onClick={() => void remove(item.object_key)}>Delete…</button></span>])} />}</section>;
}

function SyncPanel({ client, onOpen }: { readonly client: FFDBClient; onOpen(value: string): void }) {
  const [result, setResult] = useState<unknown>(null); const [error, setError] = useState<string | null>(null);
  const run = async (kind: "snapshot" | "pull") => { setError(null); try { setResult(kind === "snapshot" ? await client.sync.snapshot() : await client.sync.pull(null, 100)); } catch (cause) { setError(errorMessage(cause)); } };
  return <section className="management-panel"><PanelHeader title="Offline sync" /><div className="action-row"><button className="primary-action" type="button" onClick={() => void run("snapshot")}>Fetch snapshot</button><button type="button" onClick={() => void run("pull")}>Pull changes</button><button type="button" onClick={() => onOpen("Auth")}>Manage user session</button></div><p className="panel-copy">Sync calls use the verified end-user session, opaque cursors, and RLS-filtered delivery.</p>{error === null ? null : <ErrorState message={error} />}{result === null ? <EmptyState title="No sync response" detail="Sign in as an end user, then fetch a snapshot or pull changes." /> : <JsonPreview value={result} />}</section>;
}

function EmailPanel({ client }: { readonly client: FFDBClient }) {
  const [revision, setRevision] = useState(0); const [artifactJson, setArtifactJson] = useState(""); const [variablesJson, setVariablesJson] = useState("{}"); const [selected, setSelected] = useState(""); const [preview, setPreview] = useState<unknown>(null); const [error, setError] = useState<string | null>(null);
  const resource = useResource(useCallback(() => client.emailTemplates(), [client, revision]));
  const importArtifact = async (event: FormEvent) => { event.preventDefault(); setError(null); try { const artifact = JSON.parse(artifactJson) as EmailTemplateArtifactInput; await client.importEmailTemplateArtifact(artifact); setArtifactJson(""); setRevision((value) => value + 1); } catch (cause) { setError(errorMessage(cause)); } };
  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading templates" />;
  const active = resource.data.find((template) => `${template.kind}:${template.version}` === selected) ?? resource.data[0];
  const publish = async () => { if (active === undefined || !globalThis.confirm(`Publish ${active.kind} version ${active.version}?`)) return; setError(null); try { await client.publishEmailTemplate(active.kind, active.version); setRevision((value) => value + 1); } catch (cause) { setError(errorMessage(cause)); } };
  const renderPreview = async () => { if (active === undefined) return; setError(null); try { const variables = JSON.parse(variablesJson) as Readonly<Record<string, string | number | boolean>>; setPreview(await client.previewEmailTemplate(active.kind, active.version, variables)); } catch (cause) { setError(errorMessage(cause)); } };
  return <div className="management-grid"><section className="management-panel span-two"><PanelHeader title="Transactional email template history" />{resource.data.length === 0 ? <EmptyState title="No custom templates" detail="Import a validated, precompiled artifact; runtime request handling never executes JSX or JavaScript." /> : <SimpleTable headings={["Kind", "Version", "Artifact", "Published", "Variables", "Action"]} rows={resource.data.map((template) => [...templateRow(template), <button type="button" key={`${template.kind}-${template.version}`} onClick={() => setSelected(`${template.kind}:${template.version}`)}>{active?.kind === template.kind && active.version === template.version ? "Selected" : "Select"}</button>])} />}{error === null ? null : <ErrorState message={error} />}</section><section className="management-panel form-panel"><h2>Import precompiled artifact</h2><form onSubmit={(event) => void importArtifact(event)}><label className="field"><span>Artifact JSON</span><textarea aria-label="Artifact JSON" value={artifactJson} onChange={(event) => setArtifactJson(event.target.value)} required /></label><button className="primary-action" type="submit" disabled={artifactJson === ""}>Validate and import</button></form></section><section className="management-panel form-panel"><h2>Preview and publish</h2>{active === undefined ? <p>Select or import a template version first.</p> : <><p>{active.kind} · version {active.version} · {active.artifact_status}</p><label className="field"><span>Preview variables JSON</span><textarea aria-label="Preview variables JSON" value={variablesJson} onChange={(event) => setVariablesJson(event.target.value)} /></label><div className="action-row"><button type="button" onClick={() => void renderPreview()}>Render preview</button><button className="primary-action" type="button" disabled={active.artifact_status !== "validated"} onClick={() => void publish()}>Publish…</button></div>{preview === null ? null : <JsonPreview value={preview} />}</>}</section><section className="management-panel span-two"><PanelHeader title="Delivery provider" /><p className="panel-copy">Deployment-managed. Production startup requires a Resend transport and secret; the portal intentionally cannot read or write provider credentials. Local development uses the operator-configured Mailpit SMTP service.</p></section></div>;
}

interface BillingPanelData {
  readonly organization: { readonly id: string; readonly name: string; readonly role: string };
  readonly summary: PlatformBillingSummary;
  readonly usage: PlatformUsageSummary;
  readonly invoices: readonly PlatformInvoiceSummary[];
}

function BillingPanel({ client, configuration, onNotice }: {
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onNotice(value: string): void;
}) {
  const loader = useCallback(async (): Promise<BillingPanelData | null> => {
    const organizations = await client.organizations();
    const organization = organizations.find((value) => value.name === configuration.organizationName)
      ?? organizations[0];
    if (organization === undefined) return null;
    const [summary, usage, invoices] = await Promise.all([
      client.organizationBilling(organization.id),
      client.organizationUsage(organization.id),
      client.organizationInvoices(organization.id),
    ]);
    return { organization, summary, usage, invoices };
  }, [client, configuration.organizationName]);
  const resource = useResource(loader);
  const [pending, setPending] = useState<PlatformBillingTier | "portal" | null>(null);
  const [redirect, setRedirect] = useState<{ readonly label: string; readonly url: string } | null>(null);

  const checkout = async (organizationId: string, tier: PlatformBillingTier) => {
    setPending(tier);
    setRedirect(null);
    try {
      const result = await client.createBillingCheckout(
        organizationId,
        { tier },
        { idempotencyKey: `portal-billing:${organizationId}:${tier}:${globalThis.crypto.randomUUID()}` },
      );
      setRedirect({ label: `Continue to ${formatBillingTier(tier)} checkout`, url: result.url });
    } catch (cause) {
      onNotice(errorMessage(cause));
    } finally {
      setPending(null);
    }
  };

  const openPortal = async (organizationId: string) => {
    setPending("portal");
    setRedirect(null);
    try {
      const result = await client.createBillingPortal(organizationId, {
        idempotencyKey: `portal-customer:${organizationId}:${globalThis.crypto.randomUUID()}`,
      });
      setRedirect({ label: "Open secure billing portal", url: result.url });
    } catch (cause) {
      onNotice(errorMessage(cause));
    } finally {
      setPending(null);
    }
  };

  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading organization billing" />;
  if (resource.data === null) return <section className="management-panel"><EmptyState title="No organization" detail="Create an organization in Settings before choosing an FFDB plan." /></section>;
  const { organization, summary, usage, invoices } = resource.data;
  const canManage = organization.role === "owner" || organization.role === "admin";
  const paid = summary.tier !== "free";

  return <div className="management-grid billing-grid">
    <section className="management-panel span-two billing-summary">
      <PanelHeader title={`${organization.name} plan`} />
      <div className="billing-heading"><div><span className="status-badge">{formatBillingStatus(summary.status)}</span><h3>{formatBillingTier(summary.tier)}</h3><p>Billing unit: {summary.billing_unit === "seat" ? `${summary.seat_quantity} seat${summary.seat_quantity === 1 ? "" : "s"}` : "organization"}</p></div><strong>{summary.project_limit === null ? "Unlimited" : summary.project_limit}<small>projects</small></strong></div>
      <div className="allowance-grid">
        <Allowance label="Storage" value={`${formatBytes(usage.storage_bytes)} / ${formatBytes(summary.usage_allowance.storage_bytes)}`} />
        <Allowance label="Reads" value={`${compactNumber(usage.reads)} / ${compactNumber(summary.usage_allowance.monthly_reads)}`} />
        <Allowance label="Writes" value={`${compactNumber(usage.writes)} / ${compactNumber(summary.usage_allowance.monthly_writes)}`} />
        <Allowance label="Active users" value={`${compactNumber(usage.monthly_active_users)} / ${compactNumber(summary.usage_allowance.monthly_active_users)}`} />
      </div>
      <p className="panel-copy">Usage shown for {new Date(usage.period_start_ms).toLocaleDateString()}–{new Date(usage.period_end_ms).toLocaleDateString()}. Storage has accumulated {compactNumber(usage.storage_byte_hours)} byte-hours. {!summary.billing_enforcement_enabled || summary.billing_exempt ? "This organization is unmetered for billing; usage remains visible for capacity planning." : summary.usage_allowance.overage_enabled ? "Usage above the included allowance is reported automatically for invoicing." : "Writes, new active users, and storage growth stop at the included limits; reads continue."}</p>
      {usage.reporting_status === "healthy" ? null : <p className="security-note">Usage reporting is {usage.reporting_status}. Billable writes are paused if reporting reaches the blocked state.</p>}
      {summary.current_period_end_ms === null ? null : <p className="panel-copy">Current period ends {new Date(summary.current_period_end_ms).toLocaleDateString()}{summary.cancel_at_period_end ? "; cancellation is scheduled" : ""}.</p>}
      {!summary.billing_enforcement_enabled ? <p className="security-note">This instance is running without tenant charges. The instance owner can enable operator billing from Instance setup.</p> : !summary.provider_configured ? <p className="security-note">Tenant billing is enabled but its payment provider is not ready. The instance owner must finish Stripe setup before paid plans can be selected.</p> : null}
      {!canManage ? <p className="security-note">Only an organization owner or admin can change this plan.</p> : null}
      <div className="action-row">
        <button className="primary-action" type="button" disabled={!summary.provider_configured || !canManage || pending !== null} onClick={() => void checkout(organization.id, "pay_as_you_go")}>{pending === "pay_as_you_go" ? "Creating checkout…" : "Choose pay as you go"}</button>
        <button type="button" disabled={!summary.provider_configured || !canManage || pending !== null} onClick={() => void checkout(organization.id, "pro")}>{pending === "pro" ? "Creating checkout…" : "Choose Pro"}</button>
        <button type="button" disabled={!summary.provider_configured || !canManage || !paid || pending !== null} onClick={() => void openPortal(organization.id)}>{pending === "portal" ? "Opening portal…" : "Manage payment method"}</button>
      </div>
      {redirect === null ? null : <a className="billing-redirect" href={redirect.url} rel="noreferrer">{redirect.label}<Icon name="external" size={16} /></a>}
    </section>
    <section className="management-panel span-two">
      <PanelHeader title="Invoices" />
      {invoices.length === 0 ? <EmptyState title="No invoices yet" detail="Finalized Stripe invoices will appear here with their hosted receipt and PDF." /> : <SimpleTable headings={["Created", "Status", "Amount", "Period", "Receipt"]} rows={invoices.map((invoice) => [
        new Date(invoice.created_at_ms).toLocaleDateString(),
        invoice.status.replaceAll("_", " "),
        new Intl.NumberFormat(undefined, { style: "currency", currency: invoice.currency.toUpperCase() }).format(invoice.amount_due_minor / 100),
        invoice.period_start_ms === null || invoice.period_end_ms === null ? "—" : `${new Date(invoice.period_start_ms).toLocaleDateString()}–${new Date(invoice.period_end_ms).toLocaleDateString()}`,
        invoice.hosted_invoice_url === null ? "—" : <a href={invoice.hosted_invoice_url} rel="noreferrer">Open invoice</a>,
      ])} />}
    </section>
    <PlanCard name="Free" price="$0" detail="Two projects with hard included limits and no surprise overage charges." />
    <PlanCard name="Pay as you go" price="Usage" detail="$0.20/GB-month storage prorated from byte-hours, plus metered reads, writes, and monthly active users." />
    <PlanCard name="Pro" price="$7/mo" detail="Larger included allowances, then the same transparent usage pricing." />
  </div>;
}

function Allowance({ label, value }: { readonly label: string; readonly value: string }) { return <div><span>{label}</span><strong>{value}</strong></div>; }
function PlanCard({ name, price, detail }: { readonly name: string; readonly price: string; readonly detail: string }) { return <article className="management-panel plan-card"><span>FFDB plan</span><h2>{name}</h2><strong>{price}</strong><p>{detail}</p></article>; }
function formatBillingTier(value: PlatformBillingTier): string { return value === "pay_as_you_go" ? "Pay as you go" : value === "pro" ? "Pro" : "Free"; }
function formatBillingStatus(value: PlatformBillingSummary["status"]): string { return value.replaceAll("_", " "); }

function templateRow(template: EmailTemplateVersion): readonly ReactNode[] { return [template.kind, String(template.version), template.compilation_errors.length === 0 ? template.artifact_status : `${template.compilation_errors.length} errors`, template.published_at_ms === null ? "Draft" : new Date(template.published_at_ms).toLocaleString(), template.allowed_variables.join(", ")]; }

function LogsPanel({ client }: { readonly client: FFDBClient }) {
  const resource = useResource(useCallback(() => client.logs({ limit: 100 }), [client]));
  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading audit logs" />;
  return <section className="management-panel"><PanelHeader title="Audit log" />{resource.data.length === 0 ? <EmptyState title="No audit events" detail="Project security and lifecycle events will appear here." /> : <SimpleTable headings={["Time", "Actor", "Action", "Resource", "Outcome"]} rows={resource.data.map(logRow)} />}</section>;
}

function logRow(entry: AuditLogEntry): readonly ReactNode[] { return [new Date(entry.occurred_at_ms).toLocaleString(), entry.actor, entry.action, entry.resource, entry.outcome]; }

function BackupsPanel({ client, onNotice }: { readonly client: FFDBClient; onNotice(value: string): void }) {
  const [revision, setRevision] = useState(0); const resource = useResource(useCallback(() => client.backups(), [client, revision]));
  const create = async () => { try { await client.createBackup(); onNotice("Backup requested"); setRevision((value) => value + 1); } catch (cause) { onNotice(errorMessage(cause)); } };
  const restore = async (backup: BackupSummary) => { if (!globalThis.confirm(`Restore backup ${backup.id}? Current project data will be replaced.`)) return; try { await client.restoreBackup(backup.id); onNotice(`Restore requested for ${backup.id}`); setRevision((value) => value + 1); } catch (cause) { onNotice(errorMessage(cause)); } };
  if (resource.status !== "ready") return <ResourceState resource={resource} title="Loading backups" action="Request backup" onAction={() => void create()} />;
  return <section className="management-panel"><PanelHeader title="Backups" action="Request backup" onAction={() => void create()} />{resource.data.length === 0 ? <EmptyState title="No backups" detail="Request an online SQLite-safe backup to begin the restore-verification lifecycle." /> : <SimpleTable headings={["Created", "Status", "Size", "Restore tested", "Action"]} rows={resource.data.map((backup) => [...backupRow(backup), <button type="button" key={backup.id} onClick={() => void restore(backup)}>Restore…</button>])} />}</section>;
}

function backupRow(backup: BackupSummary): readonly ReactNode[] { return [new Date(backup.created_at_ms).toLocaleString(), backup.status, backup.size_bytes === null ? "Pending" : formatBytes(backup.size_bytes), backup.last_restore_test_ms === null ? "Not yet" : new Date(backup.last_restore_test_ms).toLocaleString()]; }

interface WorkspaceData {
  readonly organizations: Awaited<ReturnType<FFDBClient["organizations"]>>;
  readonly projects: Awaited<ReturnType<FFDBClient["projects"]>>;
  readonly members: Awaited<ReturnType<FFDBClient["organizationMembers"]>>;
  readonly activeOrganizationId: string;
}

function WorkspacePanel({ view, client, configuration, onConfiguration, onNotice, onOpen, onSetupRequired }: {
  readonly view: "projects" | "members";
  readonly client: FFDBClient;
  readonly configuration: PortalConfiguration;
  onConfiguration(value: PortalConfiguration): void;
  onNotice(value: string): void;
  onOpen(value: PortalRoute): void;
  onSetupRequired(): void;
}) {
  const [revision, setRevision] = useState(0);
  const [data, setData] = useState<WorkspaceData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [organizationName, setOrganizationName] = useState("");
  const [organizationSlug, setOrganizationSlug] = useState("");
  const [projectName, setProjectName] = useState("");
  const [projectSlug, setProjectSlug] = useState("");
  const [memberEmail, setMemberEmail] = useState("");
  const [memberRole, setMemberRole] = useState<"admin" | "developer" | "viewer">("developer");
  const [submitting, setSubmitting] = useState(false);

  const load = useCallback(async () => {
    setError(null);
    try {
      const organizations = await client.organizations();
      const active = organizations.find((organization) => organization.id === configuration.organizationId)
        ?? organizations.find((organization) => organization.name === configuration.organizationName)
        ?? organizations[0];
      const [projects, members] = active === undefined
        ? [[], []] as const
        : await Promise.all([client.projects(active.id), client.organizationMembers(active.id)]);
      setData({ organizations, projects, members, activeOrganizationId: active?.id ?? "" });
      if (active !== undefined && configuration.organizationId !== active.id) {
        persistPortalProject(configuration.projectId, configuration.developerKey, active.name, active.id, configuration.projectName, configuration.apiUrl);
        onConfiguration({ ...configuration, organizationId: active.id, organizationName: active.name });
      }
    } catch (cause) {
      setError(errorMessage(cause));
    }
  }, [client, configuration, onConfiguration, revision]);

  useEffect(() => { void load(); }, [load]);

  const createOrganization = async (event: FormEvent) => {
    event.preventDefault();
    setSubmitting(true);
    setError(null);
    try {
      const organization = await client.createOrganization({ name: organizationName.trim(), slug: organizationSlug.trim() });
      persistPortalProject("", undefined, organization.name, organization.id, "Choose a project", configuration.apiUrl);
      onConfiguration({
        ...configuration,
        organizationId: organization.id,
        organizationName: organization.name,
        projectId: "",
        projectName: "Choose a project",
        developerKey: undefined,
        developerKeyExpiresAtMs: undefined,
        developerKeyManaged: undefined,
      });
      setOrganizationName(""); setOrganizationSlug(""); setRevision((value) => value + 1);
      onNotice(`${organization.name} created; create its first project to finish onboarding`);
    } catch (cause) {
      if (isInstanceSetupRequired(cause)) onSetupRequired();
      else setError(errorMessage(cause));
    } finally { setSubmitting(false); }
  };

  const activateProject = async (project: Awaited<ReturnType<FFDBClient["projects"]>>[number], organizationNameValue: string, organizationId: string) => {
    client.setProjectId(project.id);
    let issuedKey = portalProjectKey(configuration.apiUrl, project.id);
    let keyMetadata = portalProjectKeyMetadata(configuration.apiUrl, project.id);
    if (issuedKey === undefined) {
      try {
        const credential = await issuePortalProjectCredential(client);
        issuedKey = credential.secret;
        keyMetadata = { expiresAtMs: credential.expiresAtMs, managed: credential.managed };
        persistPortalProjectKeyMetadata(configuration.apiUrl, project.id, keyMetadata);
      } catch {
        issuedKey = undefined;
        keyMetadata = undefined;
      }
    }
    client.setDeveloperKey(issuedKey ?? null);
    persistPortalProject(project.id, issuedKey, organizationNameValue, organizationId, project.name, configuration.apiUrl);
    onConfiguration({
      ...configuration,
      organizationId,
      organizationName: organizationNameValue,
      projectId: project.id,
      projectName: project.name,
      developerKey: issuedKey,
      developerKeyExpiresAtMs: keyMetadata?.expiresAtMs,
      developerKeyManaged: keyMetadata?.managed,
    });
    onNotice(issuedKey === undefined ? `${project.name} is active; opening a secure project session` : `${project.name} is ready with a temporary portal credential`);
  };

  const createProject = async (event: FormEvent) => {
    event.preventDefault();
    const organization = data?.organizations.find((value) => value.id === data.activeOrganizationId);
    if (organization === undefined) return;
    setSubmitting(true); setError(null);
    try {
      const project = await client.createProject({ organization_id: organization.id, name: projectName.trim(), slug: projectSlug.trim() });
      setProjectName(""); setProjectSlug("");
      await activateProject(project, organization.name, organization.id);
      setRevision((value) => value + 1);
      onOpen("Overview");
    } catch (cause) {
      if (isInstanceSetupRequired(cause)) onSetupRequired();
      else setError(errorMessage(cause));
    } finally { setSubmitting(false); }
  };

  const invite = async (event: FormEvent) => {
    event.preventDefault();
    if (data?.activeOrganizationId === "") return;
    setSubmitting(true); setError(null);
    try {
      await client.createOrganizationInvitation(data!.activeOrganizationId, { email: memberEmail.trim(), role: memberRole });
      onNotice(`Invitation sent to ${memberEmail.trim()}`); setMemberEmail(""); setRevision((value) => value + 1);
    } catch (cause) { setError(errorMessage(cause)); } finally { setSubmitting(false); }
  };

  if (data === null && error === null) return <ResourceState resource={{ status: "loading" }} title="Loading workspace" />;
  if (data === null) return <ErrorState message={error ?? "Workspace could not be loaded."} />;

  if (data.organizations.length === 0) return <section className="first-workspace-panel">
    <SetupProgress active={4} />
    <div className="first-workspace-layout"><div><span className="setup-lock"><Icon name="shield" size={18} />Instance setup complete</span><h2>Create your first organization</h2><p>An organization owns projects, members, usage, and billing. You can create more after entering the portal.</p><ol><li className="active">Create organization</li><li>Create project</li><li>Enter the portal</li></ol></div><form className="management-panel form-panel" onSubmit={(event) => void createOrganization(event)}><Field label="Organization name" value={organizationName} onChange={(value) => { setOrganizationName(value); if (organizationSlug === "") setOrganizationSlug(slugify(value)); }} /><Field label="Organization slug" value={organizationSlug} onChange={setOrganizationSlug} />{error === null ? null : <ErrorState message={error} />}<button className="primary-action" disabled={submitting} type="submit">{submitting ? "Creating…" : "Create organization and continue"}</button></form></div>
  </section>;

  const activeOrganization = data.organizations.find((value) => value.id === data.activeOrganizationId)!;
  if (data.projects.length === 0) return <section className="first-workspace-panel">
    <SetupProgress active={4} />
    <div className="first-workspace-layout"><div><span className="setup-lock"><Icon name="check" size={18} />{activeOrganization.name} created</span><h2>Create your first project</h2><p>Each project receives one isolated SQLite database plus its own auth, storage, sync, email, and commerce boundary.</p><ol><li className="complete">Organization created</li><li className="active">Create project</li><li>Enter the portal</li></ol></div><form className="management-panel form-panel" onSubmit={(event) => void createProject(event)}><Field label="Project name" value={projectName} onChange={(value) => { setProjectName(value); if (projectSlug === "") setProjectSlug(slugify(value)); }} /><Field label="Project slug" value={projectSlug} onChange={setProjectSlug} />{error === null ? null : <ErrorState message={error} />}<button className="primary-action" disabled={submitting} type="submit">{submitting ? "Creating project…" : "Create project and enter portal"}</button></form></div>
  </section>;

  if (view === "members") return <div className="focused-management-layout">
    <section className="management-panel span-two"><PanelHeader title={`Members · ${activeOrganization.name}`} /><p className="panel-copy">Organization roles apply across every project in this organization.</p>{data.members.length === 0 ? <EmptyState title="No members yet" detail="Invite an administrator, developer, or viewer." /> : <SimpleTable headings={["Account", "Role", "Added"]} rows={data.members.map((member) => [member.email, member.role, new Date(member.created_at_ms).toLocaleString()])} />}</section>
    <section className="management-panel form-panel"><h2>Invite member</h2><form onSubmit={(event) => void invite(event)}><Field label="Email" type="email" value={memberEmail} onChange={setMemberEmail} /><label className="field"><span>Role</span><select value={memberRole} onChange={(event) => setMemberRole(event.target.value as typeof memberRole)}><option value="admin">Administrator</option><option value="developer">Developer</option><option value="viewer">Viewer</option></select></label>{error === null ? null : <ErrorState message={error} />}<button className="primary-action" disabled={submitting} type="submit">Send invitation</button></form></section>
  </div>;

  return <div className="focused-management-layout">
    <section className="management-panel span-two"><PanelHeader title={`Projects · ${activeOrganization.name}`} /><p className="panel-copy">Select a project to update every project-scoped page and command.</p><SimpleTable headings={["Project", "State", "Project ID", "Action"]} rows={data.projects.map((project) => [<span className="entity-primary" key={project.id}><strong>{project.name}</strong><small>{project.slug}</small></span>, project.state, <code>{project.id}</code>, <button type="button" key={`${project.id}-action`} onClick={() => void activateProject(project, activeOrganization.name, activeOrganization.id)}>{project.id === configuration.projectId ? "Active project" : "Use project"}</button>])} /></section>
    <section className="management-panel form-panel"><h2>Create another project</h2><form onSubmit={(event) => void createProject(event)}><Field label="Project name" value={projectName} onChange={(value) => { setProjectName(value); if (projectSlug === "") setProjectSlug(slugify(value)); }} /><Field label="Project slug" value={projectSlug} onChange={setProjectSlug} />{error === null ? null : <ErrorState message={error} />}<button className="primary-action" disabled={submitting} type="submit">Create project</button></form></section>
  </div>;
}

function SetupProgress({ active }: { readonly active: 1 | 2 | 3 | 4 }) {
  return <ol className="setup-progress setup-progress-wide" aria-label="Instance setup progress">{["Owner", "Instance type", "Payments", "First workspace"].map((label, index) => <li aria-current={active === index + 1 ? "step" : undefined} className={active > index + 1 ? "complete" : ""} key={label}><span>{index + 1}</span>{label}</li>)}</ol>;
}

function AccountPanel({ client, configuration, onInstanceChange, onNotice }: { readonly client: FFDBClient; readonly configuration: PortalConfiguration; onInstanceChange(value: PortalConfiguration): void; onNotice(value: string): void }) {
  const [session, setSession] = useState<Awaited<ReturnType<FFDBClient["developerSession"]>> | null>(null);
  const [instanceName, setInstanceName] = useState("");
  const [apiUrl, setApiUrl] = useState("");
  useEffect(() => { void client.developerSession().then(setSession, () => setSession(null)); }, [client]);
  const addInstance = (event: FormEvent) => {
    event.preventDefault();
    try {
      const normalized = new URL(apiUrl).origin;
      const record = { apiUrl: normalized, instanceName: instanceName.trim() };
      persistPortalInstance(record); onInstanceChange(selectPortalInstance(record)); onNotice(`${record.instanceName} added; sign in to continue`);
    } catch { onNotice("Enter a valid http or https instance URL"); }
  };
  return <div className="focused-management-layout account-layout">
    <section className="management-panel account-summary"><span className="profile-avatar large">{session?.email.slice(0, 2).toUpperCase() ?? "FF"}</span><div><span>Signed-in account</span><h2>{session?.email ?? "Developer session"}</h2><p>{configuration.instanceName ?? configuration.apiUrl} · {configuration.organizationName} · {configuration.projectName}</p></div></section>
    <section className="management-panel"><PanelHeader title="Saved deployments" /><p className="panel-copy">Each deployment keeps an isolated developer session and project-key namespace.</p><SimpleTable headings={["Deployment", "Origin", "Status", "Action"]} rows={portalInstances().map((instance) => [instance.instanceName, instance.apiUrl, instance.apiUrl === configuration.apiUrl ? "Active" : "Saved", <button type="button" key={instance.apiUrl} disabled={instance.apiUrl === configuration.apiUrl} onClick={() => onInstanceChange(selectPortalInstance(instance))}>Switch</button>])} /></section>
    <section className="management-panel form-panel"><h2>Add a deployment</h2><form onSubmit={addInstance}><Field label="Deployment name" value={instanceName} onChange={setInstanceName} /><Field label="Deployment URL" type="url" value={apiUrl} onChange={setApiUrl} /><button className="primary-action" type="submit">Save and connect</button></form></section>
    <section className="management-panel"><PanelHeader title="Session" /><dl className="account-details"><div><dt>API origin</dt><dd>{configuration.apiUrl}</dd></div><div><dt>Project</dt><dd>{configuration.projectId || "Not selected"}</dd></div><div><dt>Project credential</dt><dd>{configuration.developerKey === undefined ? "Not configured" : "Scoped key configured"}</dd></div></dl><button type="button" onClick={() => void client.developerSignOut().then(() => globalThis.location.reload())}>Sign out of this deployment</button></section>
  </div>;
}

function slugify(value: string): string { return value.trim().toLowerCase().replace(/[^a-z0-9]+/gu, "-").replace(/^-|-$/gu, ""); }

function SettingsPanel({ client, configuration, onNotice, onConfiguration }: { readonly client: FFDBClient; readonly configuration: PortalConfiguration; onNotice(value: string): void; onConfiguration(value: PortalConfiguration): void }) {
  const [session, setSession] = useState<Awaited<ReturnType<FFDBClient["developerSession"]>> | null>(null);
  const [issuedKey, setIssuedKey] = useState<string | null>(null);
  const [apiKeys, setApiKeys] = useState<readonly ApiKeySummary[]>([]);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setError(null);
    try {
      const active = await client.developerSession();
      setSession(active);
      setApiKeys(active === null || client.projectId === "" ? [] : await client.apiKeys());
    } catch (cause) { setError(errorMessage(cause)); }
  }, [client]);
  useEffect(() => { void reload(); }, [reload]);

  const issueKey = async () => {
    setError(null);
    try {
      const value = await client.createApiKey({ name: "portal-generated", scopes: ["database_query", "database_migrate", "database_schema", "auth_manage", "storage_manage", "email_manage", "commerce_manage", "keys_rotate", "backups_manage", "logs_read"], expires_at_ms: null });
      setIssuedKey(value.secret);
      client.setDeveloperKey(value.secret);
      persistPortalProject(client.projectId, value.secret, configuration.organizationName, configuration.organizationId, configuration.projectName, configuration.apiUrl);
      persistPortalProjectKeyMetadata(configuration.apiUrl, client.projectId, {
        expiresAtMs: value.expires_at_ms,
        managed: false,
      });
      onConfiguration({
        ...configuration,
        developerKey: value.secret,
        developerKeyExpiresAtMs: value.expires_at_ms,
        developerKeyManaged: false,
      });
      await reload();
    } catch (cause) { setError(errorMessage(cause)); }
  };
  const revokeKey = async (key: ApiKeySummary) => {
    if (!globalThis.confirm(`Revoke API key ${key.name}?`)) return;
    try { await client.revokeApiKey(key.id); await reload(); onNotice(`${key.name} revoked`); } catch (cause) { setError(errorMessage(cause)); }
  };
  const rotate = async () => { try { const value = await client.rotateSigningKey(); onNotice(`JWT signing key rotated to ${value.active_kid}`); } catch (cause) { setError(errorMessage(cause)); } };

  return <div className="focused-management-layout settings-grid">
    <section className="management-panel connection-panel"><PanelHeader title="Active connection" /><dl><div><dt>Deployment</dt><dd>{configuration.instanceName ?? configuration.apiUrl}</dd></div><div><dt>FFDB API</dt><dd>{configuration.apiUrl}</dd></div><div><dt>Organization</dt><dd>{configuration.organizationName}</dd></div><div><dt>Project</dt><dd>{configuration.projectId || "Not selected"}</dd></div><div><dt>Project API key</dt><dd>{configuration.developerKey === undefined ? "Not configured" : `${configuration.developerKey.slice(0, 8)}… (redacted)`}</dd></div></dl><p className="security-note">Each saved deployment has an isolated session namespace. Project keys remain in sessionStorage and are never compiled into the portal.</p></section>
    <section className="management-panel"><PanelHeader title="Developer session" />{session === null ? <EmptyState title="No active session" detail="Sign in again to manage this instance." /> : <div className="signed-in"><span className="check-dot"><Icon name="check" size={14} /></span><div><strong>{session.email}</strong><p>Session expires {new Date(session.expires_at_ms).toLocaleString()}.</p></div></div>}</section>
    <section className="management-panel span-two"><PanelHeader title="Project API keys" action="Issue scoped key" onAction={() => void issueKey()} />{configuration.projectId === "" ? <EmptyState title="Choose a project" detail="Project credentials belong to one active project." /> : apiKeys.length === 0 ? <EmptyState title="No API keys" detail="Issue a scoped key for browser and CLI project operations." /> : <SimpleTable headings={["Name", "Prefix", "Scopes", "Created", "Status", "Action"]} rows={apiKeys.map((key) => [key.name, key.prefix, key.scopes.join(", "), new Date(key.created_at_ms).toLocaleString(), key.revoked_at_ms === null ? "Active" : "Revoked", <button type="button" key={key.id} disabled={key.revoked_at_ms !== null} onClick={() => void revokeKey(key)}>Revoke…</button>])} />}{issuedKey === null ? null : <div className="one-time-secret"><strong>Copy this key now</strong><code>{issuedKey}</code><button type="button" onClick={() => setIssuedKey(null)}>I saved it</button></div>}</section>
    <section className="management-panel"><PanelHeader title="Signing keys" /><p className="panel-copy">Rotate the project JWT signing key only when your clients can accept a new active key.</p><button type="button" onClick={() => void rotate()}>Rotate JWT signing key…</button></section>
    <section className="management-panel"><PanelHeader title="Help and reference" /><div className="settings-links"><a href="/docs/client">Client reference</a><a href="/docs/cli">CLI reference</a><a href="/docs/security">Security guide</a></div></section>
    {error === null ? null : <ErrorState message={error} />}
  </div>;
}

type Resource<T> = { readonly status: "loading" } | { readonly status: "ready"; readonly data: T } | { readonly status: "error"; readonly message: string };

function useResource<T>(load: () => Promise<T>): Resource<T> {
  const [resource, setResource] = useState<Resource<T>>({ status: "loading" });
  useEffect(() => { const controller = new AbortController(); setResource({ status: "loading" }); void load().then((data) => { if (!controller.signal.aborted) setResource({ status: "ready", data }); }, (cause: unknown) => { if (!controller.signal.aborted) setResource({ status: "error", message: errorMessage(cause) }); }); return () => controller.abort(); }, [load]);
  return resource;
}

function ResourceState<T>({ resource, title, action, onAction }: { readonly resource: Resource<T>; readonly title: string; readonly action?: string; onAction?(): void }) {
  if (resource.status === "loading") return <section className="management-panel"><div className="loading-line" /><h2>{title}</h2><p>Contacting the configured FFDB service…</p></section>;
  if (resource.status === "error") return <section className="management-panel"><PanelHeader title={title} {...(action === undefined ? {} : { action })} {...(onAction === undefined ? {} : { onAction })} /><ErrorState message={resource.message} /><p className="degraded-note">Verify the FFDB service version, active project, and credential scope. Include the request ID when contacting an operator.</p></section>;
  return null;
}

function MetricCard({ icon, label, value, detail }: { readonly icon: IconName; readonly label: string; readonly value: string; readonly detail: string }) { return <article className="management-panel metric-card"><Icon name={icon} /><span>{label}</span><strong>{value}</strong><p>{detail}</p></article>; }
function PanelHeader({ title, action, onAction }: { readonly title: string; readonly action?: string; onAction?(): void }) { return <div className="management-header"><h2>{title}</h2>{action === undefined ? null : <button type="button" onClick={onAction}>{action}</button>}</div>; }
function EmptyState({ title, detail }: { readonly title: string; readonly detail: string }) { return <div className="empty-state"><strong>{title}</strong><p>{detail}</p></div>; }
function ErrorState({ message }: { readonly message: string }) { return <div className="error-state" role="alert"><strong>Request failed</strong><span>{message}</span></div>; }
function JsonPreview({ value }: { readonly value: unknown }) { return <pre className="json-preview">{JSON.stringify(value, null, 2)}</pre>; }
function Field({ label, type = "text", value, onChange }: { readonly label: string; readonly type?: string; readonly value: string; onChange(value: string): void }) { const id = `field-${label.toLowerCase().replaceAll(" ", "-")}`; return <label className="field" htmlFor={id}><span>{label}</span><input id={id} type={type} value={value} onChange={(event) => onChange(event.target.value)} required /></label>; }
function NumberField({ label, value, onChange }: { readonly label: string; readonly value: number; onChange(value: number): void }) { const id = `field-${label.toLowerCase().replaceAll(" ", "-")}`; return <label className="field" htmlFor={id}><span>{label}</span><input id={id} type="number" min={1} value={value} onChange={(event) => onChange(Number(event.target.value))} required /></label>; }

function SimpleTable({ headings, rows }: { readonly headings: readonly string[]; readonly rows: readonly (readonly ReactNode[])[] }) { return <ManagedTable headings={headings} rows={rows} label="records" />; }

function QueryResultTable({ result }: { readonly result: QueryResult }) { return <div className="query-result"><div className="result-summary">{result.rows.length} rows · {result.affected_rows} affected{result.truncated ? " · truncated" : ""}</div><SimpleTable headings={result.columns.map((column) => `${column.name} · ${column.type}`)} rows={result.rows.map((row) => row.map((cell) => typeof cell === "object" && cell !== null ? JSON.stringify(cell) : cell))} /></div>; }

function errorMessage(cause: unknown): string {
  if (cause instanceof FFDBError) return `${cause.code}: ${cause.message}`;
  return cause instanceof Error ? cause.message : "Unknown portal request failure";
}

function isRejectedPortalProjectCredential(cause: unknown): boolean {
  return cause instanceof FFDBError && [
    "auth.invalid_credential",
    "auth.expired_credential",
    "auth.wrong_project",
  ].includes(cause.code);
}

function isInstanceSetupRequired(cause: unknown): boolean {
  return cause instanceof FFDBError && cause.status === 409 && cause.code === "instance.setup_required";
}

function formatBytes(bytes: number): string {
  if (bytes < 1_000) return `${bytes} B`;
  if (bytes < 1_000_000) return `${(bytes / 1_000).toFixed(1)} KB`;
  if (bytes < 1_000_000_000) return `${(bytes / 1_000_000).toFixed(1)} MB`;
  return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
}

function prometheusTotal(metrics: string, name: string): string {
  let total = 0;
  let found = false;
  for (const line of metrics.split("\n")) {
    if (!(line === name || line.startsWith(`${name}{`) || line.startsWith(`${name} `))) continue;
    const match = /\s(-?(?:\d+(?:\.\d+)?|\.\d+)(?:e[+-]?\d+)?)$/iu.exec(line);
    if (match?.[1] === undefined) continue;
    const value = Number(match[1]);
    if (Number.isFinite(value)) { total += value; found = true; }
  }
  return found ? String(total) : "Unavailable";
}

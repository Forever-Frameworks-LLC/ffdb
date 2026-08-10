import {
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  Clock3,
  Database,
  LogIn,
  LogOut,
  RefreshCcw,
  Search,
  ShieldCheck,
  SlidersHorizontal,
  UserRound,
  UsersRound,
  X,
} from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import {
  FFDBError,
  type AuthSettings,
  type AuthTokenPair,
  type AuthUser,
  type FFDBClient,
  type SnapshotResponse,
  type SyncPullResponse,
} from "@ffdb/client";

import "./auth-sync.css";

export interface AuthRouteProps {
  readonly client: FFDBClient;
  readonly initialTab?: AuthRouteTab;
  readonly onNotice?: (message: string) => void;
  readonly onSessionChange?: (session: AuthTokenPair | null) => void;
}

export type AuthRouteTab = "users" | "policy";

export interface SyncRouteProps {
  readonly client: FFDBClient;
  readonly onManageSession: () => void;
  readonly onNotice?: (message: string) => void;
}

type Loadable<T> =
  | { readonly status: "loading" }
  | { readonly status: "ready"; readonly data: T }
  | { readonly status: "error"; readonly message: string };

type UserStatus = "all" | "active" | "disabled" | "unverified";
type UserSort = "newest" | "oldest" | "email";
type SyncResult =
  | { readonly kind: "snapshot"; readonly data: SnapshotResponse }
  | { readonly kind: "pull"; readonly data: SyncPullResponse };

const USERS_PER_PAGE = 10;

export function AuthRoute({ client, initialTab = "users", onNotice, onSessionChange }: AuthRouteProps) {
  const [session, setSession] = useState<Loadable<AuthTokenPair | null>>({ status: "loading" });
  const [settings, setSettings] = useState<Loadable<AuthSettings>>({ status: "loading" });
  const [users, setUsers] = useState<Loadable<readonly AuthUser[]>>({ status: "loading" });
  const [usersRevision, setUsersRevision] = useState(0);
  const [activeTab, setActiveTab] = useState<AuthRouteTab>(initialTab);
  const [sessionDialogEmail, setSessionDialogEmail] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    setSession({ status: "loading" });
    void client.currentSession().then(
      (value) => { if (current) setSession({ status: "ready", data: value }); },
      (cause: unknown) => { if (current) setSession({ status: "error", message: errorMessage(cause) }); },
    );
    return () => { current = false; };
  }, [client]);

  useEffect(() => {
    let current = true;
    setSettings({ status: "loading" });
    void client.authSettings().then(
      (value) => { if (current) setSettings({ status: "ready", data: value }); },
      (cause: unknown) => { if (current) setSettings({ status: "error", message: errorMessage(cause) }); },
    );
    return () => { current = false; };
  }, [client]);

  useEffect(() => {
    let current = true;
    setUsers({ status: "loading" });
    void client.authUsers().then(
      (value) => { if (current) setUsers({ status: "ready", data: value }); },
      (cause: unknown) => { if (current) setUsers({ status: "error", message: errorMessage(cause) }); },
    );
    return () => { current = false; };
  }, [client, usersRevision]);

  const updateSession = useCallback((value: AuthTokenPair | null) => {
    setSession({ status: "ready", data: value });
    onSessionChange?.(value);
  }, [onSessionChange]);

  return (
    <div className="auth-route">
      <header className="auth-route__intro">
        <span className="auth-route__eyebrow"><ShieldCheck size={14} /> Project authentication</span>
        <SessionPill session={session} />
      </header>

      <div className="auth-route__tabs" role="tablist" aria-label="Authentication tasks">
        <button id="auth-tab-users" type="button" role="tab" aria-controls="auth-panel-users" aria-selected={activeTab === "users"} tabIndex={activeTab === "users" ? 0 : -1} onClick={() => setActiveTab("users")} onKeyDown={(event) => handleAuthTabKeyDown(event, ["users", "policy"], 0, setActiveTab)}><UsersRound size={14} /> Users {users.status === "ready" ? <span>{users.data.length}</span> : null}</button>
        <button id="auth-tab-policy" type="button" role="tab" aria-controls="auth-panel-policy" aria-selected={activeTab === "policy"} tabIndex={activeTab === "policy" ? 0 : -1} onClick={() => { setActiveTab("policy"); setSessionDialogEmail(null); }} onKeyDown={(event) => handleAuthTabKeyDown(event, ["users", "policy"], 1, setActiveTab)}><SlidersHorizontal size={14} /> Policy</button>
      </div>

      {activeTab === "policy" ? <div id="auth-panel-policy" role="tabpanel" aria-labelledby="auth-tab-policy" tabIndex={0}><AuthSettingsCard client={client} resource={settings} onChange={setSettings} onNotice={onNotice} /></div> : null}
      {activeTab === "users" ? <div id="auth-panel-users" role="tabpanel" aria-labelledby="auth-tab-users" tabIndex={0}><AuthUsersCard
        resource={users}
        hasSession={session.status === "ready" && session.data !== null}
        onReload={() => setUsersRevision((value) => value + 1)}
        onTest={(email) => setSessionDialogEmail(email)}
        onToggle={async (user, disabled) => {
          await client.setAuthUserDisabled(user.id, disabled);
          setUsersRevision((value) => value + 1);
          onNotice?.(`${user.email} ${disabled ? "disabled" : "enabled"}`);
        }}
      /></div> : null}
      {sessionDialogEmail === null ? null : <SessionDialog client={client} initialEmail={sessionDialogEmail} session={session} onChange={updateSession} onClose={() => setSessionDialogEmail(null)} onNotice={onNotice} />}
    </div>
  );
}

function handleAuthTabKeyDown(event: KeyboardEvent<HTMLButtonElement>, tabs: readonly ("users" | "policy")[], index: number, onSelect: (tab: "users" | "policy") => void) {
  let nextIndex: number | null = null;
  if (event.key === "ArrowRight") nextIndex = (index + 1) % tabs.length;
  if (event.key === "ArrowLeft") nextIndex = (index - 1 + tabs.length) % tabs.length;
  if (event.key === "Home") nextIndex = 0;
  if (event.key === "End") nextIndex = tabs.length - 1;
  if (nextIndex === null) return;
  const nextTab = tabs[nextIndex];
  if (nextTab === undefined) return;
  event.preventDefault();
  onSelect(nextTab);
  event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role='tab']").item(nextIndex).focus();
}

function SessionPill({ session }: { readonly session: Loadable<AuthTokenPair | null> }) {
  if (session.status === "loading") return <span className="auth-status-pill is-loading"><RefreshCcw size={13} /> Checking session</span>;
  if (session.status === "error") return <span className="auth-status-pill is-warning"><CircleAlert size={13} /> Session unavailable</span>;
  if (session.data === null) return <span className="auth-status-pill"><UserRound size={13} /> No end-user session</span>;
  return <span className="auth-status-pill is-active"><CircleCheck size={13} /> Signed in as {session.data.user.email}</span>;
}

function SessionDialog({ client, initialEmail, session, onChange, onClose, onNotice }: {
  readonly client: FFDBClient;
  readonly initialEmail: string;
  readonly session: Loadable<AuthTokenPair | null>;
  readonly onChange: (session: AuthTokenPair | null) => void;
  readonly onClose: () => void;
  readonly onNotice: ((message: string) => void) | undefined;
}) {
  useEffect(() => {
    const closeOnEscape = (event: globalThis.KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    globalThis.addEventListener("keydown", closeOnEscape);
    return () => globalThis.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  return <div className="auth-session-dialog" role="presentation"><button className="auth-session-dialog__backdrop" type="button" aria-label="Close session tester" onClick={onClose} /><div role="dialog" aria-modal="true" aria-labelledby="end-user-session-title"><button autoFocus className="auth-session-dialog__close" type="button" aria-label="Close session tester" onClick={onClose}><X size={17} /></button><SessionCard client={client} initialEmail={initialEmail} session={session} onChange={onChange} onNotice={onNotice} /></div></div>;
}

function SessionCard({ client, initialEmail, session, onChange, onNotice }: {
  readonly client: FFDBClient;
  readonly initialEmail: string;
  readonly session: Loadable<AuthTokenPair | null>;
  readonly onChange: (session: AuthTokenPair | null) => void;
  readonly onNotice: ((message: string) => void) | undefined;
}) {
  const [email, setEmail] = useState(initialEmail);
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => { setEmail(initialEmail); }, [initialEmail]);

  const signIn = async (event: FormEvent) => {
    event.preventDefault();
    setPending(true);
    setError(null);
    try {
      const value = await client.auth.signIn(email.trim(), password);
      onChange(value);
      onNotice?.(`Signed in as ${value.user.email}`);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setPassword("");
      setPending(false);
    }
  };

  const signOut = async () => {
    setPending(true);
    setError(null);
    try {
      await client.auth.signOut();
      onChange(null);
      onNotice?.("End-user session signed out");
    } catch (cause) {
      if (isSessionError(cause)) {
        await client.setSession(null);
        onChange(null);
        onNotice?.("Expired local session cleared");
      } else {
        setError(errorMessage(cause));
      }
    } finally {
      setPending(false);
    }
  };

  return (
    <section className="auth-card auth-session-card" aria-labelledby="end-user-session-title">
      <CardHeading id="end-user-session-title" icon={<LogIn size={18} />} title="Test an end-user session" description="Used by RLS-protected data, storage, and offline sync calls in this browser." />
      {session.status === "loading" ? <CardLoading label="Loading end-user session" /> : null}
      {session.status === "error" ? <InlineError message={session.message} /> : null}
      {session.status === "ready" && session.data === null ? (
        <form className="auth-form" onSubmit={(event) => void signIn(event)}>
          <label>
            <span>Email</span>
            <input autoComplete="username" inputMode="email" type="email" value={email} onChange={(event) => setEmail(event.target.value)} placeholder="user@example.com" required />
          </label>
          <label>
            <span>Password</span>
            <input autoComplete="current-password" type="password" value={password} onChange={(event) => setPassword(event.target.value)} required />
          </label>
          {error === null ? null : <InlineError message={error} />}
          <button className="auth-button auth-button--primary" disabled={pending || email.trim() === "" || password === ""} type="submit">
            <LogIn size={15} /> {pending ? "Signing in…" : "Sign in"}
          </button>
          <p className="auth-form__hint">This is a project end user, separate from your FFDB portal account.</p>
        </form>
      ) : null}
      {session.status === "ready" && session.data !== null ? (
        <div className="auth-session-summary">
          <div className="auth-session-summary__identity">
            <span className="auth-avatar">{initials(session.data.user.email)}</span>
            <div>
              <strong>{session.data.user.email}</strong>
              <span>{session.data.user.role} · {session.data.user.email_verified ? "Verified email" : "Email verification pending"}</span>
            </div>
          </div>
          <dl className="auth-session-facts">
            <div><dt>Session</dt><dd>{shortIdentifier(session.data.session_id)}</dd></div>
            <div><dt>Token lifetime</dt><dd>{formatDuration(session.data.expires_in_seconds)}</dd></div>
          </dl>
          {error === null ? null : <InlineError message={error} />}
          <button className="auth-button auth-button--danger-quiet" disabled={pending} type="button" onClick={() => void signOut()}>
            <LogOut size={15} /> {pending ? "Signing out…" : "Sign out of this session"}
          </button>
        </div>
      ) : null}
    </section>
  );
}

function AuthSettingsCard({ client, resource, onChange, onNotice }: {
  readonly client: FFDBClient;
  readonly resource: Loadable<AuthSettings>;
  readonly onChange: (resource: Loadable<AuthSettings>) => void;
  readonly onNotice: ((message: string) => void) | undefined;
}) {
  const [draft, setDraft] = useState<AuthSettings | null>(resource.status === "ready" ? resource.data : null);
  const [baseline, setBaseline] = useState<AuthSettings | null>(resource.status === "ready" ? resource.data : null);
  const [webOriginsText, setWebOriginsText] = useState(resource.status === "ready" ? resource.data.allowed_web_origins.join("\n") : "");
  const [authRedirectsText, setAuthRedirectsText] = useState(resource.status === "ready" ? resource.data.allowed_auth_redirects.join("\n") : "");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (resource.status !== "ready") return;
    setDraft(resource.data);
    setBaseline(resource.data);
    setWebOriginsText(resource.data.allowed_web_origins.join("\n"));
    setAuthRedirectsText(resource.data.allowed_auth_redirects.join("\n"));
  }, [resource]);

  const webOrigins = parseApplicationUrls(webOriginsText, "origin");
  const authRedirects = parseApplicationUrls(authRedirectsText, "redirect");
  const urlError = webOrigins.error ?? authRedirects.error;
  const proposed = draft === null ? null : {
    ...draft,
    allowed_web_origins: webOrigins.values,
    allowed_auth_redirects: authRedirects.values,
  };
  const dirty = proposed !== null && baseline !== null && JSON.stringify(proposed) !== JSON.stringify(baseline);

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (proposed === null || urlError !== null) return;
    setPending(true);
    setError(null);
    try {
      const value = await client.updateAuthSettings(proposed);
      setDraft(value);
      setBaseline(value);
      setWebOriginsText(value.allowed_web_origins.join("\n"));
      setAuthRedirectsText(value.allowed_auth_redirects.join("\n"));
      onChange({ status: "ready", data: value });
      onNotice?.("Authentication settings saved");
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setPending(false);
    }
  };

  return (
    <section className="auth-card auth-settings-card" aria-labelledby="auth-policy-title">
      <CardHeading id="auth-policy-title" icon={<SlidersHorizontal size={18} />} title="Authentication policy" description="Defaults applied to new registrations and issued sessions." />
      {resource.status === "loading" ? <CardLoading label="Loading authentication policy" /> : null}
      {resource.status === "error" ? <InlineError message={resource.message} /> : null}
      {draft === null ? null : (
        <form className="auth-settings-form" onSubmit={(event) => void save(event)}>
          <div className="auth-switch-list">
            <SwitchRow
              checked={draft.registration_enabled}
              label="Allow new registrations"
              description="New users may create an account for this project."
              onChange={(checked) => setDraft({ ...draft, registration_enabled: checked })}
            />
            <SwitchRow
              checked={draft.email_verification_required}
              label="Require verified email"
              description="Users must verify their address before protected access."
              onChange={(checked) => setDraft({ ...draft, email_verification_required: checked })}
            />
          </div>
          <div className="auth-number-grid">
            <NumberInput label="Minimum password length" value={draft.password_min_length} min={8} max={128} onChange={(value) => setDraft({ ...draft, password_min_length: value })} />
            <NumberInput label="Access token TTL" value={draft.access_token_ttl_seconds} min={60} suffix="seconds" onChange={(value) => setDraft({ ...draft, access_token_ttl_seconds: value })} />
            <NumberInput label="Refresh token TTL" value={draft.refresh_token_ttl_seconds} min={60} suffix="seconds" onChange={(value) => setDraft({ ...draft, refresh_token_ttl_seconds: value })} />
          </div>
          <div className="auth-application-urls">
            <div className="auth-application-urls__heading">
              <div>
                <strong>Application URLs</strong>
                <span>Project-scoped browser and auth destinations. Changes take effect immediately—no server restart.</span>
              </div>
              <span>Up to 20 each</span>
            </div>
            <div className="auth-url-grid">
              <label>
                <span>Allowed web origins</span>
                <small>Browser origins permitted to call this project’s API. One origin per line; no path.</small>
                <textarea
                  aria-describedby="allowed-web-origins-hint"
                  placeholder={"http://localhost:5180\nhttps://app.example.com"}
                  spellCheck={false}
                  value={webOriginsText}
                  onChange={(event) => setWebOriginsText(event.target.value)}
                />
                <small id="allowed-web-origins-hint">Use the exact scheme, host, and port your browser shows.</small>
              </label>
              <label>
                <span>Allowed auth redirects</span>
                <small>Exact pages FFDB may return to after verification or password reset.</small>
                <textarea
                  aria-describedby="allowed-auth-redirects-hint"
                  placeholder={"http://localhost:5180/?ffdb_auth=verified\nhttp://localhost:5180/?ffdb_auth=password-reset"}
                  spellCheck={false}
                  value={authRedirectsText}
                  onChange={(event) => setAuthRedirectsText(event.target.value)}
                />
                <small id="allowed-auth-redirects-hint">Paths, query strings, and fragments must match exactly.</small>
              </label>
            </div>
          </div>
          {urlError === null ? null : <InlineError message={urlError} />}
          {error === null ? null : <InlineError message={error} />}
          <div className="auth-form-actions">
            <span>{dirty ? "Unsaved policy changes" : "Policy is up to date"}</span>
            <button className="auth-button auth-button--primary" disabled={!dirty || pending || urlError !== null} type="submit">
              {pending ? "Saving…" : "Save policy"}
            </button>
          </div>
        </form>
      )}
    </section>
  );
}

function parseApplicationUrls(value: string, kind: "origin" | "redirect"): { readonly values: readonly string[]; readonly error: string | null } {
  const lines = value.split(/\r?\n/u).map((line) => line.trim()).filter(Boolean);
  if (lines.length > 20) return { values: [], error: `Use no more than 20 ${kind === "origin" ? "web origins" : "auth redirects"}.` };
  const values: string[] = [];
  for (const line of lines) {
    let url: URL;
    try {
      url = new URL(line);
    } catch {
      return { values: [], error: `“${line}” is not a valid absolute URL.` };
    }
    if (!(["http:", "https:"] as const).includes(url.protocol as "http:" | "https:") || url.username !== "" || url.password !== "") {
      return { values: [], error: `“${line}” must be an HTTP(S) URL without embedded credentials.` };
    }
    if (kind === "origin" && (url.pathname !== "/" || url.search !== "" || url.hash !== "")) {
      return { values: [], error: `“${line}” includes a path, query, or fragment; web origins stop after the port.` };
    }
    const normalized = kind === "origin" ? url.origin : url.href;
    if (!values.includes(normalized)) values.push(normalized);
  }
  return { values, error: null };
}

function AuthUsersCard({ resource, hasSession, onReload, onTest, onToggle }: {
  readonly resource: Loadable<readonly AuthUser[]>;
  readonly hasSession: boolean;
  readonly onReload: () => void;
  readonly onTest: (email: string) => void;
  readonly onToggle: (user: AuthUser, disabled: boolean) => Promise<void>;
}) {
  const [search, setSearch] = useState("");
  const [status, setStatus] = useState<UserStatus>("all");
  const [sort, setSort] = useState<UserSort>("newest");
  const [page, setPage] = useState(1);
  const [pendingUserId, setPendingUserId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const filteredUsers = useMemo(() => {
    if (resource.status !== "ready") return [];
    const query = search.trim().toLocaleLowerCase();
    return resource.data
      .filter((user) => query === "" || user.email.toLocaleLowerCase().includes(query) || user.role.toLocaleLowerCase().includes(query))
      .filter((user) => status === "all"
        || (status === "active" && !user.disabled)
        || (status === "disabled" && user.disabled)
        || (status === "unverified" && !user.email_verified))
      .toSorted((left, right) => {
        if (sort === "email") return left.email.localeCompare(right.email);
        return sort === "oldest" ? left.created_at_ms - right.created_at_ms : right.created_at_ms - left.created_at_ms;
      });
  }, [resource, search, sort, status]);

  useEffect(() => { setPage(1); }, [search, sort, status]);
  const pageCount = Math.max(1, Math.ceil(filteredUsers.length / USERS_PER_PAGE));
  const currentPage = Math.min(page, pageCount);
  const visibleUsers = filteredUsers.slice((currentPage - 1) * USERS_PER_PAGE, currentPage * USERS_PER_PAGE);

  const toggle = async (user: AuthUser) => {
    const disabled = !user.disabled;
    if (disabled && !globalThis.confirm(`Disable ${user.email}? Existing sessions will be rejected.`)) return;
    setPendingUserId(user.id);
    setError(null);
    try {
      await onToggle(user, disabled);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setPendingUserId(null);
    }
  };

  return (
    <section className="auth-card auth-users-card" aria-labelledby="project-auth-users-title">
      <div className="auth-users-card__header">
        <CardHeading id="project-auth-users-title" icon={<UsersRound size={18} />} title="Project auth users" description="Registered application users; password hashes and token material are never returned." />
        <div className="auth-users-card__actions"><button className="auth-button auth-button--quiet" type="button" onClick={() => onTest("")}><LogIn size={14} /> {hasSession ? "Manage session" : "Test credentials"}</button><button className="auth-button auth-button--quiet" type="button" onClick={onReload}><RefreshCcw size={14} /> Refresh</button></div>
      </div>
      <div className="auth-table-toolbar">
        <label className="auth-search">
          <Search size={15} />
          <span className="sr-only">Search users</span>
          <input type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search email or role" />
        </label>
        <label className="auth-select"><span>Status</span><select value={status} onChange={(event) => setStatus(event.target.value as UserStatus)}><option value="all">All users</option><option value="active">Active</option><option value="disabled">Disabled</option><option value="unverified">Unverified</option></select></label>
        <label className="auth-select"><span>Sort</span><select value={sort} onChange={(event) => setSort(event.target.value as UserSort)}><option value="newest">Newest first</option><option value="oldest">Oldest first</option><option value="email">Email A–Z</option></select></label>
      </div>
      {error === null ? null : <InlineError message={error} />}
      {resource.status === "loading" ? <CardLoading label="Loading project auth users" /> : null}
      {resource.status === "error" ? <InlineError message={resource.message} /> : null}
      {resource.status === "ready" && resource.data.length === 0 ? <EmptyPanel title="No registered users" detail="Users will appear here after they register through your application." /> : null}
      {resource.status === "ready" && resource.data.length > 0 && filteredUsers.length === 0 ? <EmptyPanel title="No matching users" detail="Clear the search or change the status filter." /> : null}
      {visibleUsers.length > 0 ? (
        <div className="auth-table-scroll portal-table-scroll" role="region" aria-label="Project authentication users" tabIndex={0}>
          <table className="auth-users-table">
            <thead><tr><th>User</th><th>Role</th><th>Verification</th><th>Status</th><th>Created</th><th><span className="sr-only">Actions</span></th></tr></thead>
            <tbody>{visibleUsers.map((user) => (
              <tr key={user.id}>
                <td data-label="User"><div className="auth-user-cell"><span className="auth-avatar auth-avatar--small">{initials(user.email)}</span><div><strong>{user.email}</strong><span>{shortIdentifier(user.id)}</span></div></div></td>
                <td data-label="Role"><span className="auth-role">{user.role}</span></td>
                <td data-label="Verification"><span className={`auth-badge ${user.email_verified ? "is-success" : "is-warning"}`}>{user.email_verified ? "Verified" : "Pending"}</span></td>
                <td data-label="Status"><span className={`auth-badge ${user.disabled ? "is-neutral" : "is-success"}`}>{user.disabled ? "Disabled" : "Active"}</span></td>
                <td data-label="Created"><time dateTime={new Date(user.created_at_ms).toISOString()}>{formatDate(user.created_at_ms)}</time></td>
                <td data-label="Action"><div className="auth-row-actions"><button className="auth-table-action" disabled={user.disabled} type="button" onClick={() => onTest(user.email)}><LogIn size={12} /> Test</button><button className={`auth-table-action ${user.disabled ? "" : "is-danger"}`} disabled={pendingUserId === user.id} type="button" onClick={() => void toggle(user)}>{pendingUserId === user.id ? "Saving…" : user.disabled ? "Enable" : "Disable"}</button></div></td>
              </tr>
            ))}</tbody>
          </table>
        </div>
      ) : null}
      {filteredUsers.length > 0 ? (
        <footer className="auth-table-footer">
          <span>Showing {(currentPage - 1) * USERS_PER_PAGE + 1}–{Math.min(currentPage * USERS_PER_PAGE, filteredUsers.length)} of {filteredUsers.length}</span>
          <div className="auth-pagination" aria-label="User table pagination">
            <button aria-label="Previous page" disabled={currentPage === 1} type="button" onClick={() => setPage((value) => Math.max(1, value - 1))}><ChevronLeft size={15} /></button>
            <span>Page {currentPage} of {pageCount}</span>
            <button aria-label="Next page" disabled={currentPage === pageCount} type="button" onClick={() => setPage((value) => Math.min(pageCount, value + 1))}><ChevronRight size={15} /></button>
          </div>
        </footer>
      ) : null}
    </section>
  );
}

export function SyncRoute({ client, onManageSession, onNotice }: SyncRouteProps) {
  const [session, setSession] = useState<Loadable<AuthTokenPair | null>>({ status: "loading" });
  const [result, setResult] = useState<SyncResult | null>(null);
  const [pending, setPending] = useState<"snapshot" | "pull" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sessionNotice, setSessionNotice] = useState<string | null>(null);

  const refreshSession = useCallback(async () => {
    setSession({ status: "loading" });
    try {
      setSession({ status: "ready", data: await client.currentSession() });
    } catch (cause) {
      setSession({ status: "error", message: errorMessage(cause) });
    }
  }, [client]);

  useEffect(() => { void refreshSession(); }, [refreshSession]);

  const run = async (kind: "snapshot" | "pull") => {
    if (session.status !== "ready" || session.data === null) return;
    setPending(kind);
    setError(null);
    setSessionNotice(null);
    try {
      if (kind === "snapshot") {
        setResult({ kind, data: await client.sync.snapshot() });
        onNotice?.("RLS-filtered snapshot fetched");
      } else {
        setResult({ kind, data: await client.sync.pull(null, 100) });
        onNotice?.("Latest sync changes pulled");
      }
    } catch (cause) {
      if (isSessionError(cause)) {
        await client.setSession(null);
        setSession({ status: "ready", data: null });
        setResult(null);
        setSessionNotice("Your end-user session ended. Sign in again before requesting protected sync data.");
      } else {
        setError(errorMessage(cause));
      }
    } finally {
      setPending(null);
    }
  };

  const activeSession = session.status === "ready" ? session.data : null;
  return (
    <div className="sync-route">
      <header className="sync-route__intro">
        <div>
          <span className="auth-route__eyebrow"><RefreshCcw size={14} /> Offline delivery</span>
          <h2>Inspect the signed-in user’s sync scope</h2>
          <p>Snapshots and change pulls use the same verified end-user session and row-level security policies as your application.</p>
        </div>
        {activeSession === null ? null : <span className="auth-status-pill is-active"><CircleCheck size={13} /> {activeSession.user.email}</span>}
      </header>

      {session.status === "loading" ? <section className="sync-prerequisite"><CardLoading label="Checking end-user session" /></section> : null}
      {session.status === "error" ? <section className="sync-prerequisite"><InlineError message={session.message} /><button className="auth-button auth-button--quiet" type="button" onClick={() => void refreshSession()}><RefreshCcw size={14} /> Retry session check</button></section> : null}
      {session.status === "ready" && session.data === null ? (
        <section className="sync-prerequisite" aria-labelledby="sync-session-required-title">
          <div className="sync-prerequisite__icon"><ShieldCheck size={24} /></div>
          <div className="sync-prerequisite__copy">
            <span>Session required</span>
            <h3 id="sync-session-required-title">Sign in as an application user first</h3>
            <p>FFDB cannot fetch a snapshot without a user identity because every returned row must pass that user’s RLS policies.</p>
            {sessionNotice === null ? null : <div className="sync-session-notice" role="status"><Clock3 size={15} /> {sessionNotice}</div>}
          </div>
          <div className="sync-prerequisite__steps">
            <ol>
              <li><span>1</span><div><strong>Open user session</strong><small>Use a registered project end user.</small></div></li>
              <li><span>2</span><div><strong>Return to Sync</strong><small>The session is kept in this browser.</small></div></li>
              <li><span>3</span><div><strong>Fetch protected data</strong><small>Only policy-authorized rows are returned.</small></div></li>
            </ol>
            <button className="auth-button auth-button--primary" type="button" onClick={onManageSession}><LogIn size={15} /> Sign in on Auth</button>
          </div>
        </section>
      ) : null}

      {session.status === "ready" && session.data !== null ? (
        <>
          <section className="sync-command-card">
            <div className="sync-command-card__identity">
              <span className="auth-avatar">{initials(session.data.user.email)}</span>
              <div><span>Active RLS identity</span><strong>{session.data.user.email}</strong></div>
            </div>
            <div className="sync-command-card__actions">
              <button className="auth-button auth-button--primary" disabled={pending !== null} type="button" onClick={() => void run("snapshot")}><Database size={15} /> {pending === "snapshot" ? "Fetching…" : "Fetch snapshot"}</button>
              <button className="auth-button auth-button--quiet" disabled={pending !== null} type="button" onClick={() => void run("pull")}><RefreshCcw className={pending === "pull" ? "is-spinning" : ""} size={15} /> {pending === "pull" ? "Pulling…" : "Pull changes"}</button>
              <button className="auth-button auth-button--text" type="button" onClick={onManageSession}>Manage session</button>
            </div>
          </section>
          <p className="sync-safety-note"><ShieldCheck size={15} /> Results below reflect this user’s current policy scope. They are an inspection aid, not a bypass around RLS.</p>
          {error === null ? null : <InlineError message={error} />}
          {result === null ? <EmptyPanel title="No sync request yet" detail="Fetch a full snapshot for initial hydration, or pull the first page of changes from the beginning of the log." /> : <SyncResultPanel result={result} />}
        </>
      ) : null}
    </div>
  );
}

function SyncResultPanel({ result }: { readonly result: SyncResult }) {
  if (result.kind === "snapshot") {
    const tables = Object.entries(result.data.tables);
    return (
      <section className="sync-result-card" aria-labelledby="snapshot-result-title">
        <div className="sync-result-card__heading"><div><span>Snapshot result</span><h3 id="snapshot-result-title">{tables.length} {tables.length === 1 ? "table" : "tables"} at schema v{result.data.schema_version}</h3></div><span className="auth-badge is-success">Complete</span></div>
        <dl className="sync-result-metrics"><div><dt>Tables</dt><dd>{tables.length}</dd></div><div><dt>Rows</dt><dd>{tables.reduce((count, [, table]) => count + table.rows.length, 0)}</dd></div><div><dt>Cursor</dt><dd title={result.data.cursor}>{shortIdentifier(result.data.cursor)}</dd></div></dl>
        {tables.length === 0 ? <EmptyPanel title="No visible tables" detail="No rows or tables are visible to this user under the current RLS policy." /> : (
          <div className="auth-table-scroll portal-table-scroll" role="region" aria-label="Offline snapshot tables" tabIndex={0}><table className="sync-result-table"><thead><tr><th>Table</th><th>Columns</th><th>Rows</th><th>Truncated</th></tr></thead><tbody>{tables.map(([name, table]) => <tr key={name}><td data-label="Table"><strong>{name}</strong></td><td data-label="Columns">{table.columns.length}</td><td data-label="Rows">{table.rows.length}</td><td data-label="Truncated">{table.truncated ? "Yes" : "No"}</td></tr>)}</tbody></table></div>
        )}
        <RawResult value={result.data} />
      </section>
    );
  }

  return (
    <section className="sync-result-card" aria-labelledby="pull-result-title">
      <div className="sync-result-card__heading"><div><span>Change pull</span><h3 id="pull-result-title">{result.data.changes.length} {result.data.changes.length === 1 ? "change" : "changes"} returned</h3></div><span className={`auth-badge ${result.data.has_more ? "is-warning" : "is-success"}`}>{result.data.has_more ? "More available" : "Caught up"}</span></div>
      <dl className="sync-result-metrics"><div><dt>Changes</dt><dd>{result.data.changes.length}</dd></div><div><dt>More pages</dt><dd>{result.data.has_more ? "Yes" : "No"}</dd></div><div><dt>Cursor</dt><dd title={result.data.cursor}>{shortIdentifier(result.data.cursor)}</dd></div></dl>
      {result.data.control === null ? null : <div className="sync-control-note"><CircleAlert size={16} /><div><strong>{result.data.control.type.replaceAll("_", " ")}</strong><span>{"reason" in result.data.control ? result.data.control.reason : "The current user scope changed."}</span></div></div>}
      {result.data.changes.length === 0 ? <EmptyPanel title="No changes to apply" detail="This user is caught up from the requested cursor." /> : (
        <div className="auth-table-scroll portal-table-scroll" role="region" aria-label="Offline sync changes" tabIndex={0}><table className="sync-result-table"><thead><tr><th>Sequence</th><th>Table</th><th>Operation</th><th>Actor</th><th>Committed</th></tr></thead><tbody>{result.data.changes.slice(0, 100).map((change) => <tr key={`${change.sequence}-${change.transaction_id}`}><td data-label="Sequence">{change.sequence}</td><td data-label="Table"><strong>{change.table}</strong></td><td data-label="Operation"><span className="auth-role">{change.operation}</span></td><td data-label="Actor">{change.actor ?? "System"}</td><td data-label="Committed">{formatDate(change.committed_at_ms)}</td></tr>)}</tbody></table></div>
      )}
      <RawResult value={result.data} />
    </section>
  );
}

function CardHeading({ id, icon, title, description }: { readonly id: string; readonly icon: React.ReactNode; readonly title: string; readonly description: string }) {
  return <div className="auth-card-heading"><span>{icon}</span><div><h3 id={id}>{title}</h3><p>{description}</p></div></div>;
}

function SwitchRow({ checked, label, description, onChange }: { readonly checked: boolean; readonly label: string; readonly description: string; readonly onChange: (checked: boolean) => void }) {
  return <div className="auth-switch-row"><div><strong>{label}</strong><span>{description}</span></div><button aria-checked={checked} aria-label={label} className="auth-switch" role="switch" type="button" onClick={() => onChange(!checked)}><span /></button></div>;
}

function NumberInput({ label, value, min, max, suffix, onChange }: { readonly label: string; readonly value: number; readonly min: number; readonly max?: number; readonly suffix?: string; readonly onChange: (value: number) => void }) {
  return <label className="auth-number-input"><span>{label}</span><div><input min={min} max={max} type="number" value={value} onChange={(event) => { const next = Number(event.target.value); if (Number.isFinite(next)) onChange(next); }} />{suffix === undefined ? null : <small>{suffix}</small>}</div></label>;
}

function CardLoading({ label }: { readonly label: string }) {
  return <div className="auth-loading" role="status"><RefreshCcw className="is-spinning" size={16} /><span>{label}…</span></div>;
}

function InlineError({ message }: { readonly message: string }) {
  return <div className="auth-inline-error" role="alert"><CircleAlert size={16} /><span>{message}</span></div>;
}

function EmptyPanel({ title, detail }: { readonly title: string; readonly detail: string }) {
  return <div className="auth-empty-panel"><Database size={21} /><div><strong>{title}</strong><span>{detail}</span></div></div>;
}

function RawResult({ value }: { readonly value: unknown }) {
  return <details className="sync-raw-result"><summary>View raw response</summary><pre>{JSON.stringify(value, null, 2)}</pre></details>;
}

function isSessionError(cause: unknown): boolean {
  return cause instanceof FFDBError && (cause.status === 401 || cause.code.startsWith("auth.session") || cause.code.includes("token_expired"));
}

function errorMessage(cause: unknown): string {
  if (cause instanceof FFDBError) return `${cause.message}${cause.requestId === null ? "" : ` · Request ${cause.requestId}`}`;
  return cause instanceof Error ? cause.message : "The request could not be completed.";
}

function initials(email: string): string {
  const name = email.split("@", 1)[0] ?? email;
  const parts = name.split(/[._-]+/).filter(Boolean);
  return (parts.length > 1 ? `${parts[0]?.[0] ?? ""}${parts[1]?.[0] ?? ""}` : name.slice(0, 2)).toUpperCase();
}

function shortIdentifier(value: string): string {
  if (value.length <= 18) return value;
  return `${value.slice(0, 8)}…${value.slice(-6)}`;
}

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds} seconds`;
  if (seconds < 3_600) return `${Math.round(seconds / 60)} minutes`;
  if (seconds < 86_400) return `${Math.round(seconds / 3_600)} hours`;
  return `${Math.round(seconds / 86_400)} days`;
}

function formatDate(value: number): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(value);
}

import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";

import {
  FFDBClient,
  FFDBError,
  type HostUpdateJob,
  type HostUpdateOperation,
  type HostUpdateRelease,
  type HostUpdateSettings,
  type HostUpdateStatus,
} from "@ffdb/client";

import { Icon } from "../icons.js";
import "./instance-updates.css";

type PendingOperation =
  | { readonly kind: "install"; readonly version: string }
  | { readonly kind: "rollback"; readonly version: string }
  | { readonly kind: "settings"; readonly settings: HostUpdateSettings };

interface ConfirmationState {
  readonly operation: PendingOperation;
  readonly requiresReauthentication: boolean;
}

const defaultSettings: HostUpdateSettings = {
  channel: "stable",
  automatic_checks: true,
  check_interval_hours: 24,
  automatic_apply: false,
  maintenance_window_start: null,
  maintenance_window_duration_minutes: 60,
};

export function InstanceUpdatesPanel({ client, onNotice, onUpdateAvailability }: {
  readonly client: FFDBClient;
  onNotice(message: string): void;
  onUpdateAvailability?(available: boolean): void;
}) {
  const [status, setStatus] = useState<HostUpdateStatus | null>(null);
  const [settings, setSettings] = useState<HostUpdateSettings>(defaultSettings);
  const [job, setJob] = useState<HostUpdateJob | null>(null);
  const [loading, setLoading] = useState(true);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unsupportedReason, setUnsupportedReason] = useState<string | null>(null);
  const [reconnecting, setReconnecting] = useState(false);
  const [confirmation, setConfirmation] = useState<ConfirmationState | null>(null);
  const [password, setPassword] = useState("");
  const [confirming, setConfirming] = useState(false);

  const loadStatus = useCallback(async (): Promise<HostUpdateStatus> => {
    const next = await client.hostUpdateStatus({ retry: false });
    setStatus(next);
    setUnsupportedReason(null);
    setSettings(next.settings);
    onUpdateAvailability?.(next.supported && next.update_available);
    if (next.active_job !== null && isActiveJob(next.active_job)) setJob(next.active_job);
    return next;
  }, [client, onUpdateAvailability]);

  useEffect(() => {
    let current = true;
    setLoading(true);
    setError(null);
    void loadStatus().catch((cause) => {
      if (!current) return;
      if (cause instanceof FFDBError && cause.status === 404) setUnsupportedReason("This FFDB API predates portal-managed host updates.");
      else setError(updateErrorMessage(cause));
    }).finally(() => {
      if (current) setLoading(false);
    });
    return () => { current = false; };
  }, [loadStatus]);

  useEffect(() => {
    if (job === null || !isActiveJob(job)) return;
    let current = true;
    let timer: ReturnType<typeof globalThis.setTimeout> | undefined;
    const poll = async () => {
      try {
        const next = await client.hostUpdateJob(job.job_id, { retry: false });
        if (!current) return;
        setJob(next);
        setReconnecting(false);
        setError(next.state === "failed" ? next.message || "The host operation failed." : null);
        if (isActiveJob(next)) {
          timer = globalThis.setTimeout(() => void poll(), 1_500);
          return;
        }
        if (next.state === "succeeded") {
          const refreshed = await loadStatus();
          if (!current) return;
          onNotice(completionNotice(next, refreshed));
        }
      } catch (cause) {
        if (!current) return;
        setReconnecting(true);
        timer = globalThis.setTimeout(() => void poll(), 2_000);
      }
    };
    timer = globalThis.setTimeout(() => void poll(), 650);
    return () => {
      current = false;
      if (timer !== undefined) globalThis.clearTimeout(timer);
    };
  }, [client, job?.job_id, job?.state, loadStatus, onNotice]);

  const beginJob = (next: HostUpdateJob) => {
    setJob(next);
    setError(null);
    setReconnecting(false);
  };

  const checkForUpdates = async () => {
    setChecking(true);
    setError(null);
    try {
      const next = await client.checkForHostUpdate({ idempotencyKey: operationId("update-check"), retry: false });
      beginJob(next);
    } catch (cause) {
      setError(updateErrorMessage(cause));
    } finally {
      setChecking(false);
    }
  };

  const requestOperation = async (operation: PendingOperation) => {
    const options = { idempotencyKey: operationId(`host-${operation.kind}`), retry: false } as const;
    const next = operation.kind === "settings"
      ? await client.configureHostUpdates(operation.settings, options)
      : operation.kind === "install"
        ? await client.installHostUpdate(operation.version, options)
        : await client.rollbackHostUpdate(operation.version, options);
    beginJob(next);
    closeConfirmation();
  };

  const performConfirmedOperation = async () => {
    if (confirmation === null) return;
    setConfirming(true);
    setError(null);
    try {
      await requestOperation(confirmation.operation);
    } catch (cause) {
      if (isReauthenticationRequired(cause)) {
        setConfirmation({ ...confirmation, requiresReauthentication: true });
      } else {
        setError(updateErrorMessage(cause));
      }
    } finally {
      setConfirming(false);
    }
  };

  const reauthenticateAndRetry = async (event: FormEvent) => {
    event.preventDefault();
    if (confirmation === null) return;
    setConfirming(true);
    setError(null);
    try {
      const session = await client.developerSession();
      if (session === null) throw new Error("Your administrator session has ended. Sign in again before continuing.");
      await client.developerSignIn(session.email, password, { retry: false });
      await requestOperation(confirmation.operation);
    } catch (cause) {
      setError(updateErrorMessage(cause));
    } finally {
      setPassword("");
      setConfirming(false);
    }
  };

  const closeConfirmation = useCallback(() => {
    setConfirmation(null);
    setPassword("");
  }, []);

  const submitSettings = (event: FormEvent) => {
    event.preventDefault();
    if (settings.automatic_apply && settings.maintenance_window_start === null) {
      setError("Choose a UTC maintenance-window start before enabling automatic installation.");
      return;
    }
    setConfirmation({ operation: { kind: "settings", settings }, requiresReauthentication: false });
  };

  const rollbackReleases = useMemo(
    () => status?.releases.filter((release) => !release.active && (!status.update_available || release.version !== status.available_version)) ?? [],
    [status],
  );
  const activeJob = job !== null && isActiveJob(job);

  if (loading) return <section className="updates-loading" role="status"><span className="access-spinner" /><div><h2>Loading host updates</h2><p>Reading the installed release and signed update channel.</p></div></section>;
  if (unsupportedReason !== null) return <UpdateUnsupported reason={unsupportedReason} />;
  if (status === null) return <section className="updates-page"><UpdateError message={error ?? "Host updates are unavailable."} onRetry={() => { setLoading(true); setUnsupportedReason(null); void loadStatus().catch((cause) => { if (cause instanceof FFDBError && cause.status === 404) setUnsupportedReason("This FFDB API predates portal-managed host updates."); else setError(updateErrorMessage(cause)); }).finally(() => setLoading(false)); }} /></section>;
  if (!status.supported) return <UpdateUnsupported reason={status.unavailable_reason ?? "This installation does not expose the native updater service."} />;

  return <div className="updates-page">
    <header className="updates-heading">
      <div><span><Icon name="sync" size={15} />INSTANCE LIFECYCLE</span><h1>Updates</h1><p>Install verified FFDB releases, control maintenance checks, and roll back to a compatible release.</p></div>
      <button className="secondary-action" disabled={activeJob || checking || !status.capabilities.check} type="button" onClick={() => void checkForUpdates()}><Icon name="sync" size={15} />{checking ? "Starting check…" : "Check for updates"}</button>
    </header>

    {status.update_available && status.available_version !== null ? <section className="update-available" aria-label="Update available">
      <span className="update-available-icon"><Icon name="backup" size={20} /></span>
      <div><strong>FFDB {status.available_version} is available</strong><p>The updater will verify the official release signature, create a backup, install atomically, and require health checks to pass.</p></div>
      <button className="primary-action" disabled={activeJob || !status.capabilities.install} type="button" onClick={() => setConfirmation({ operation: { kind: "install", version: status.available_version! }, requiresReauthentication: false })}>Review update</button>
    </section> : null}

    {error === null ? null : <UpdateError message={error} onDismiss={() => setError(null)} />}

    {job === null ? null : <JobProgress job={job} reconnecting={reconnecting} />}

    <section className="updates-release-grid" aria-label="Release status">
      <article className="update-release-card">
        <div className="update-card-icon"><Icon name="database" size={20} /></div><span>Installed release</span><strong>{status.installed_version ?? "Unknown"}</strong><p>{status.last_check_at_ms === null ? "The release channel has not been checked yet." : `Last checked ${formatTimestamp(status.last_check_at_ms)}.`}</p>
      </article>
      <article className="update-release-card">
        <div className="update-card-icon"><Icon name={status.update_available ? "backup" : "check"} size={20} /></div><span>Stable channel</span><strong>{status.available_version ?? "No newer release"}</strong><p>{status.update_available ? "Ready for administrator review." : "This host is on the newest release reported by the stable channel."}</p>
      </article>
      <article className="update-release-card update-trust-card">
        <div className="update-card-icon"><Icon name="shield" size={20} /></div><span>Release trust</span><strong>Verification required</strong><p>Only official stable-channel artifacts with a valid signature and checksum can be installed.</p>
      </article>
    </section>

    <div className="updates-content-grid">
      <section className="updates-surface">
        <div className="updates-section-heading"><div><h2>Update policy</h2><p>Checks are automatic. Installation remains manual unless you explicitly enable a UTC maintenance window.</p></div><span className="update-channel-badge">Stable</span></div>
        <form className="updates-settings" onSubmit={submitSettings}>
          <label className="update-switch-row"><span><strong>Automatic release checks</strong><small>Read the signed stable channel on a recurring schedule.</small></span><input checked={settings.automatic_checks} disabled={!status.capabilities.automatic_checks} type="checkbox" onChange={(event) => setSettings({ ...settings, automatic_checks: event.target.checked })} /></label>
          <label className="update-field"><span>Check interval</span><div><input aria-label="Update check interval" disabled={!status.capabilities.automatic_checks} max={168} min={1} type="number" value={settings.check_interval_hours} onChange={(event) => setSettings({ ...settings, check_interval_hours: Number(event.target.value) })} /><small>hours</small></div></label>
          <label className="update-switch-row"><span><strong>Install automatically</strong><small>Create a backup and install only during the configured window. Off by default.</small></span><input checked={settings.automatic_apply} disabled={!status.capabilities.automatic_apply} type="checkbox" onChange={(event) => setSettings({ ...settings, automatic_apply: event.target.checked, maintenance_window_start: event.target.checked ? settings.maintenance_window_start ?? "03:00" : null })} /></label>
          <div className="update-window-fields">
            <label className="update-field"><span>Window starts (UTC)</span><input aria-label="Maintenance window start UTC" disabled={!settings.automatic_apply || !status.capabilities.automatic_apply} type="time" value={settings.maintenance_window_start ?? "03:00"} onChange={(event) => setSettings({ ...settings, maintenance_window_start: event.target.value })} /></label>
            <label className="update-field"><span>Window duration</span><div><input aria-label="Maintenance window duration" disabled={!settings.automatic_apply || !status.capabilities.automatic_apply} max={1440} min={15} type="number" value={settings.maintenance_window_duration_minutes} onChange={(event) => setSettings({ ...settings, maintenance_window_duration_minutes: Number(event.target.value) })} /><small>minutes</small></div></label>
          </div>
          <div className="updates-settings-actions"><p><Icon name="lock" size={13} />Changing update policy requires a recent administrator sign-in.</p><button className="secondary-action" disabled={activeJob || settingsEqual(settings, status.settings) || (!status.capabilities.automatic_checks && !status.capabilities.automatic_apply)} type="submit">Save policy</button></div>
        </form>
      </section>

      <section className="updates-surface">
        <div className="updates-section-heading"><div><h2>Rollback</h2><p>Return to a release already installed on this host. FFDB blocks targets that are incompatible with the current data format.</p></div></div>
        <div className="rollback-list">
          {rollbackReleases.length === 0 ? <div className="rollback-empty"><Icon name="backup" size={20} /><strong>No earlier releases installed</strong><p>A rollback target appears after this host completes its first side-by-side update.</p></div> : rollbackReleases.map((release) => <div className="rollback-release" key={release.version}><div><strong>FFDB {release.version}</strong><span className={release.rollback_compatible ? "compatibility-badge is-compatible" : "compatibility-badge"}>{release.rollback_compatible ? "Compatible" : "Blocked"}</span><p>{release.rollback_compatible ? `State schema ${release.state_schema}. Eligible for an atomic rollback with health verification.` : `State schema ${release.state_schema}. This release cannot safely read the current FFDB data format.`}</p></div><button className="secondary-action" disabled={activeJob || !release.rollback_compatible || !status.capabilities.rollback} type="button" onClick={() => setConfirmation({ operation: { kind: "rollback", version: release.version }, requiresReauthentication: false })}>Rollback…</button></div>)}
        </div>
      </section>
    </div>

    {confirmation === null ? null : <UpdateConfirmation confirmation={confirmation} release={releaseForOperation(confirmation.operation, status.releases)} status={status} password={password} pending={confirming} onCancel={closeConfirmation} onConfirm={() => void performConfirmedOperation()} onPassword={setPassword} onReauthenticate={(event) => void reauthenticateAndRetry(event)} />}
  </div>;
}

function JobProgress({ job, reconnecting }: { readonly job: HostUpdateJob; readonly reconnecting: boolean }) {
  const terminal = !isActiveJob(job);
  return <section className={`update-job update-job--${job.state}`} aria-live="polite" aria-label="Host update progress">
    <span className="update-job-icon">{job.state === "succeeded" ? <Icon name="check" size={18} /> : job.state === "failed" ? "!" : <span className="access-spinner" />}</span>
    <div><span>{operationLabel(job.operation)} · {phaseLabel(job.phase)}</span><strong>{reconnecting ? "Reconnecting to FFDB…" : job.message || jobSummary(job)}</strong><p>{reconnecting ? "The gateway can remain available while the API restarts. This page will resume automatically." : terminal ? `Finished ${formatTimestamp(job.updated_at_ms)}.` : "You can leave this page; the root-owned updater continues independently."}</p></div>
    <span className="update-job-state">{reconnecting ? "Reconnecting" : capitalize(job.state)}</span>
  </section>;
}

function UpdateConfirmation({ confirmation, release, status, password, pending, onCancel, onConfirm, onPassword, onReauthenticate }: {
  readonly confirmation: ConfirmationState;
  readonly release: HostUpdateRelease | null;
  readonly status: HostUpdateStatus;
  readonly password: string;
  readonly pending: boolean;
  onCancel(): void;
  onConfirm(): void;
  onPassword(value: string): void;
  onReauthenticate(event: FormEvent): void;
}) {
  const dialogRef = useRef<HTMLElement>(null);
  const { operation } = confirmation;
  const copy = operation.kind === "install"
    ? { eyebrow: "INSTALL RELEASE", title: `Update to FFDB ${operation.version}?`, detail: "FFDB will take a backup, verify the release, switch versions atomically, restart application services, and run health checks. The portal will reconnect automatically.", action: "Install update" }
    : operation.kind === "rollback"
      ? { eyebrow: "ROLL BACK RELEASE", title: `Roll back to FFDB ${operation.version}?`, detail: "FFDB will create a fresh backup before switching releases. The rollback proceeds only if the host confirms data-format compatibility and post-start health checks pass.", action: "Start rollback" }
      : { eyebrow: "SAVE UPDATE POLICY", title: "Apply this update policy?", detail: operation.settings.automatic_apply ? `Signed releases may be installed automatically from ${operation.settings.maintenance_window_start} UTC for ${operation.settings.maintenance_window_duration_minutes} minutes.` : "Automatic installation will remain off. The host will only check the stable channel on the configured schedule.", action: "Save policy" };
  useEffect(() => {
    const previouslyFocused = globalThis.document.activeElement instanceof HTMLElement ? globalThis.document.activeElement : null;
    const dialog = dialogRef.current;
    const initial = dialog?.querySelector<HTMLElement>(confirmation.requiresReauthentication ? "input" : "button");
    globalThis.setTimeout(() => initial?.focus(), 0);
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !pending) {
        event.preventDefault();
        onCancel();
        return;
      }
      if (event.key !== "Tab" || dialog === null) return;
      const focusable = [...dialog.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), a[href]')];
      if (focusable.length === 0) return;
      const first = focusable[0]!;
      const last = focusable.at(-1)!;
      if (event.shiftKey && globalThis.document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && globalThis.document.activeElement === last) { event.preventDefault(); first.focus(); }
    };
    globalThis.document.addEventListener("keydown", handleKeyDown);
    return () => {
      globalThis.document.removeEventListener("keydown", handleKeyDown);
      globalThis.setTimeout(() => previouslyFocused?.focus(), 0);
    };
  }, [confirmation.requiresReauthentication, onCancel, pending]);

  return <div className="update-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget && !pending) onCancel(); }}>
    <section className="update-dialog" ref={dialogRef} role="dialog" aria-modal="true" aria-labelledby="update-dialog-title">
      <header><div><span>{confirmation.requiresReauthentication ? "CONFIRM ADMINISTRATOR" : copy.eyebrow}</span><h2 id="update-dialog-title">{confirmation.requiresReauthentication ? "Sign in again to continue" : copy.title}</h2></div><button aria-label="Close update confirmation" disabled={pending} type="button" onClick={onCancel}>×</button></header>
      {confirmation.requiresReauthentication ? <form onSubmit={onReauthenticate}><p>Host lifecycle changes require an administrator sign-in from the last 15 minutes. Your password is sent only to FFDB's normal sign-in endpoint.</p><label className="update-dialog-password"><span>Password</span><input autoComplete="current-password" required type="password" value={password} onChange={(event) => onPassword(event.target.value)} /></label><div className="update-dialog-actions"><button className="secondary-action" disabled={pending} type="button" onClick={onCancel}>Cancel</button><button className="primary-action" disabled={pending || password.length === 0} type="submit">{pending ? "Signing in…" : `Sign in and ${copy.action.toLowerCase()}`}</button></div></form> : <><p>{copy.detail}</p>{operation.kind === "settings" ? null : <dl className="update-release-evidence"><div><dt>Signature</dt><dd>{release?.signature_verified ? "Verified" : "Verified before activation"}</dd></div><div><dt>Identity</dt><dd title={release?.signature_identity ?? status.signature_identity ?? undefined}>{release?.signature_identity ?? status.signature_identity ?? "Official FFDB release workflow"}</dd></div><div><dt>State schema</dt><dd>{release?.state_schema ?? status.state_schema}</dd></div><div><dt>Rollback floor</dt><dd>{release?.minimum_rollback_version ?? status.minimum_rollback_version ?? "None declared"}</dd></div></dl>}<ul><li><Icon name="shield" size={14} />Official signature and checksum verification are mandatory.</li><li><Icon name="backup" size={14} />A pre-change backup is mandatory.</li><li><Icon name="check" size={14} />Failed health checks prevent the new release from remaining active.</li></ul>{release?.release_url === null || release?.release_url === undefined ? null : <a className="update-release-link" href={release.release_url} rel="noreferrer" target="_blank"><Icon name="external" size={13} />Read release notes before continuing</a>}<div className="update-dialog-actions"><button className="secondary-action" disabled={pending} type="button" onClick={onCancel}>Cancel</button><button className={operation.kind === "rollback" ? "danger-action" : "primary-action"} disabled={pending} type="button" onClick={onConfirm}>{pending ? "Requesting…" : copy.action}</button></div></>}
    </section>
  </div>;
}

function UpdateUnsupported({ reason }: { readonly reason: string }) {
  return <div className="updates-page"><header className="updates-heading"><div><span><Icon name="sync" size={15} />INSTANCE LIFECYCLE</span><h1>Updates</h1><p>Host lifecycle controls depend on the installation topology.</p></div></header><section className="updates-unsupported"><span><Icon name="terminal" size={24} /></span><div><h2>Portal updates are not available on this installation</h2><p>{reason}</p><div className="updates-unsupported-options"><article><strong>Docker installation</strong><p>Keep the Docker socket outside the API container. Run the signed host controller directly on the server.</p><code>sudo ffdb-host update-check</code><code>sudo ffdb-host update</code></article><article><strong>Earlier native installation</strong><p>Upgrade once with a signed native release bundle. The bundle installs the constrained updater agent and enables future portal controls.</p><a className="secondary-action" href="/docs/host-updates">Open host update guide</a></article></div></div></section></div>;
}

function UpdateError({ message, onDismiss, onRetry }: { readonly message: string; onDismiss?(): void; onRetry?(): void }) {
  return <div className="updates-error" role="alert"><span>!</span><div><strong>Host update request failed</strong><p>{message}</p></div>{onRetry === undefined ? null : <button className="secondary-action" type="button" onClick={onRetry}>Try again</button>}{onDismiss === undefined ? null : <button aria-label="Dismiss host update error" type="button" onClick={onDismiss}>×</button>}</div>;
}

function isActiveJob(job: HostUpdateJob): boolean {
  return job.state === "queued" || job.state === "running";
}

function isReauthenticationRequired(cause: unknown): boolean {
  return cause instanceof FFDBError && cause.status === 428 && cause.code === "platform_auth.reauthentication_required";
}

function updateErrorMessage(cause: unknown): string {
  if (cause instanceof FFDBError) {
    if (cause.status === 404) return "This installation does not include the host updater. Install a release that bundles the updater service, then reload the portal.";
    if (cause.status === 503) return "The root-owned updater service is not available. Application traffic is unaffected; check the updater service before trying again.";
    return cause.message;
  }
  return cause instanceof Error ? cause.message : "The host update request could not be completed.";
}

function settingsEqual(left: HostUpdateSettings, right: HostUpdateSettings): boolean {
  return left.channel === right.channel
    && left.automatic_checks === right.automatic_checks
    && left.check_interval_hours === right.check_interval_hours
    && left.automatic_apply === right.automatic_apply
    && left.maintenance_window_start === right.maintenance_window_start
    && left.maintenance_window_duration_minutes === right.maintenance_window_duration_minutes;
}

function releaseForOperation(operation: PendingOperation, releases: readonly HostUpdateRelease[]): HostUpdateRelease | null {
  return operation.kind === "settings" ? null : releases.find((release) => release.version === operation.version) ?? null;
}

function completionNotice(job: HostUpdateJob, status: HostUpdateStatus): string {
  if (job.operation === "check") return status.update_available && status.available_version !== null ? `FFDB ${status.available_version} is available` : "This host is up to date";
  if (job.operation === "install") return `Updated to FFDB ${status.installed_version ?? job.requested_version ?? "the selected release"}`;
  if (job.operation === "rollback") return `Rolled back to FFDB ${status.installed_version ?? job.requested_version ?? "the selected release"}`;
  return "Update policy saved";
}

function jobSummary(job: HostUpdateJob): string {
  if (job.operation === "check") return "Checking the stable release channel";
  if (job.operation === "configure") return "Applying the host update policy";
  return `${job.operation === "install" ? "Installing" : "Rolling back to"} FFDB ${job.requested_version ?? "release"}`;
}

function operationLabel(operation: HostUpdateOperation): string {
  if (operation === "check") return "Release check";
  if (operation === "install") return "Update";
  if (operation === "rollback") return "Rollback";
  return "Policy change";
}

function phaseLabel(phase: string): string {
  return phase.replaceAll("_", " ").replace(/^./u, (value) => value.toUpperCase());
}

function capitalize(value: string): string {
  return value.replace(/^./u, (character) => character.toUpperCase());
}

function formatTimestamp(value: number): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(value);
}

function operationId(prefix: string): string {
  return `${prefix}-${globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(16).slice(2)}`}`;
}

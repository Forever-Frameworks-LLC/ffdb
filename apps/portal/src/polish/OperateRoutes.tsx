import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";

import {
  FFDBClient,
  type BackupSummary,
  type EmailTemplateArtifactInput,
  type EmailTemplatePreview,
  type EmailTemplateVersion,
  type PolicyCommand,
  type PolicyDefinition,
  type StorageBucket,
  type StorageObjectItem,
} from "@ffdb/client";

import { Icon, type IconName } from "../icons.js";
import { ManagedTable } from "./ManagedTable.js";
import "./operate-routes.css";

interface OperationMessage {
  readonly tone: "success" | "error" | "info";
  readonly title: string;
  readonly detail?: string;
}

interface ResourceState<T> {
  readonly value: T | null;
  readonly error: string | null;
  readonly loading: boolean;
}

function useReloadableResource<T>(read: () => Promise<T>) {
  const [revision, setRevision] = useState(0);
  const [state, setState] = useState<ResourceState<T>>({ value: null, error: null, loading: true });
  const stableRead = useCallback(read, [read]);
  useEffect(() => {
    let active = true;
    setState((current) => ({ value: current.value, error: null, loading: true }));
    void stableRead().then(
      (value) => { if (active) setState({ value, error: null, loading: false }); },
      (cause: unknown) => { if (active) setState((current) => ({ value: current.value, error: errorMessage(cause), loading: false })); },
    );
    return () => { active = false; };
  }, [revision, stableRead]);
  return { ...state, reload: () => setRevision((value) => value + 1) };
}

export interface PoliciesPanelProps {
  readonly client: FFDBClient;
  onEdit(sql: string): void;
}

export function PoliciesPanel({ client, onEdit }: PoliciesPanelProps) {
  const resource = useReloadableResource(useCallback(() => client.policies(), [client]));
  const [command, setCommand] = useState<PolicyCommand>("select");
  const [tab, setTab] = useState<"inventory" | "draft">("inventory");
  const [selected, setSelected] = useState<PolicyDefinition | null>(null);
  const policies = resource.value ?? [];
  const enabled = policies.filter((policy) => policy.enabled).length;
  const forced = policies.filter((policy) => policy.forced).length;
  const protectedTables = new Set(policies.map((policy) => policy.table)).size;
  const createDraft = () => onEdit(policyTemplate(command));

  if (resource.loading && resource.value === null) return <RouteSkeleton title="Loading security policies" />;
  if (resource.error !== null && resource.value === null) {
    return <RouteFailure title="Policies could not be loaded" detail={resource.error} onRetry={resource.reload} />;
  }

  return (
    <div className="operate-root">
      <WorkspaceTabs<"inventory" | "draft">
        ariaLabel="Policy tasks"
        selected={tab}
        tabs={[{ id: "inventory", label: "Inventory" }, { id: "draft", label: "New policy" }]}
        summary={<InlineSummary items={[`${policies.length} total`, `${enabled} enabled`, `${protectedTables} tables`, `${forced} forced`]} />}
        actions={<button className="operate-button" type="button" onClick={resource.reload}><Icon name="sync" size={15} />Refresh</button>}
        onSelect={setTab}
      />
      {resource.error === null ? null : <Message message={{ tone: "error", title: "The latest refresh failed", detail: resource.error }} />}
      {tab === "inventory" ? <TabPanel id="inventory">
        <section className="operate-card operate-card--wide">
          <CardHeading title="Policy inventory" detail="Search by table, role, command, or expression. Open a policy to review its complete decision logic." />
          <div className="operate-card-body">
            <ManagedTable
              empty="No policies exist yet. Draft the first policy before exposing application data."
              headings={["Policy", "Table", "Command", "Roles", "Mode", "Status", "Review"]}
              label="policies"
              rows={policies.map((policy) => [
                <strong key={`${policy.name}-name`}>{policy.name}</strong>,
                <code key={`${policy.name}-table`}>{policy.table}</code>,
                policy.command.toUpperCase(),
                policy.roles.length === 0 ? "No roles" : policy.roles.join(", "),
                policy.kind === "restrictive" ? "Restrictive" : "Permissive",
                <Status key={`${policy.name}-status`} tone={policy.enabled ? "positive" : "muted"}>{policy.enabled ? (policy.forced ? "Enabled · forced" : "Enabled") : "Disabled"}</Status>,
                <button className="operate-link-button" key={`${policy.name}-review`} type="button" onClick={() => setSelected(policy)}>View details</button>,
              ])}
            />
          </div>
        </section>
      </TabPanel> : <TabPanel id="draft">
        <section className="operate-card operate-focused-task">
          <CardHeading title="Start a policy draft" detail="Choose the SQL command this policy controls. The generated draft opens in the SQL editor for table, role, ownership, and rollback review." />
          <div className="operate-compact-form">
            <label className="operate-field"><span>Policy command</span><select aria-label="Policy command template" value={command} onChange={(event) => setCommand(event.target.value as PolicyCommand)}><option value="select">SELECT · read rows</option><option value="insert">INSERT · create rows</option><option value="update">UPDATE · change rows</option><option value="delete">DELETE · remove rows</option><option value="all">ALL · every command</option></select></label>
            <div className="operate-guidance"><Icon name="shield" size={18} /><p>RLS-enabled tables are default-deny until a matching policy allows the application user. Review the generated identifiers and expressions before applying.</p></div>
            <div className="operate-form-actions"><button className="operate-button operate-button--primary" type="button" onClick={createDraft}><Icon name="code" size={15} />Open SQL draft</button></div>
          </div>
        </section>
      </TabPanel>}
      {selected === null ? null : (
        <DetailDialog title={selected.name} subtitle={`${selected.table} · ${selected.command.toUpperCase()}`} onClose={() => setSelected(null)}>
          <DefinitionList items={[
            ["Evaluation", selected.kind],
            ["Roles", selected.roles.length === 0 ? "None" : selected.roles.join(", ")],
            ["Enabled", selected.enabled ? "Yes" : "No"],
            ["Forced", selected.forced ? "Yes" : "No"],
          ]} />
          <Expression label="USING expression" value={selected.using_expression} />
          <Expression label="WITH CHECK expression" value={selected.check_expression} />
          <div className="operate-dialog-actions">
            <button className="operate-button" type="button" onClick={() => setSelected(null)}>Close</button>
            <button className="operate-button operate-button--primary" type="button" onClick={() => { onEdit(policyEditTemplate(selected)); setSelected(null); }}><Icon name="code" size={15} />Open SQL draft</button>
          </div>
        </DetailDialog>
      )}
    </div>
  );
}

export interface StoragePanelProps {
  readonly client: FFDBClient;
  onManageSession?(): void;
}

export function StoragePanel({ client, onManageSession }: StoragePanelProps) {
  const [revision, setRevision] = useState(0);
  const resource = useReloadableResource(useCallback(() => client.storage.buckets(), [client, revision]));
  const [tab, setTab] = useState<"buckets" | "objects" | "create" | "maintenance">("buckets");
  const [selectedBucket, setSelectedBucket] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState<OperationMessage | null>(null);
  const buckets = resource.value ?? [];
  const active = buckets.find((bucket) => bucket.name === selectedBucket) ?? null;

  const cleanup = async () => {
    setPending(true); setMessage(null);
    try {
      const result = await client.storage.cleanupReservations();
      setMessage({ tone: "success", title: "Reservation cleanup completed", detail: `${result.removed} removed · ${result.retried} retried` });
    } catch (cause) { setMessage({ tone: "error", title: "Reservation cleanup failed", detail: errorMessage(cause) }); }
    finally { setPending(false); }
  };

  if (resource.loading && resource.value === null) return <RouteSkeleton title="Loading storage" />;
  if (resource.error !== null && resource.value === null) return <RouteFailure title="Storage could not be loaded" detail={resource.error} onRetry={resource.reload} />;

  return (
    <div className="operate-root">
      <WorkspaceTabs<"buckets" | "objects" | "create" | "maintenance">
        ariaLabel="Storage tasks"
        selected={tab}
        tabs={[{ id: "buckets", label: "Buckets" }, { id: "objects", label: "Objects" }, { id: "create", label: "New bucket" }, { id: "maintenance", label: "Maintenance" }]}
        summary={<InlineSummary items={[`${buckets.length} ${buckets.length === 1 ? "bucket" : "buckets"}`, active === null ? "No bucket selected" : active.name]} />}
        actions={<button className="operate-button" type="button" onClick={resource.reload}><Icon name="sync" size={15} />Refresh</button>}
        onSelect={setTab}
      />
      {message === null ? null : <Message message={message} onDismiss={() => setMessage(null)} />}
      {resource.error === null ? null : <Message message={{ tone: "error", title: "The latest bucket refresh failed", detail: resource.error }} />}
      {tab === "buckets" ? <TabPanel id="buckets"><section className="operate-card operate-card--wide">
        <CardHeading title="Bucket inventory" detail="Access, maximum object size, and versioning mode are fixed at creation in the current API." />
        <div className="operate-card-body">
          <ManagedTable
            empty="No buckets exist. Open New bucket to create a private application bucket."
            headings={["Bucket", "Access", "Versioning", "Max object", "Project quota", "Created", "Objects"]}
            label="buckets"
            rows={buckets.map((bucket) => [
              <strong key={`${bucket.id}-name`}>{bucket.name}</strong>,
              <Status key={`${bucket.id}-access`} tone={bucket.public ? "attention" : "positive"}>{bucket.public ? "Public" : "Private"}</Status>,
              bucket.versioning ? "Enabled" : "Disabled",
              formatBytes(bucket.max_object_bytes),
              formatBytes(bucket.project_quota_bytes),
              formatDate(bucket.created_at_ms),
              <button aria-label={selectedBucket === bucket.name ? `Viewing objects in ${bucket.name}` : `Browse objects in ${bucket.name}`} className="operate-link-button" key={`${bucket.id}-browse`} type="button" onClick={() => { setSelectedBucket(bucket.name); setTab("objects"); }}>{selectedBucket === bucket.name ? "Open" : "Browse"}</button>,
            ])}
          />
        </div>
      </section></TabPanel> : null}
      {tab === "objects" ? <TabPanel id="objects">{active === null ? (
        <section className="operate-card operate-bucket-picker">
          <CardHeading title="Choose a bucket" detail="Object operations use an application-user session and are evaluated through project storage policies." />
          <div className="operate-compact-form"><label className="operate-field"><span>Bucket</span><select aria-label="Bucket to browse" value="" onChange={(event) => setSelectedBucket(event.target.value)}><option value="">Select a bucket</option>{buckets.map((bucket) => <option key={bucket.id} value={bucket.name}>{bucket.name}</option>)}</select></label>{buckets.length === 0 ? <p className="operate-muted-copy">Create a bucket before managing objects.</p> : null}</div>
        </section>
      ) : <BucketObjects key={active.id} client={client} bucket={active} onClose={() => setSelectedBucket(null)} {...(onManageSession === undefined ? {} : { onManageSession })} />}</TabPanel> : null}
      {tab === "create" ? <TabPanel id="create"><CreateBucketForm client={client} onCancel={() => setTab("buckets")} onCreated={(bucket) => { setSelectedBucket(bucket.name); setRevision((value) => value + 1); setMessage({ tone: "success", title: `Bucket ${bucket.name} created`, detail: bucket.public ? "Public bucket" : "Private bucket" }); setTab("objects"); }} /></TabPanel> : null}
      {tab === "maintenance" ? <TabPanel id="maintenance"><section className="operate-card operate-focused-task">
        <CardHeading title="Reservation cleanup" detail="Reconcile interrupted provider reservations after failed uploads or network interruptions. This does not delete committed objects." />
        <div className="operate-maintenance-action"><span className="operate-icon-well"><Icon name="sync" size={18} /></span><div><h3>Release or retry stale reservations</h3><p>The API decides which unfinished reservations are safe to remove or retry.</p></div><button className="operate-button operate-button--primary" disabled={pending} type="button" onClick={() => void cleanup()}>{pending ? "Cleaning…" : "Run cleanup"}</button></div>
      </section></TabPanel> : null}
    </div>
  );
}

function CreateBucketForm({ client, onCancel, onCreated }: { readonly client: FFDBClient; onCancel(): void; onCreated(bucket: StorageBucket): void }) {
  const [name, setName] = useState("");
  const [visibility, setVisibility] = useState<"private" | "public">("private");
  const [versioning, setVersioning] = useState(false);
  const [maxObjectMb, setMaxObjectMb] = useState(25);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const normalized = name.trim().toLowerCase();
  const validName = /^[a-z0-9](?:[a-z0-9.-]{1,61}[a-z0-9])?$/u.test(normalized);
  const create = async (event: FormEvent) => {
    event.preventDefault(); if (!validName || maxObjectMb < 1) return;
    setPending(true); setError(null);
    try { onCreated(await client.storage.createBucket({ name: normalized, public: visibility === "public", versioning, max_object_bytes: maxObjectMb * 1_048_576 })); }
    catch (cause) { setError(errorMessage(cause)); }
    finally { setPending(false); }
  };
  return (
    <section className="operate-card operate-create-card">
      <CardHeading title="Create a bucket" detail="Choose access intentionally. Public buckets bypass application-user read checks for downloads." />
      <form className="operate-form" onSubmit={(event) => void create(event)}>
        <label className="operate-field"><span>Bucket name</span><input autoFocus aria-invalid={name !== "" && !validName} placeholder="user-uploads" value={name} onChange={(event) => setName(event.target.value)} /><small>3–63 lowercase letters, numbers, periods, or hyphens.</small></label>
        <label className="operate-field"><span>Access</span><select value={visibility} onChange={(event) => setVisibility(event.target.value as typeof visibility)}><option value="private">Private · RLS required</option><option value="public">Public read access</option></select></label>
        <label className="operate-field"><span>Maximum object (MB)</span><input min="1" step="1" type="number" value={maxObjectMb} onChange={(event) => setMaxObjectMb(Number(event.target.value))} /></label>
        <label className="operate-switch"><input checked={versioning} type="checkbox" onChange={(event) => setVersioning(event.target.checked)} /><span><strong>Enable object versioning</strong><small>Preserve prior provider versions when supported.</small></span></label>
        {error === null ? null : <Message message={{ tone: "error", title: "Bucket creation failed", detail: error }} />}
        <div className="operate-form-actions"><button className="operate-button" type="button" onClick={onCancel}>Cancel</button><button className="operate-button operate-button--primary" disabled={!validName || maxObjectMb < 1 || pending} type="submit">{pending ? "Creating…" : "Create bucket"}</button></div>
      </form>
    </section>
  );
}

function BucketObjects({ client, bucket, onClose, onManageSession }: { readonly client: FFDBClient; readonly bucket: StorageBucket; onClose(): void; onManageSession?(): void }) {
  const [sessionState, setSessionState] = useState<"loading" | "ready" | "signed-out">("loading");
  const [revision, setRevision] = useState(0);
  const [prefixDraft, setPrefixDraft] = useState("");
  const [prefix, setPrefix] = useState("");
  const [cursorHistory, setCursorHistory] = useState<readonly (string | undefined)[]>([undefined]);
  const [pageIndex, setPageIndex] = useState(0);
  const [items, setItems] = useState<readonly StorageObjectItem[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [file, setFile] = useState<File | null>(null);
  const [key, setKey] = useState("");
  const [pending, setPending] = useState<string | null>(null);
  const [message, setMessage] = useState<OperationMessage | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const cursor = cursorHistory[pageIndex];

  useEffect(() => {
    let active = true;
    void client.currentSession().then(
      (session) => { if (active) setSessionState(session === null ? "signed-out" : "ready"); },
      () => { if (active) setSessionState("signed-out"); },
    );
    return () => { active = false; };
  }, [client]);

  const load = useCallback(async () => {
    setLoading(true); setLoadError(null);
    if (sessionState !== "ready") { setLoading(false); return; }
    try {
      const page = await client.storage.list(bucket.name, { prefix: prefix.trim(), limit: 100, ...(cursor === undefined ? {} : { cursor }) });
      setItems(page.items); setNextCursor(page.next_cursor);
    } catch (cause) {
      if (isUserSessionError(cause)) setSessionState("signed-out");
      else setLoadError(errorMessage(cause));
    }
    finally { setLoading(false); }
  }, [bucket.name, client, cursor, prefix, revision, sessionState]);
  useEffect(() => { if (sessionState !== "loading") void load(); }, [load, sessionState]);

  const upload = async (event: FormEvent) => {
    event.preventDefault(); if (file === null || key.trim() === "") return;
    const objectKey = key.trim(); setPending("upload"); setMessage(null);
    try {
      await client.storage.upload(bucket.name, objectKey, file, { sizeBytes: file.size, contentType: file.type || "application/octet-stream" });
      setFile(null); setKey(""); if (fileInput.current !== null) fileInput.current.value = "";
      setMessage({ tone: "success", title: `${objectKey} uploaded`, detail: formatBytes(file.size) });
      setRevision((value) => value + 1);
    } catch (cause) {
      if (isUserSessionError(cause)) setSessionState("signed-out");
      else setMessage({ tone: "error", title: "Upload failed", detail: errorMessage(cause) });
    }
    finally { setPending(null); }
  };
  const download = async (objectKey: string) => {
    setPending(`download:${objectKey}`); setMessage(null);
    try {
      const signed = await client.storage.downloadUrl(bucket.name, objectKey);
      const opened = globalThis.open(signed.url, "_blank", "noopener,noreferrer");
      setMessage({ tone: "success", title: `Download link created for ${objectKey}`, detail: opened === null ? "Your browser blocked the new tab. Allow popups and try again." : "The signed provider URL opened in a new tab." });
    } catch (cause) {
      if (isUserSessionError(cause)) setSessionState("signed-out");
      else setMessage({ tone: "error", title: "Download failed", detail: errorMessage(cause) });
    }
    finally { setPending(null); }
  };
  const remove = async (objectKey: string) => {
    if (!globalThis.confirm(`Permanently delete ${objectKey} from ${bucket.name}?`)) return;
    setPending(`delete:${objectKey}`); setMessage(null);
    try { await client.storage.delete(bucket.name, objectKey); setMessage({ tone: "success", title: `${objectKey} deleted` }); setRevision((value) => value + 1); }
    catch (cause) {
      if (isUserSessionError(cause)) setSessionState("signed-out");
      else setMessage({ tone: "error", title: "Delete failed", detail: errorMessage(cause) });
    }
    finally { setPending(null); }
  };
  const trackedBytes = items.reduce((total, item) => total + item.size_bytes, 0);

  return (
    <section className="operate-card operate-card--wide">
      <CardHeading title={`Objects · ${bucket.name}`} detail={`${items.length} visible on this page · ${formatBytes(trackedBytes)} tracked`} action={<button className="operate-button" type="button" onClick={onClose}>Close</button>} />
      <div className="operate-card-body operate-stack">
        {sessionState === "loading" ? <InlineLoading label="Checking application-user session" /> : sessionState === "signed-out" ? (
          <div className="operate-session-gate" role="status">
            <span className="operate-icon-well"><Icon name="lock" size={18} /></span>
            <div><h3>Sign in as an application user to manage objects</h3><p>Bucket administration uses the project developer credential. Object listing, upload, download, and deletion are evaluated with an end-user session and the project’s row-level storage policies.</p></div>
            {onManageSession === undefined ? <span className="operate-session-instruction">Open Auth from the project navigation to sign in.</span> : <button className="operate-button operate-button--primary" type="button" onClick={onManageSession}>Open Auth</button>}
          </div>
        ) : <>
        {message === null ? null : <Message message={message} onDismiss={() => setMessage(null)} />}
        <form className="operate-object-controls" onSubmit={(event) => void upload(event)}>
          <label className="operate-field"><span>Object file</span><input ref={fileInput} type="file" onChange={(event) => { const selected = event.target.files?.[0] ?? null; setFile(selected); if (selected !== null) setKey(selected.name); }} /></label>
          <label className="operate-field"><span>Object key</span><input placeholder="avatars/user-123.png" value={key} onChange={(event) => setKey(event.target.value)} /></label>
          <button className="operate-button operate-button--primary" disabled={file === null || key.trim() === "" || pending !== null} type="submit">{pending === "upload" ? "Uploading…" : "Upload object"}</button>
        </form>
        <form className="operate-filter-row" onSubmit={(event) => { event.preventDefault(); setPrefix(prefixDraft.trim()); setCursorHistory([undefined]); setPageIndex(0); setRevision((value) => value + 1); }}>
          <label className="operate-field"><span>Key prefix</span><input placeholder="avatars/" value={prefixDraft} onChange={(event) => setPrefixDraft(event.target.value)} /></label>
          <button className="operate-button" type="submit"><Icon name="search" size={15} />Apply prefix</button>
          <button className="operate-button" type="button" onClick={() => { setPrefixDraft(""); setPrefix(""); setCursorHistory([undefined]); setPageIndex(0); setRevision((value) => value + 1); }}>Clear</button>
        </form>
        {loading ? <InlineLoading label="Loading objects" /> : loadError === null ? (
          <ManagedTable
            empty={prefix === "" ? "No objects are visible in this bucket." : "No objects match this prefix."}
            headings={["Object key", "Size", "Type", "Owner", "Version", "Updated", "Actions"]}
            label="objects"
            rows={items.map((item) => [
              <code key={`${item.id}-key`}>{item.object_key}</code>, formatBytes(item.size_bytes), item.content_type ?? "Unknown", shortId(item.owner_id), item.version_id ?? "—", formatDate(item.updated_at_ms),
              <span className="operate-row-actions" key={`${item.id}-actions`}><button disabled={pending !== null} type="button" onClick={() => void download(item.object_key)}>{pending === `download:${item.object_key}` ? "Opening…" : "Download"}</button><button className="is-danger" disabled={pending !== null} type="button" onClick={() => void remove(item.object_key)}>{pending === `delete:${item.object_key}` ? "Deleting…" : "Delete…"}</button></span>,
            ])}
          />
        ) : <RouteFailure compact title="Objects could not be loaded" detail={loadError} onRetry={() => void load()} />}
        {pageIndex === 0 && nextCursor === null ? null : <div className="operate-server-page"><span>Server page {pageIndex + 1}{nextCursor === null ? " · end of results" : " · more objects available"}</span><div><button className="operate-button" disabled={pageIndex === 0} type="button" onClick={() => setPageIndex((value) => Math.max(0, value - 1))}>Previous 100</button><button className="operate-button" disabled={nextCursor === null} type="button" onClick={() => { if (nextCursor !== null) { setCursorHistory((history) => [...history.slice(0, pageIndex + 1), nextCursor]); setPageIndex((value) => value + 1); } }}>Next 100</button></div></div>}
        </>}
      </div>
    </section>
  );
}

export interface EmailPanelProps { readonly client: FFDBClient }

export function EmailPanel({ client }: EmailPanelProps) {
  const [revision, setRevision] = useState(0);
  const resource = useReloadableResource(useCallback(() => client.emailTemplates(), [client, revision]));
  const [tab, setTab] = useState<"versions" | "import" | "preview" | "delivery">("versions");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [artifactText, setArtifactText] = useState("");
  const [variablesText, setVariablesText] = useState("{}");
  const [preview, setPreview] = useState<EmailTemplatePreview | null>(null);
  const [pending, setPending] = useState<"import" | "preview" | "publish" | null>(null);
  const [message, setMessage] = useState<OperationMessage | null>(null);
  const templates = resource.value ?? [];
  const active = templates.find((template) => templateId(template) === selectedId) ?? templates[0] ?? null;

  const importArtifact = async (event: FormEvent) => {
    event.preventDefault(); setMessage(null); setPending("import");
    try {
      const artifact = parseArtifact(artifactText);
      const created = await client.importEmailTemplateArtifact(artifact);
      setArtifactText(""); setSelectedId(templateId(created)); setRevision((value) => value + 1); setTab("preview");
      setMessage({ tone: "success", title: `${humanize(created.kind)} version ${created.version} imported`, detail: created.artifact_status === "validated" ? "The artifact passed server validation." : `${created.compilation_errors.length} validation errors were returned.` });
    } catch (cause) { setMessage({ tone: "error", title: "Artifact import failed", detail: errorMessage(cause) }); }
    finally { setPending(null); }
  };
  const renderPreview = async () => {
    if (active === null) return; setMessage(null); setPending("preview");
    try {
      const variables = parseVariables(variablesText);
      setPreview(await client.previewEmailTemplate(active.kind, active.version, variables));
      setMessage({ tone: "success", title: "Preview rendered", detail: `${humanize(active.kind)} version ${active.version}` });
    } catch (cause) { setMessage({ tone: "error", title: "Preview failed", detail: errorMessage(cause) }); }
    finally { setPending(null); }
  };
  const publish = async () => {
    if (active === null || !globalThis.confirm(`Publish ${humanize(active.kind)} version ${active.version} for new deliveries?`)) return;
    setMessage(null); setPending("publish");
    try {
      await client.publishEmailTemplate(active.kind, active.version); setRevision((value) => value + 1);
      setMessage({ tone: "success", title: `${humanize(active.kind)} version ${active.version} published`, detail: "New transactional messages will use this version." });
    } catch (cause) { setMessage({ tone: "error", title: "Publish failed", detail: errorMessage(cause) }); }
    finally { setPending(null); }
  };

  if (resource.loading && resource.value === null) return <RouteSkeleton title="Loading email templates" />;
  if (resource.error !== null && resource.value === null) return <RouteFailure title="Email templates could not be loaded" detail={resource.error} onRetry={resource.reload} />;

  return (
    <div className="operate-root">
      <WorkspaceTabs<"versions" | "import" | "preview" | "delivery">
        ariaLabel="Email template tasks"
        selected={tab}
        tabs={[{ id: "versions", label: "Versions" }, { id: "import", label: "Import" }, { id: "preview", label: "Preview & publish" }, { id: "delivery", label: "Delivery" }]}
        summary={<InlineSummary items={[`${templates.length} ${templates.length === 1 ? "version" : "versions"}`, `${templates.filter((template) => template.published_at_ms !== null).length} published`]} />}
        actions={<button className="operate-button" type="button" onClick={resource.reload}><Icon name="sync" size={15} />Refresh</button>}
        onSelect={(value) => { setTab(value); if (value !== "preview") setPreview(null); }}
      />
      {message === null ? null : <Message message={message} onDismiss={() => setMessage(null)} />}
      {resource.error === null ? null : <Message message={{ tone: "error", title: "The latest template refresh failed", detail: resource.error }} />}
      {tab === "versions" ? <TabPanel id="versions"><section className="operate-card operate-card--wide">
        <CardHeading title="Template history" detail="Search across kind, version, publication state, variables, and validation status." />
        <div className="operate-card-body">
          <ManagedTable
            empty="No custom template versions exist. Import a precompiled artifact to create the first version."
            headings={["Template", "Version", "Validation", "Published", "Variables", "Compiled", "Review"]}
            label="template versions"
            rows={templates.map((template) => [
              <strong key={`${templateId(template)}-kind`}>{humanize(template.kind)}</strong>, String(template.version),
              <Status key={`${templateId(template)}-status`} tone={template.artifact_status === "validated" ? "positive" : "negative"}>{template.artifact_status === "validated" ? "Validated" : `${template.compilation_errors.length} errors`}</Status>,
              template.published_at_ms === null ? "Draft" : formatDate(template.published_at_ms),
              template.allowed_variables.length === 0 ? "None" : template.allowed_variables.join(", "), formatDate(template.compiled_at_ms),
              <button aria-label={`Open ${humanize(template.kind)} version ${template.version}`} className="operate-link-button" key={`${templateId(template)}-review`} type="button" onClick={() => { setSelectedId(templateId(template)); setPreview(null); setTab("preview"); }}>Open</button>,
            ])}
          />
        </div>
      </section></TabPanel> : null}
      {tab === "import" ? <TabPanel id="import"><section className="operate-card operate-focused-task">
          <CardHeading title="Import an artifact" detail="Paste or load the JSON produced by your build pipeline. FFDB validates the compiled subject, HTML, text, variables, and source digest." />
          <form className="operate-form operate-form--single" onSubmit={(event) => void importArtifact(event)}>
            <label className="operate-file-control"><Icon name="archive" size={17} /><span><strong>Load artifact JSON</strong><small>The file stays in this browser until you submit.</small></span><input accept="application/json,.json" aria-label="Load artifact JSON file" type="file" onChange={(event) => { const file = event.target.files?.[0]; if (file !== undefined) void file.text().then(setArtifactText, (cause: unknown) => setMessage({ tone: "error", title: "File could not be read", detail: errorMessage(cause) })); }} /></label>
            <label className="operate-field"><span>Artifact JSON</span><textarea className="operate-code-input" rows={12} spellCheck={false} value={artifactText} onChange={(event) => setArtifactText(event.target.value)} placeholder={'{\n  "kind": "verification",\n  "version": 1,\n  …\n}'} /></label>
            <div className="operate-form-actions"><button className="operate-button" disabled={artifactText === ""} type="button" onClick={() => setArtifactText("")}>Clear</button><button className="operate-button operate-button--primary" disabled={artifactText.trim() === "" || pending !== null} type="submit">{pending === "import" ? "Validating…" : "Validate and import"}</button></div>
          </form>
      </section></TabPanel> : null}
      {tab === "preview" ? <TabPanel id="preview"><div className="operate-preview-workspace"><section className="operate-card">
          <CardHeading title="Preview and publish" detail={active === null ? "Select or import a validated version first." : `${humanize(active.kind)} · version ${active.version} · ${active.published_at_ms === null ? "draft" : "published"}`} />
          <div className="operate-card-body operate-stack">
            {active === null ? <Empty icon="mail" title="No template selected" detail="Import a compiled artifact or choose a version from Versions." /> : <>
              <label className="operate-field"><span>Template version</span><select aria-label="Template version to preview" value={templateId(active)} onChange={(event) => { setSelectedId(event.target.value); setPreview(null); }}>{templates.map((template) => <option key={templateId(template)} value={templateId(template)}>{humanize(template.kind)} · version {template.version} · {template.published_at_ms === null ? "draft" : "published"}</option>)}</select></label>
              {active.compilation_errors.length === 0 ? null : <Message message={{ tone: "error", title: "This artifact cannot be published", detail: active.compilation_errors.join(" · ") }} />}
              <DefinitionList items={[["Allowed variables", active.allowed_variables.length === 0 ? "None" : active.allowed_variables.join(", ")], ["Source SHA-256", shortHash(active.source_sha256)], ["Published", active.published_at_ms === null ? "No" : formatDate(active.published_at_ms)]]} />
              <label className="operate-field"><span>Preview variables JSON</span><textarea className="operate-code-input" rows={7} spellCheck={false} value={variablesText} onChange={(event) => setVariablesText(event.target.value)} /></label>
              <div className="operate-form-actions"><button className="operate-button" disabled={pending !== null} type="button" onClick={() => void renderPreview()}>{pending === "preview" ? "Rendering…" : "Render preview"}</button><button className="operate-button operate-button--primary" disabled={active.artifact_status !== "validated" || pending !== null} type="button" onClick={() => void publish()}>{pending === "publish" ? "Publishing…" : "Publish version…"}</button></div>
            </>}
          </div>
        </section>{preview === null ? null : <EmailPreview preview={preview} onClose={() => setPreview(null)} />}</div></TabPanel> : null}
      {tab === "delivery" ? <TabPanel id="delivery"><section className="operate-card operate-provider-note">
        <span className="operate-icon-well"><Icon name="settings" size={18} /></span><div><h3>Delivery transport is deployment-managed</h3><p>Provider credentials are intentionally not readable from the project portal. Operators configure the production Resend transport or the local Mailpit SMTP service at deployment time; template content and publication remain project-scoped here.</p></div>
      </section></TabPanel> : null}
    </div>
  );
}

function EmailPreview({ preview, onClose }: { readonly preview: EmailTemplatePreview; onClose(): void }) {
  const [mode, setMode] = useState<"html" | "text">("html");
  return (
    <section className="operate-card operate-card--wide">
      <CardHeading title="Rendered preview" detail={preview.subject} action={<button className="operate-button" type="button" onClick={onClose}>Close preview</button>} />
      <div className="operate-preview-toolbar"><div role="group" aria-label="Preview format"><button aria-pressed={mode === "html"} type="button" onClick={() => setMode("html")}>HTML</button><button aria-pressed={mode === "text"} type="button" onClick={() => setMode("text")}>Plain text</button></div><span>Sandboxed preview · scripts disabled</span></div>
      {mode === "html" ? <iframe className="operate-email-frame" sandbox="" srcDoc={preview.html} title="Rendered email HTML" /> : <pre className="operate-email-text">{preview.text}</pre>}
    </section>
  );
}

export interface BackupsPanelProps {
  readonly client: FFDBClient;
  onNotice?(value: string): void;
}

export function BackupsPanel({ client, onNotice }: BackupsPanelProps) {
  const [revision, setRevision] = useState(0);
  const resource = useReloadableResource(useCallback(() => client.backups(), [client, revision]));
  const [tab, setTab] = useState<"history" | "integrity">("history");
  const [pending, setPending] = useState<"create" | "integrity" | "restore" | null>(null);
  const [message, setMessage] = useState<OperationMessage | null>(null);
  const [integrity, setIntegrity] = useState<{ readonly ok: boolean; readonly messages: readonly string[]; readonly checkedAt: number } | null>(null);
  const [restoreTarget, setRestoreTarget] = useState<BackupSummary | null>(null);
  const [confirmation, setConfirmation] = useState("");
  const backups = resource.value ?? [];

  const create = async () => {
    setPending("create"); setMessage(null);
    try {
      const result = await client.createBackup();
      const next = { tone: "success" as const, title: "Backup requested", detail: `${shortId(result.backup_id)} · ${formatBytes(result.size_bytes)} · SHA-256 ${shortHash(result.sha256)}` };
      setMessage(next); onNotice?.(next.title); setRevision((value) => value + 1);
    } catch (cause) { const detail = errorMessage(cause); setMessage({ tone: "error", title: "Backup request failed", detail }); onNotice?.(`Backup failed: ${detail}`); }
    finally { setPending(null); }
  };
  const checkIntegrity = async () => {
    setPending("integrity"); setMessage(null);
    try {
      const result = await client.integrityCheck();
      setIntegrity({ ...result, checkedAt: Date.now() });
      setMessage({ tone: result.ok ? "success" : "error", title: result.ok ? "Integrity check passed" : "Integrity check found problems", detail: result.messages.length === 0 ? "SQLite reported no integrity messages." : result.messages.join(" · ") });
    } catch (cause) { setMessage({ tone: "error", title: "Integrity check failed", detail: errorMessage(cause) }); }
    finally { setPending(null); }
  };
  const restore = async () => {
    if (restoreTarget === null || confirmation !== restoreTarget.id) return;
    setPending("restore"); setMessage(null);
    try {
      const result = await client.restoreBackup(restoreTarget.id);
      setMessage({ tone: result.integrity_ok ? "success" : "error", title: result.integrity_ok ? "Restore verified" : "Restore completed with an integrity failure", detail: `Backup ${shortId(result.backup_id)} · schema version ${result.schema_version}` });
      onNotice?.(`Restore completed for ${restoreTarget.id}`); setRestoreTarget(null); setConfirmation(""); setRevision((value) => value + 1);
    } catch (cause) { const detail = errorMessage(cause); setMessage({ tone: "error", title: "Restore failed", detail }); onNotice?.(`Restore failed: ${detail}`); }
    finally { setPending(null); }
  };

  if (resource.loading && resource.value === null) return <RouteSkeleton title="Loading backups" />;
  if (resource.error !== null && resource.value === null) return <RouteFailure title="Backups could not be loaded" detail={resource.error} onRetry={resource.reload} />;

  const completed = backups.filter((backup) => backup.status === "complete" || backup.status === "restore_verified").length;
  const verified = backups.filter((backup) => backup.last_restore_test_ms !== null || backup.status === "restore_verified").length;
  const latest = backups.reduce<BackupSummary | null>((current, backup) => current === null || backup.created_at_ms > current.created_at_ms ? backup : current, null);
  return (
    <div className="operate-root">
      <WorkspaceTabs<"history" | "integrity">
        ariaLabel="Backup tasks"
        selected={tab}
        tabs={[{ id: "history", label: "Recovery points" }, { id: "integrity", label: "Integrity" }]}
        summary={<InlineSummary items={[`${backups.length} retained`, `${completed} complete`, `${verified} restore-tested`, latest === null ? "No backup yet" : `Latest ${relativeDate(latest.created_at_ms)}`]} />}
        actions={<><button className="operate-button" type="button" onClick={resource.reload}><Icon name="sync" size={15} />Refresh</button><button className="operate-button operate-button--primary" disabled={pending !== null} type="button" onClick={() => void create()}><Icon name="backup" size={15} />{pending === "create" ? "Requesting…" : "Request backup"}</button></>}
        onSelect={setTab}
      />
      {message === null ? null : <Message message={message} onDismiss={() => setMessage(null)} />}
      {resource.error === null ? null : <Message message={{ tone: "error", title: "The latest backup refresh failed", detail: resource.error }} />}
      {tab === "history" ? <TabPanel id="history"><section className="operate-card operate-card--wide">
        <CardHeading title="Backup history" detail="Restore is available only after a backup is complete. Type the complete backup ID before replacing current project data." />
        <div className="operate-card-body">
          <ManagedTable
            empty="No backups exist. Request the first online backup to establish a recovery point."
            headings={["Created", "Status", "Size", "Checksum", "Completed", "Restore tested", "Recovery"]}
            label="backups"
            rows={backups.map((backup) => {
              const canRestore = backup.status === "complete" || backup.status === "restore_verified";
              return [formatDate(backup.created_at_ms), <Status key={`${backup.id}-status`} tone={backupTone(backup.status)}>{humanize(backup.status)}</Status>, backup.size_bytes === null ? "Pending" : formatBytes(backup.size_bytes), backup.sha256 === null ? "Pending" : <code key={`${backup.id}-sha`}>{shortHash(backup.sha256)}</code>, backup.completed_at_ms === null ? "—" : formatDate(backup.completed_at_ms), backup.last_restore_test_ms === null ? "Not yet" : formatDate(backup.last_restore_test_ms), <button className="operate-link-button is-danger" disabled={!canRestore || pending !== null} key={`${backup.id}-restore`} title={canRestore ? "Restore this recovery point" : "Backup must complete before restore"} type="button" onClick={() => { setRestoreTarget(backup); setConfirmation(""); }}>Restore…</button>];
            })}
          />
        </div>
      </section></TabPanel> : <TabPanel id="integrity"><section className="operate-card operate-focused-task">
        <CardHeading title="Live database integrity" detail="Run SQLite’s server-side integrity check against the active project database without creating or restoring a backup." />
        <div className="operate-integrity-workspace">
          <div className="operate-maintenance-action"><span className={`operate-icon-well ${integrity === null ? "" : integrity.ok ? "is-positive" : "is-negative"}`}><Icon name={integrity?.ok === true ? "check" : "shield"} size={18} /></span><div><h3>{integrity === null ? "No integrity result in this session" : integrity.ok ? "Live database integrity passed" : "Live database needs attention"}</h3><p>{integrity === null ? "Run the check to record the current result." : integrity.messages.length === 0 ? "SQLite returned no additional messages." : integrity.messages.join(" · ")}</p>{integrity === null ? null : <small>Checked {formatDate(integrity.checkedAt)}</small>}</div><button className="operate-button operate-button--primary" disabled={pending !== null} type="button" onClick={() => void checkIntegrity()}>{pending === "integrity" ? "Checking…" : integrity === null ? "Run integrity check" : "Run again"}</button></div>
        </div>
      </section></TabPanel>}
      {restoreTarget === null ? null : (
        <DetailDialog danger title="Restore project database?" subtitle={`Backup ${restoreTarget.id}`} onClose={() => { if (pending !== "restore") { setRestoreTarget(null); setConfirmation(""); } }}>
          <div className="operate-destructive-copy"><Icon name="shield" size={20} /><p>This replaces the project’s current SQLite database. Active writes can be lost, clients may need a fresh sync snapshot, and this action cannot be undone from the portal.</p></div>
          <DefinitionList items={[["Created", formatDate(restoreTarget.created_at_ms)], ["Size", restoreTarget.size_bytes === null ? "Unknown" : formatBytes(restoreTarget.size_bytes)], ["Checksum", restoreTarget.sha256 === null ? "Unknown" : shortHash(restoreTarget.sha256)], ["Last restore test", restoreTarget.last_restore_test_ms === null ? "Never" : formatDate(restoreTarget.last_restore_test_ms)]]} />
          <label className="operate-field" htmlFor="operate-restore-confirm"><span>Type the backup ID to confirm</span><code>{restoreTarget.id}</code></label><input aria-label="Type the backup ID to confirm" className="operate-confirm-input" id="operate-restore-confirm" autoFocus autoComplete="off" value={confirmation} onChange={(event) => setConfirmation(event.target.value)} />
          <div className="operate-dialog-actions"><button className="operate-button" disabled={pending === "restore"} type="button" onClick={() => { setRestoreTarget(null); setConfirmation(""); }}>Cancel</button><button className="operate-button operate-button--danger" disabled={confirmation !== restoreTarget.id || pending === "restore"} type="button" onClick={() => void restore()}>{pending === "restore" ? "Restoring…" : "Restore and replace data"}</button></div>
        </DetailDialog>
      )}
    </div>
  );
}

function WorkspaceTabs<T extends string>({ ariaLabel, tabs, selected, summary, actions, onSelect }: {
  readonly ariaLabel: string;
  readonly tabs: readonly { readonly id: T; readonly label: string }[];
  readonly selected: T;
  readonly summary?: ReactNode;
  readonly actions?: ReactNode;
  onSelect(value: T): void;
}) {
  const moveFocus = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let nextIndex = index;
    if (event.key === "ArrowRight") nextIndex = (index + 1) % tabs.length;
    else if (event.key === "ArrowLeft") nextIndex = (index - 1 + tabs.length) % tabs.length;
    else if (event.key === "Home") nextIndex = 0;
    else if (event.key === "End") nextIndex = tabs.length - 1;
    else return;

    const nextTab = tabs[nextIndex];
    if (nextTab === undefined) return;
    event.preventDefault();
    onSelect(nextTab.id);
    const nextButton = event.currentTarget.parentElement?.children.item(nextIndex);
    if (nextButton instanceof HTMLButtonElement) nextButton.focus();
  };

  return <div className="operate-workspace-bar"><div aria-label={ariaLabel} className="operate-tabs" role="tablist">{tabs.map((tab, index) => <button aria-controls={`operate-tabpanel-${tab.id}`} aria-selected={selected === tab.id} id={`operate-tab-${tab.id}`} key={tab.id} role="tab" tabIndex={selected === tab.id ? 0 : -1} type="button" onClick={() => onSelect(tab.id)} onKeyDown={(event) => moveFocus(event, index)}>{tab.label}</button>)}</div>{summary === undefined ? null : summary}{actions === undefined ? null : <div className="operate-toolbar-actions">{actions}</div>}</div>;
}

function TabPanel({ id, children }: { readonly id: string; readonly children: ReactNode }) {
  return <div aria-labelledby={`operate-tab-${id}`} id={`operate-tabpanel-${id}`} role="tabpanel" tabIndex={0}>{children}</div>;
}

function InlineSummary({ items }: { readonly items: readonly string[] }) {
  return <div className="operate-inline-summary" aria-label="Current summary">{items.map((item) => <span key={item}>{item}</span>)}</div>;
}

function CardHeading({ title, detail, action }: { readonly title: string; readonly detail: string; readonly action?: ReactNode }) {
  return <header className="operate-card-heading"><div><h3>{title}</h3><p>{detail}</p></div>{action === undefined ? null : action}</header>;
}

function Status({ tone, children }: { readonly tone: "positive" | "attention" | "negative" | "muted"; readonly children: ReactNode }) {
  return <span className={`operate-status is-${tone}`}><i />{children}</span>;
}

function Message({ message, onDismiss }: { readonly message: OperationMessage; onDismiss?(): void }) {
  return <div className={`operate-message is-${message.tone}`} role={message.tone === "error" ? "alert" : "status"}><Icon name={message.tone === "success" ? "check" : message.tone === "error" ? "shield" : "bell"} size={17} /><div><strong>{message.title}</strong>{message.detail === undefined ? null : <span>{message.detail}</span>}</div>{onDismiss === undefined ? null : <button aria-label="Dismiss message" type="button" onClick={onDismiss}>×</button>}</div>;
}

function DetailDialog({ title, subtitle, children, danger = false, onClose }: { readonly title: string; readonly subtitle: string; readonly children: ReactNode; readonly danger?: boolean; onClose(): void }) {
  return <div className="operate-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.currentTarget === event.target) onClose(); }}><section aria-modal="true" className={`operate-dialog${danger ? " is-danger" : ""}`} role="dialog" aria-labelledby="operate-dialog-title"><header><div><h3 id="operate-dialog-title">{title}</h3><p>{subtitle}</p></div><button aria-label="Close dialog" type="button" onClick={onClose}>×</button></header><div className="operate-dialog-body">{children}</div></section></div>;
}

function DefinitionList({ items }: { readonly items: readonly (readonly [string, string])[] }) {
  return <dl className="operate-definitions">{items.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>;
}

function Expression({ label, value }: { readonly label: string; readonly value: string | null }) {
  return <div className="operate-expression"><span>{label}</span><code>{value ?? "Not defined"}</code></div>;
}

function Empty({ icon, title, detail }: { readonly icon: IconName; readonly title: string; readonly detail: string }) {
  return <div className="operate-empty"><Icon name={icon} size={24} /><div><h4>{title}</h4><p>{detail}</p></div></div>;
}

function InlineLoading({ label }: { readonly label: string }) { return <div className="operate-inline-loading" role="status"><i /><span>{label}…</span></div>; }

function RouteSkeleton({ title }: { readonly title: string }) {
  return <div className="operate-root" aria-busy="true"><div className="operate-skeleton-heading"><span /><strong>{title}</strong><i /></div><div className="operate-skeleton-grid"><i /><i /><i /></div><div className="operate-skeleton-table"><i /><i /><i /><i /></div></div>;
}

function RouteFailure({ title, detail, compact = false, onRetry }: { readonly title: string; readonly detail: string; readonly compact?: boolean; onRetry(): void }) {
  return <section className={`operate-failure${compact ? " is-compact" : ""}`} role="alert"><span className="operate-icon-well is-negative"><Icon name="shield" size={19} /></span><div><h3>{title}</h3><p>{detail}</p></div><button className="operate-button" type="button" onClick={onRetry}><Icon name="sync" size={15} />Try again</button></section>;
}

function policyTemplate(command: PolicyCommand): string {
  const using = command === "insert" ? "" : "\nUSING (owner_id = auth.uid())";
  const check = command === "select" || command === "delete" ? "" : "\nWITH CHECK (owner_id = auth.uid())";
  return `-- Review the table, role, and ownership column before applying.\nALTER TABLE table_name ENABLE ROW LEVEL SECURITY;\n\nCREATE POLICY policy_name\nON table_name\nAS PERMISSIVE\nFOR ${command.toUpperCase()}\nTO authenticated${using}${check};`;
}

function policyEditTemplate(policy: PolicyDefinition): string {
  return `-- Policies are changed through a migration so review and rollback stay explicit.\nDROP POLICY IF EXISTS "${sqlIdentifier(policy.name)}" ON "${sqlIdentifier(policy.table)}";\n\n${policyTemplate(policy.command).replaceAll("table_name", `"${sqlIdentifier(policy.table)}"`).replaceAll("policy_name", `"${sqlIdentifier(policy.name)}"`)}`;
}

function sqlIdentifier(value: string): string { return value.replaceAll('"', '""'); }

function parseArtifact(value: string): EmailTemplateArtifactInput {
  const parsed = JSON.parse(value) as unknown;
  if (!isRecord(parsed)) throw new Error("Artifact JSON must be an object.");
  const stringFields = ["kind", "source", "source_sha256", "subject_template", "html_template", "text_template"] as const;
  for (const field of stringFields) if (typeof parsed[field] !== "string" || parsed[field].trim() === "") throw new Error(`${field} must be a non-empty string.`);
  if (!Number.isInteger(parsed.version) || Number(parsed.version) < 1) throw new Error("version must be a positive integer.");
  if (!Array.isArray(parsed.allowed_variables) || !parsed.allowed_variables.every((item) => typeof item === "string")) throw new Error("allowed_variables must be an array of strings.");
  const kinds = ["verification", "password_reset", "email_change", "invitation", "magic_link"];
  if (!kinds.includes(String(parsed.kind))) throw new Error(`kind must be one of ${kinds.join(", ")}.`);
  return parsed as unknown as EmailTemplateArtifactInput;
}

function parseVariables(value: string): Readonly<Record<string, string | number | boolean>> {
  const parsed = JSON.parse(value) as unknown;
  if (!isRecord(parsed) || Object.values(parsed).some((item) => !["string", "number", "boolean"].includes(typeof item))) throw new Error("Preview variables must be a JSON object with string, number, or boolean values.");
  return parsed as Readonly<Record<string, string | number | boolean>>;
}

function isRecord(value: unknown): value is Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value); }
function templateId(template: EmailTemplateVersion): string { return `${template.kind}:${template.version}`; }
function humanize(value: string): string { return value.replaceAll("_", " ").replace(/\b\w/gu, (letter) => letter.toUpperCase()); }
function shortId(value: string): string { return value.length <= 12 ? value : `${value.slice(0, 8)}…${value.slice(-4)}`; }
function shortHash(value: string): string { return value.length <= 16 ? value : `${value.slice(0, 12)}…${value.slice(-6)}`; }
function formatDate(value: number): string { return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(value)); }
function relativeDate(value: number): string { const delta = Date.now() - value; if (delta < 60_000) return "Just now"; if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`; if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h ago`; return `${Math.floor(delta / 86_400_000)}d ago`; }
function formatBytes(value: number): string { if (!Number.isFinite(value) || value <= 0) return "0 B"; const units = ["B", "KB", "MB", "GB", "TB"]; const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1); const amount = value / (1024 ** index); return `${amount >= 10 || index === 0 ? amount.toFixed(0) : amount.toFixed(1)} ${units[index]}`; }
function backupTone(status: BackupSummary["status"]): "positive" | "attention" | "negative" | "muted" { if (status === "complete" || status === "restore_verified") return "positive"; if (status === "failed") return "negative"; if (status === "queued" || status === "running" || status === "restoring") return "attention"; return "muted"; }
function isUserSessionError(cause: unknown): boolean { const message = errorMessage(cause); return /auth\.session_missing|user session required|session (?:has )?expired|invalid refresh|\b401\b/iu.test(message); }
function errorMessage(cause: unknown): string { return cause instanceof Error ? cause.message : String(cause); }

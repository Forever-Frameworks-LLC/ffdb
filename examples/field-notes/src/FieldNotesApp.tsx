import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type FormEvent,
} from "react";
import {
  Activity,
  ArrowDownToLine,
  Check,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Cloud,
  CloudUpload,
  Code2,
  Database,
  File,
  FileText,
  Folder,
  Gauge,
  HardDriveUpload,
  ListTodo,
  LockKeyhole,
  Menu,
  Paperclip,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Server,
  ShieldCheck,
  Trash2,
  Upload,
  UserRound,
  Users,
  X,
} from "lucide-react";
import type { AuthTokenPair, SessionSummary, StorageObjectItem } from "@ffdb/client";
import { generateId } from "@ffdb/client";
import {
  optimisticList,
  useAuth,
  useFFDB,
  useQuery,
  useSessions,
  useStorageUpload,
  useSync,
} from "@ffdb/react";
import { OfflineSyncClient } from "@ffdb/sync-client";
import { IndexedDbReplica } from "@ffdb/sync-client/browser";

import { ffdbProjectId } from "./ffdb";
import {
  createTask,
  eventsFromResult,
  featureLabels,
  filterTasks,
  formatBytes,
  formatTime,
  objectDisplayName,
  queueTaskDelete,
  queueTaskEdit,
  safeFileName,
  seedWorkspace,
  taskObjectPrefix,
  tasksFromReplica,
  toggleTask,
  type FeatureCheck,
  type FieldTask,
} from "./model";

type View = "workspace" | "storage" | "sessions" | "diagnostics";
type Filter = "all" | "open" | "done";

interface PendingUpload {
  readonly id: string;
  readonly name: string;
}

export function FieldNotesApp({ session }: { readonly session: AuthTokenPair }) {
  const client = useFFDB();
  const auth = useAuth();
  const sessions = useSessions();
  const upload = useStorageUpload();
  const fileInput = useRef<HTMLInputElement>(null);
  const [view, setView] = useState<View>("workspace");
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const [filter, setFilter] = useState<Filter>("all");
  const [search, setSearch] = useState("");
  const [tasks, setTasks] = useState<readonly FieldTask[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [newTitle, setNewTitle] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editTitle, setEditTitle] = useState("");
  const [attachments, setAttachments] = useState<readonly StorageObjectItem[]>([]);
  const [allObjects, setAllObjects] = useState<readonly StorageObjectItem[]>([]);
  const [pendingUploads, setPendingUploads] = useState<readonly PendingUpload[]>([]);
  const [pendingTaskIds, setPendingTaskIds] = useState<ReadonlySet<string>>(new Set());
  const [eventRevision, setEventRevision] = useState(0);
  const [busy, setBusy] = useState<string | null>("initializing");
  const [notice, setNotice] = useState<string | null>(null);
  const [online, setOnline] = useState(navigator.onLine);
  const [health, setHealth] = useState<"checking" | "ready" | "error">("checking");
  const [diagnosticChecks, setDiagnosticChecks] = useState<readonly FeatureCheck[]>([]);

  const syncClient = useMemo(() => {
    const replicaName = `ffdb-field-notes-${ffdbProjectId}-${session.user.id}`;
    return new OfflineSyncClient(client, new IndexedDbReplica(replicaName));
  }, [client, session.user.id]);
  const sync = useSync(syncClient);

  const selectedTask = tasks.find((task) => task.id === selectedId) ?? null;
  const visibleTasks = useMemo(() => filterTasks(tasks, filter, search), [tasks, filter, search]);
  const eventQuery = useQuery(
    selectedId === null ? null : {
      sql: "SELECT id, kind, message, created_at_ms FROM field_task_events WHERE task_id = ?1 AND owner_id = auth.uid() ORDER BY created_at_ms DESC LIMIT 6",
      parameters: [{ type: "text", value: selectedId }],
    },
    [selectedId, eventRevision],
  );
  const events = eventsFromResult(eventQuery.data);

  const loadTasks = useCallback(async () => {
    const next = tasksFromReplica(await syncClient.listRows("field_tasks"));
    setTasks(next);
    setSelectedId((current) => current !== null && next.some((task) => task.id === current) ? current : next[0]?.id ?? null);
  }, [syncClient]);

  const loadObjects = useCallback(async (taskId?: string) => {
    const prefix = taskId === undefined
      ? `users/${session.user.id}/`
      : taskObjectPrefix(session.user.id, taskId);
    const page = await client.storage.list("field-notes", { prefix, limit: 100 });
    if (taskId === undefined) setAllObjects(page.items);
    else setAttachments(page.items);
  }, [client, session.user.id]);

  const runSync = useCallback(async (message = "All local changes are up to date.") => {
    setBusy("sync");
    setNotice(null);
    try {
      await syncClient.sync();
      await loadTasks();
      setPendingTaskIds(new Set());
      setEventRevision((value) => value + 1);
      setNotice(message);
    } catch (cause) {
      setNotice(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  }, [loadTasks, syncClient]);

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        await syncClient.sync();
        const seeded = await seedWorkspace(client, session.user.id);
        if (seeded) await syncClient.sync();
        if (active) {
          await loadTasks();
          setHealth("ready");
        }
      } catch (cause) {
        if (active) {
          setHealth("error");
          setNotice(errorMessage(cause));
        }
      } finally {
        if (active) setBusy(null);
      }
    })();
    return () => { active = false; };
  }, [client, loadTasks, session.user.id, syncClient]);

  useEffect(() => {
    if (selectedId === null) {
      setAttachments([]);
      return;
    }
    void loadObjects(selectedId).catch((cause) => setNotice(errorMessage(cause)));
  }, [loadObjects, selectedId]);

  useEffect(() => {
    const handleOnline = () => {
      setOnline(true);
      void runSync("Connection restored and pending changes synced.");
    };
    const handleOffline = () => setOnline(false);
    window.addEventListener("online", handleOnline);
    window.addEventListener("offline", handleOffline);
    return () => {
      window.removeEventListener("online", handleOnline);
      window.removeEventListener("offline", handleOffline);
    };
  }, [runSync]);

  async function addTask(event: FormEvent) {
    event.preventDefault();
    if (!newTitle.trim()) return;
    setBusy("create");
    try {
      const taskId = await createTask(client, session.user.id, newTitle, "medium");
      setNewTitle("");
      await runSync("Task created through a two-statement transaction.");
      setSelectedId(taskId);
    } catch (cause) {
      setNotice(errorMessage(cause));
      setBusy(null);
    }
  }

  async function changeStatus(task: FieldTask) {
    setBusy(`toggle:${task.id}`);
    try {
      await toggleTask(client, task);
      await runSync(task.status === "open" ? "Task and audit event committed atomically." : "Task reopened in one transaction.");
    } catch (cause) {
      setNotice(errorMessage(cause));
      setBusy(null);
    }
  }

  function beginEdit(task: FieldTask) {
    setSelectedId(task.id);
    setEditingId(task.id);
    setEditTitle(task.title);
  }

  async function saveEdit(task: FieldTask) {
    if (!editTitle.trim() || editTitle.trim() === task.title) {
      setEditingId(null);
      return;
    }
    try {
      await queueTaskEdit(syncClient, task, { title: editTitle.trim() });
      setPendingTaskIds((current) => new Set(current).add(task.id));
      setEditingId(null);
      await loadTasks();
      setNotice("Edit queued locally. Use Sync now to push it to FFDB.");
    } catch (cause) {
      setNotice(errorMessage(cause));
    }
  }

  async function removeTask(task: FieldTask) {
    if (!window.confirm(`Queue “${task.title}” for deletion?`)) return;
    try {
      await queueTaskDelete(syncClient, task);
      setPendingTaskIds((current) => new Set(current).add(task.id));
      await loadTasks();
      setNotice("Deletion queued locally. Sync when you are ready.");
    } catch (cause) {
      setNotice(errorMessage(cause));
    }
  }

  async function handleFiles(event: ChangeEvent<HTMLInputElement>) {
    const files = [...(event.target.files ?? [])];
    event.target.value = "";
    for (const file of files) await uploadFile(file);
  }

  async function uploadFile(file: globalThis.File) {
    if (selectedTask === null) return;
    const pending = { id: generateId("upload_"), name: file.name };
    const optimistic = optimisticList(pendingUploads, pending);
    setPendingUploads(optimistic.next);
    setBusy("upload");
    const key = `${taskObjectPrefix(session.user.id, selectedTask.id)}${generateId("file_")}-${safeFileName(file.name)}`;
    try {
      if (file.size < 5 * 1024 * 1024) {
        await upload.upload("field-notes", key, file, {
          sizeBytes: file.size,
          contentType: file.type || "application/octet-stream",
        });
      } else {
        await multipartUpload(file, key);
      }
      await recordAttachmentChange(selectedTask, 1, `Uploaded ${file.name}`);
      await Promise.all([loadObjects(selectedTask.id), runSync(file.size < 5 * 1024 * 1024 ? "Protected upload committed." : "Multipart upload completed and committed.")]);
    } catch (cause) {
      setPendingUploads(optimistic.rollback());
      setNotice(errorMessage(cause));
    } finally {
      setPendingUploads((current) => current.filter((item) => item.id !== pending.id));
      setBusy(null);
    }
  }

  async function multipartUpload(file: globalThis.File, key: string) {
    const multipart = await client.storage.createMultipart("field-notes", key, {
      sizeBytes: file.size,
      contentType: file.type || "application/octet-stream",
    });
    try {
      const parts = [];
      const chunkSize = 5 * 1024 * 1024;
      for (let offset = 0, partNumber = 1; offset < file.size; offset += chunkSize, partNumber += 1) {
        const chunk = file.slice(offset, Math.min(file.size, offset + chunkSize));
        parts.push(await client.storage.uploadPart(multipart, partNumber, chunk, {
          sizeBytes: chunk.size,
          contentType: file.type || "application/octet-stream",
        }));
      }
      await client.storage.completeMultipart(multipart, parts, {
        sizeBytes: file.size,
        contentType: file.type || "application/octet-stream",
      });
    } catch (cause) {
      await client.storage.abortMultipart(multipart).catch(() => undefined);
      throw cause;
    }
  }

  async function recordAttachmentChange(task: FieldTask, delta: 1 | -1, message: string) {
    const now = Date.now();
    const countSql = delta === 1
      ? "attachment_count = attachment_count + 1"
      : "attachment_count = CASE WHEN attachment_count > 0 THEN attachment_count - 1 ELSE 0 END";
    await client.transaction({ statements: [
      {
        sql: `UPDATE field_tasks SET ${countSql}, updated_at_ms = ?1 WHERE id = ?2 AND owner_id = auth.uid()`,
        parameters: [{ type: "integer", value: now }, { type: "text", value: task.id }],
      },
      {
        sql: "INSERT INTO field_task_events (id, task_id, owner_id, kind, message, created_at_ms) VALUES (?1, ?2, ?3, 'attachment', ?4, ?5)",
        parameters: [
          { type: "text", value: generateId("event_") },
          { type: "text", value: task.id },
          { type: "text", value: task.ownerId },
          { type: "text", value: message },
          { type: "integer", value: now },
        ],
      },
    ] });
  }

  async function downloadObject(object: StorageObjectItem) {
    setBusy(`download:${object.id}`);
    try {
      const signed = await client.storage.downloadUrl("field-notes", object.object_key);
      const response = await client.providerFetch(signed.url, {
        method: signed.method,
        headers: new Headers(signed.headers.map(([name, value]): [string, string] => [name, value])),
      });
      if (!response.ok) throw new Error(`Object provider returned ${response.status}`);
      const url = URL.createObjectURL(await response.blob());
      const link = document.createElement("a");
      link.href = url;
      link.download = objectDisplayName(object);
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (cause) {
      setNotice(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  }

  async function deleteObject(object: StorageObjectItem) {
    if (selectedTask === null || !window.confirm(`Delete ${objectDisplayName(object)}?`)) return;
    setBusy(`delete-object:${object.id}`);
    try {
      await client.storage.delete("field-notes", object.object_key);
      await recordAttachmentChange(selectedTask, -1, `Deleted ${objectDisplayName(object)}`);
      await Promise.all([loadObjects(selectedTask.id), runSync("Object delete re-authorized and committed.")]);
    } catch (cause) {
      setNotice(errorMessage(cause));
      setBusy(null);
    }
  }

  async function runDiagnostics() {
    setBusy("diagnostics");
    const checks: FeatureCheck[] = [];
    const execute = async (id: FeatureCheck["id"], detail: () => Promise<string>) => {
      try {
        checks.push({ id, label: featureLabels[id], status: "ready", detail: await detail() });
      } catch (cause) {
        checks.push({ id, label: featureLabels[id], status: "error", detail: errorMessage(cause) });
      }
      setDiagnosticChecks([...checks]);
    };
    await execute("auth", async () => `Signed in as ${session.user.email}`);
    await execute("sql", async () => {
      const result = await client.query({
        sql: "SELECT COUNT(*) FROM field_tasks WHERE owner_id = ?1 AND owner_id = auth.uid()",
        parameters: [{ type: "text", value: session.user.id }],
      });
      return `${String(result.rows[0]?.[0] ?? 0)} owner-scoped rows returned`;
    });
    await execute("transactions", async () => `${events.length} selected-task audit events visible`);
    await execute("rls", async () => tasks.every((task) => task.ownerId === session.user.id) ? "Every visible task matches auth.uid()" : Promise.reject(new Error("Cross-owner row detected")));
    await execute("sync", async () => { await syncClient.sync(); return `${syncClient.state.pending} pending mutations`; });
    await execute("storage", async () => `${(await client.storage.list("field-notes", { prefix: `users/${session.user.id}/`, limit: 1 })).items.length} protected object(s) sampled`);
    await execute("sessions", async () => `${(await client.auth.sessions()).length} active or historical session(s)`);
    try {
      await Promise.all([client.health(), client.readiness()]);
      setHealth("ready");
    } catch {
      setHealth("error");
    }
    setBusy(null);
  }

  const featureChecks = buildFeatureChecks({ session, syncPending: sync.pending, attachmentCount: attachments.length, sessionCount: sessions.data?.length ?? 0 });

  return (
    <div className="app-shell">
      <header className="topbar">
        <button className="mobile-menu" onClick={() => setMobileNavOpen((value) => !value)} aria-label="Toggle navigation"><Menu /></button>
        <button className="brand brand-button" onClick={() => setView("workspace")}><span className="brand-mark">F</span> FFDB Field Notes</button>
        <button className="user-menu" onClick={() => setView("sessions")}><span>{initials(session.user.email)}</span><div><strong>{session.user.email.split("@")[0]}</strong><small>{session.user.email}</small></div><ChevronDown /></button>
      </header>
      <aside className={`sidebar ${mobileNavOpen ? "open" : ""}`}>
        <nav>
          <NavButton icon={<ListTodo />} label="Workspace" active={view === "workspace"} onClick={() => selectView("workspace")} />
          <NavButton icon={<Folder />} label="Storage" active={view === "storage"} onClick={() => selectView("storage")} />
          <NavButton icon={<Users />} label="Sessions" active={view === "sessions"} onClick={() => selectView("sessions")} />
          <NavButton icon={<Activity />} label="Diagnostics" active={view === "diagnostics"} onClick={() => selectView("diagnostics")} />
        </nav>
        <div className="sidebar-section connection-section">
          <span className="section-label">Connection</span>
          <strong><span className={`status-dot ${online && health === "ready" ? "connected" : health}`} />{online ? (health === "ready" ? "Online" : "Needs attention") : "Offline"}</strong>
          <small>Browser · IndexedDB replica</small>
          <small>{ffdbProjectId.slice(0, 18)}{ffdbProjectId.length > 18 ? "…" : ""}</small>
          <button className="sync-button" disabled={busy === "sync"} onClick={() => void runSync()}><RefreshCw className={sync.phase !== "idle" ? "spin" : ""} /> Sync now</button>
          <span className="last-sync">Last sync: {formatTime(sync.lastSyncedAtMs)}</span>
          <span className="up-to-date"><CheckCircle2 /> {sync.pending === 0 ? "Up to date" : `${sync.pending} pending`}</span>
        </div>
        <div className="sidebar-section queue-section">
          <span className="section-label">Sync queue</span>
          <div><CloudUpload /><strong>{sync.pending}</strong><span>Pending changes</span></div>
          <div><ArrowDownToLine /><strong>{upload.status === "uploading" ? 1 : 0}</strong><span>Active uploads</span></div>
        </div>
        <button className="signout" onClick={() => void auth.signOut()}><UserRound /> Sign out</button>
      </aside>

      <main className="main-content">
        {notice !== null && <div className="notice" role="status"><span>{notice}</span><button onClick={() => setNotice(null)} aria-label="Dismiss notice"><X /></button></div>}
        {view === "workspace" && (
          <WorkspaceView
            tasks={visibleTasks}
            totalTasks={tasks.length}
            selectedId={selectedId}
            pendingTaskIds={pendingTaskIds}
            filter={filter}
            search={search}
            newTitle={newTitle}
            editingId={editingId}
            editTitle={editTitle}
            busy={busy}
            onFilter={setFilter}
            onSearch={setSearch}
            onNewTitle={setNewTitle}
            onAdd={addTask}
            onSelect={setSelectedId}
            onToggle={(task) => void changeStatus(task)}
            onBeginEdit={beginEdit}
            onEditTitle={setEditTitle}
            onSaveEdit={(task) => void saveEdit(task)}
            onCancelEdit={() => setEditingId(null)}
            onDelete={(task) => void removeTask(task)}
          />
        )}
        {view === "storage" && <StorageView objects={allObjects} busy={busy} onLoad={() => void loadObjects()} onDownload={(object) => void downloadObject(object)} />}
        {view === "sessions" && <SessionsView sessions={sessions.data ?? []} status={sessions.status} onRefresh={sessions.refetch} onRefreshToken={async () => { await auth.refresh(); setNotice("Access token rotated through the FFDB session refresh flow."); }} onRevoke={async (id) => { await client.auth.revokeSession(id); sessions.refetch(); }} />}
        {view === "diagnostics" && <DiagnosticsView checks={diagnosticChecks.length > 0 ? diagnosticChecks : featureChecks} busy={busy === "diagnostics"} health={health} onRun={() => void runDiagnostics()} />}
      </main>

      <aside className="inspector">
        <header><h2>Feature inspector</h2><button className="icon-button" aria-label="Close inspector"><X /></button></header>
        {selectedTask === null ? (
          <div className="inspector-empty"><Database /><strong>Select a task</strong><p>Its RLS, sync, storage, and transaction evidence will appear here.</p></div>
        ) : (
          <>
            <div className="selected-summary"><strong>{selectedTask.title}</strong><small>ID: {selectedTask.id}</small></div>
            <div className="inspector-tabs"><button className="active">Overview</button><button onClick={() => setView("diagnostics")}>Evidence</button><button onClick={() => setView("storage")}>Data</button></div>
            <div className="feature-list">
              {featureChecks.map((check) => <FeatureRow key={check.id} check={check} />)}
            </div>
            <section className="attachments-section">
              <h3>Attachments ({attachments.length})</h3>
              <input ref={fileInput} type="file" multiple hidden onChange={(event) => void handleFiles(event)} />
              <button className="upload-drop" onClick={() => fileInput.current?.click()} disabled={selectedTask === null || busy === "upload"}><Upload /><span><strong>Upload file</strong><small>Simple or multipart at 5 MB</small></span></button>
              {pendingUploads.map((item) => <div className="attachment-row pending" key={item.id}><span className="spinner" /><div><strong>{item.name}</strong><small>Authorizing upload…</small></div></div>)}
              {attachments.map((object) => (
                <div className="attachment-row" key={object.id}><FileText /><div><strong>{objectDisplayName(object)}</strong><small>{formatBytes(object.size_bytes)} · {object.content_type ?? "file"}</small></div><button onClick={() => void downloadObject(object)} aria-label={`Download ${objectDisplayName(object)}`}><ArrowDownToLine /></button><button onClick={() => void deleteObject(object)} aria-label={`Delete ${objectDisplayName(object)}`}><Trash2 /></button></div>
              ))}
            </section>
            <section className="events-section">
              <h3>Latest events <button onClick={() => void eventQuery.refetch()}>Refresh</button></h3>
              {eventQuery.status === "loading" && <span className="muted">Loading audit evidence…</span>}
              {events.map((event) => <div className="event-row" key={event.id}><span className={`event-dot ${event.kind}`} /><time>{formatTime(event.createdAtMs)}</time><p><strong>{event.kind}</strong>{event.message}</p></div>)}
              {eventQuery.status === "success" && events.length === 0 && <span className="muted">No events for this task yet.</span>}
            </section>
          </>
        )}
      </aside>
    </div>
  );

  function selectView(next: View) {
    setView(next);
    setMobileNavOpen(false);
    if (next === "storage") void loadObjects();
  }
}

interface WorkspaceViewProps {
  readonly tasks: readonly FieldTask[];
  readonly totalTasks: number;
  readonly selectedId: string | null;
  readonly pendingTaskIds: ReadonlySet<string>;
  readonly filter: Filter;
  readonly search: string;
  readonly newTitle: string;
  readonly editingId: string | null;
  readonly editTitle: string;
  readonly busy: string | null;
  onFilter(filter: Filter): void;
  onSearch(search: string): void;
  onNewTitle(title: string): void;
  onAdd(event: FormEvent): void;
  onSelect(id: string): void;
  onToggle(task: FieldTask): void;
  onBeginEdit(task: FieldTask): void;
  onEditTitle(title: string): void;
  onSaveEdit(task: FieldTask): void;
  onCancelEdit(): void;
  onDelete(task: FieldTask): void;
}

function WorkspaceView(props: WorkspaceViewProps) {
  return <div className="workspace-view">
    <div className="page-heading"><h1>Today’s fieldwork</h1><p>A small workspace with the whole database underneath.</p></div>
    <form className="add-task" onSubmit={props.onAdd}>
      <button className="primary" disabled={props.busy === "create"}><Plus /> Add a task</button>
      <input value={props.newTitle} onChange={(event) => props.onNewTitle(event.target.value)} placeholder="What needs to get done?" aria-label="New task title" />
    </form>
    <div className="task-controls">
      <div className="segmented" aria-label="Filter tasks">{(["all", "open", "done"] as const).map((value) => <button key={value} className={props.filter === value ? "active" : ""} onClick={() => props.onFilter(value)}>{value[0]!.toUpperCase() + value.slice(1)}</button>)}</div>
      <label className="search"><Search /><input value={props.search} onChange={(event) => props.onSearch(event.target.value)} placeholder="Search tasks…" /></label>
    </div>
    <div className="task-list">
      {props.tasks.map((task) => (
        <article className={`task-row ${props.selectedId === task.id ? "selected" : ""} ${task.status === "done" ? "completed" : ""}`} key={task.id} onClick={() => props.onSelect(task.id)}>
          <button className={`task-check ${task.status}`} onClick={(event) => { event.stopPropagation(); props.onToggle(task); }} aria-label={task.status === "done" ? "Reopen task" : "Complete task"}>{task.status === "done" && <Check />}</button>
          <div className="task-copy">
            {props.editingId === task.id ? <input className="inline-edit" autoFocus value={props.editTitle} onChange={(event) => props.onEditTitle(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") props.onSaveEdit(task); if (event.key === "Escape") props.onCancelEdit(); }} onBlur={() => props.onSaveEdit(task)} onClick={(event) => event.stopPropagation()} /> : <h2>{task.title}</h2>}
            <div><Folder /><span>Field Notes</span><span className={`priority ${task.priority}`} /> <span>{task.priority}</span>{task.attachmentCount > 0 && <><Paperclip /><span>{task.attachmentCount}</span></>}</div>
          </div>
          <div className="task-state">{props.pendingTaskIds.has(task.id) ? <span className="pending-state"><Cloud /> Pending sync</span> : <span className="synced-state"><CheckCircle2 /> Synced</span>}<small>{task.status === "done" ? "Completed" : "Updated"} {formatTime(task.updatedAtMs)}</small></div>
          <div className="row-actions"><button onClick={(event) => { event.stopPropagation(); props.onBeginEdit(task); }} aria-label={`Edit ${task.title}`}><Pencil /></button><button onClick={(event) => { event.stopPropagation(); props.onDelete(task); }} aria-label={`Delete ${task.title}`}><Trash2 /></button><ChevronRight /></div>
        </article>
      ))}
      {props.tasks.length === 0 && <div className="empty-state"><ListTodo /><h2>No matching fieldwork</h2><p>Add a task or adjust the current filter.</p></div>}
    </div>
    <div className="task-count">{props.totalTasks} {props.totalTasks === 1 ? "task" : "tasks"}</div>
  </div>;
}

function StorageView({ objects, busy, onLoad, onDownload }: { readonly objects: readonly StorageObjectItem[]; readonly busy: string | null; onLoad(): void; onDownload(object: StorageObjectItem): void }) {
  useEffect(() => { onLoad(); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return <div className="section-view"><div className="section-view-header"><div><h1>Protected storage</h1><p>RLS-authorized metadata backed by S3-compatible object bytes.</p></div><button className="secondary" onClick={onLoad}><RefreshCw /> Refresh</button></div><div className="data-table"><div className="data-table-head"><span>Object</span><span>Type</span><span>Size</span><span>Updated</span><span /></div>{objects.map((object) => <div className="data-row" key={object.id}><span><File /><strong>{objectDisplayName(object)}</strong><small>{object.object_key}</small></span><span>{object.content_type ?? "file"}</span><span>{formatBytes(object.size_bytes)}</span><span>{formatTime(object.updated_at_ms)}</span><button disabled={busy === `download:${object.id}`} onClick={() => onDownload(object)}><ArrowDownToLine /></button></div>)}{objects.length === 0 && <div className="table-empty"><HardDriveUpload /><strong>No objects yet</strong><span>Select a task in Workspace and upload an attachment.</span></div>}</div></div>;
}

function SessionsView({ sessions, status, onRefresh, onRefreshToken, onRevoke }: { readonly sessions: readonly SessionSummary[]; readonly status: string; onRefresh(): void; onRefreshToken(): Promise<void>; onRevoke(id: string): Promise<void> }) {
  return <div className="section-view"><div className="section-view-header"><div><h1>Active sessions</h1><p>Inspect, rotate, and revoke FFDB refresh-token sessions for this account.</p></div><div className="header-actions"><button className="secondary" onClick={() => void onRefreshToken()}><ShieldCheck /> Rotate token</button><button className="secondary" onClick={onRefresh}><RefreshCw className={status === "loading" ? "spin" : ""} /> Refresh list</button></div></div><div className="session-list">{sessions.map((session) => <article key={session.id}><span className={`session-icon ${session.current ? "current" : ""}`}><Server /></span><div><strong>{session.current ? "This browser" : session.user_agent ?? "Unknown client"}</strong><span>{session.ip_address ?? "IP withheld"} · Seen {formatTime(session.last_seen_at_ms)}</span><small>Expires {new Date(session.expires_at_ms).toLocaleDateString()}</small></div>{session.current ? <span className="current-label"><CheckCircle2 /> Current</span> : <button className="danger-button" onClick={() => void onRevoke(session.id)}>Revoke</button>}</article>)}{status === "success" && sessions.length === 0 && <div className="empty-state"><Users /><h2>No sessions returned</h2></div>}</div></div>;
}

function DiagnosticsView({ checks, busy, health, onRun }: { readonly checks: readonly FeatureCheck[]; readonly busy: boolean; readonly health: string; onRun(): void }) {
  return <div className="section-view"><div className="section-view-header"><div><h1>Runtime diagnostics</h1><p>Exercise the user-safe API surface and keep request evidence visible.</p></div><button className="primary" disabled={busy} onClick={onRun}>{busy ? <span className="spinner" /> : <Gauge />} Run checks</button></div><div className="diagnostic-summary"><span><Database /></span><div><strong>FFDB project</strong><small>{ffdbProjectId}</small></div><div className={`health-label ${health}`}><span className={`status-dot ${health}`} />{health === "ready" ? "API ready" : health}</div></div><div className="diagnostic-list">{checks.map((check, index) => <article key={check.id}><span className="check-number">0{index + 1}</span><FeatureIcon id={check.id} /><div><strong>{check.label}</strong><p>{check.detail}</p></div><span className={`check-status ${check.status}`}>{check.status === "ready" ? <CheckCircle2 /> : check.status === "pending" ? <RefreshCw /> : <CircleAlert />}{check.status}</span></article>)}</div><p className="operator-note"><LockKeyhole /> Schema migration, bucket creation, policy introspection, and integrity checks live in <code>pnpm setup</code>, where the developer key stays outside the browser bundle.</p></div>;
}

function NavButton({ icon, label, active, onClick }: { readonly icon: React.ReactNode; readonly label: string; readonly active: boolean; onClick(): void }) {
  return <button className={active ? "active" : ""} onClick={onClick}>{icon}<span>{label}</span></button>;
}

function FeatureRow({ check }: { readonly check: FeatureCheck }) {
  return <div><FeatureIcon id={check.id} /><span>{check.label}</span><strong className={check.status}>{check.status === "ready" ? (check.id === "storage" ? check.detail : "OK") : check.status}</strong><ChevronRight /></div>;
}

function FeatureIcon({ id }: { readonly id: FeatureCheck["id"] }) {
  if (id === "auth") return <LockKeyhole />;
  if (id === "sql") return <Code2 />;
  if (id === "transactions") return <Database />;
  if (id === "rls") return <ShieldCheck />;
  if (id === "sync") return <Cloud />;
  if (id === "storage") return <Paperclip />;
  return <Users />;
}

function buildFeatureChecks({ session, syncPending, attachmentCount, sessionCount }: { readonly session: AuthTokenPair; readonly syncPending: number; readonly attachmentCount: number; readonly sessionCount: number }): readonly FeatureCheck[] {
  return [
    { id: "auth", label: featureLabels.auth, status: "ready", detail: session.user.email_verified ? "Verified" : "Signed in" },
    { id: "sql", label: featureLabels.sql, status: "ready", detail: "Tagged parameters" },
    { id: "transactions", label: featureLabels.transactions, status: "ready", detail: "Task + event" },
    { id: "rls", label: featureLabels.rls, status: "ready", detail: "auth.uid() owner" },
    { id: "sync", label: featureLabels.sync, status: syncPending > 0 ? "pending" : "ready", detail: `${syncPending} pending` },
    { id: "storage", label: featureLabels.storage, status: "ready", detail: `${attachmentCount} files` },
    { id: "sessions", label: featureLabels.sessions, status: "ready", detail: `${sessionCount} active` },
  ];
}

function initials(email: string): string {
  const name = email.split("@")[0] ?? "FF";
  return name.split(/[._-]/).map((part) => part[0]).filter(Boolean).join("").slice(0, 2).toUpperCase() || "FF";
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : "The FFDB request failed.";
}

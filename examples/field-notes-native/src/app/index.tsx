import type { AuthTokenPair, SessionSummary, StorageObjectItem } from "@ffdb/client";
import { FFDBError } from "@ffdb/client";
import type { AutoSyncController, OfflineSyncClient, SyncState } from "@ffdb/sync-client";
import * as Crypto from "expo-crypto";
import * as Linking from "expo-linking";
import Head from "expo-router/head";
import { StatusBar } from "expo-status-bar";
import {
  AppState,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  RefreshControl,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { useCallback, useEffect, useState } from "react";

import {
  configurationError,
  ffdb,
  ffdbProjectId,
  nativeAuthRedirect,
  nativeSyncClient,
} from "@/lib/ffdb";
import { tasksFromReplica, type FieldTask } from "@/lib/model";

type AuthMode = "sign_in" | "register" | "reset";
type AppTab = "fieldwork" | "storage" | "diagnostics";
type ConnectionState = "checking" | "ready" | "offline" | "error";

interface Diagnostic {
  readonly label: string;
  readonly status: "ready" | "error";
  readonly detail: string;
}

const idleSync: SyncState = {
  phase: "idle",
  autoSync: "stopped",
  pending: 0,
  lastSyncedAtMs: null,
  lastChangedAtMs: null,
  error: null,
};

export default function FieldNotesNative() {
  const callbackUrl = Linking.useURL();
  const [session, setSession] = useState<AuthTokenPair | null>(null);
  const [loadingSession, setLoadingSession] = useState(true);
  const [syncClient, setSyncClient] = useState<OfflineSyncClient | null>(null);
  const [syncState, setSyncState] = useState<SyncState>(idleSync);
  const [connection, setConnection] = useState<ConnectionState>("checking");
  const [tasks, setTasks] = useState<readonly FieldTask[]>([]);
  const [pendingIds, setPendingIds] = useState<ReadonlySet<string>>(new Set());
  const [objects, setObjects] = useState<readonly StorageObjectItem[]>([]);
  const [sessions, setSessions] = useState<readonly SessionSummary[]>([]);
  const [diagnostics, setDiagnostics] = useState<readonly Diagnostic[]>([]);
  const [tab, setTab] = useState<AppTab>("fieldwork");
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [newTitle, setNewTitle] = useState("");

  const loadLocal = useCallback(async (client: OfflineSyncClient) => {
    const [records, pending] = await Promise.all([
      client.listRows("field_tasks"),
      client.getPending(Number.MAX_SAFE_INTEGER),
    ]);
    setTasks(tasksFromReplica(records));
    setPendingIds(new Set(pending
      .filter((mutation) => mutation.table === "field_tasks" && typeof mutation.primary_key === "string")
      .map((mutation) => String(mutation.primary_key))));
  }, []);

  const loadAccountEvidence = useCallback(async (activeSession: AuthTokenPair) => {
    if (ffdb === null) return;
    const prefix = `users/${activeSession.user.id}/`;
    const [storagePage, activeSessions] = await Promise.all([
      ffdb.storage.list("field-notes", { prefix, limit: 50 }),
      ffdb.auth.sessions(),
    ]);
    setObjects(storagePage.items);
    setSessions(activeSessions);
  }, []);

  const syncNow = useCallback(async (
    client: OfflineSyncClient,
    activeSession: AuthTokenPair,
    successMessage?: string,
  ) => {
    setBusy("sync");
    setConnection("checking");
    try {
      await client.sync();
      await loadLocal(client);
      await loadAccountEvidence(activeSession);
      setConnection("ready");
      if (successMessage !== undefined) setNotice(successMessage);
    } catch (cause) {
      await loadLocal(client).catch(() => undefined);
      setConnection("error");
      setNotice(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  }, [loadAccountEvidence, loadLocal]);

  useEffect(() => {
    if (ffdb === null) {
      setLoadingSession(false);
      return;
    }
    let active = true;
    void ffdb.auth.session()
      .then((stored) => { if (active) setSession(stored); })
      .catch((cause: unknown) => { if (active) setNotice(errorMessage(cause)); })
      .finally(() => { if (active) setLoadingSession(false); });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    if (session === null) {
      setSyncClient(null);
      setTasks([]);
      setPendingIds(new Set());
      return;
    }
    let active = true;
    let unsubscribe: () => void = () => {};
    let autoSync: AutoSyncController | null = null;
    void nativeSyncClient(session).then(async (client) => {
      if (!active) return;
      setSyncClient(client);
      let lastChangedAtMs = client.state.lastChangedAtMs;
      unsubscribe = client.subscribe((state) => {
        if (!active) return;
        setSyncState(state);
        if (state.phase === "error" || state.autoSync === "backoff") setConnection("error");
        else if (state.lastSyncedAtMs !== null) setConnection("ready");
        if (state.lastChangedAtMs !== null && state.lastChangedAtMs !== lastChangedAtMs) {
          lastChangedAtMs = state.lastChangedAtMs;
          void Promise.all([loadLocal(client), loadAccountEvidence(session)])
            .catch((cause: unknown) => {
              if (active) setNotice(errorMessage(cause));
            });
        }
      });
      await Promise.all([loadLocal(client), loadAccountEvidence(session)]);
      if (!active) return;
      autoSync = client.startAutoSync({ active: AppState.currentState === "active" });
    }).catch((cause: unknown) => {
      if (active) {
        setConnection("error");
        setNotice(errorMessage(cause));
      }
    });
    const appState = AppState.addEventListener("change", (state) => {
      autoSync?.setActive(state === "active");
    });
    return () => {
      active = false;
      autoSync?.stop();
      unsubscribe();
      appState.remove();
    };
  }, [loadAccountEvidence, loadLocal, session]);

  const authCallbackFinished = callbackUrl?.startsWith(nativeAuthRedirect) === true;

  if (loadingSession) return <><AppHead /><LoadingScreen label="Opening the secure session…" /></>;
  if (configurationError !== null || ffdb === null) {
    return <><AppHead /><ConfigurationScreen message={configurationError ?? "FFDB is not configured."} /></>;
  }
  if (session === null) {
    return <><AppHead /><AuthScreen
      callbackFinished={authCallbackFinished}
      busy={busy}
      notice={notice}
      onNotice={setNotice}
      onBusy={setBusy}
      onAuthenticated={setSession}
    /></>;
  }

  async function createTask() {
    if (ffdb === null || syncClient === null || newTitle.trim() === "" || session === null) return;
    setBusy("create");
    setNotice(null);
    const id = `task_${Crypto.randomUUID()}`;
    const now = Date.now();
    try {
      await ffdb.transaction({ statements: [
        {
          sql: "INSERT INTO field_tasks (id, owner_id, title, notes, status, priority, attachment_count, created_at_ms, updated_at_ms) VALUES (?1, auth.uid(), ?2, '', 'open', 'medium', 0, ?3, ?3)",
          parameters: [
            { type: "text", value: id },
            { type: "text", value: newTitle.trim() },
            { type: "integer", value: now },
          ],
        },
        {
          sql: "INSERT INTO field_task_events (id, task_id, owner_id, kind, message, created_at_ms) VALUES (?1, ?2, auth.uid(), 'created', 'Created from Expo in one transaction', ?3)",
          parameters: [
            { type: "text", value: `event_${Crypto.randomUUID()}` },
            { type: "text", value: id },
            { type: "integer", value: now },
          ],
        },
      ] }, { idempotencyKey: `native-create:${id}` });
      setNewTitle("");
      await syncNow(syncClient, session, "Task committed and pulled into the native SQLite replica.");
    } catch (cause) {
      setNotice(errorMessage(cause));
      setConnection("error");
      setBusy(null);
    }
  }

  async function toggleTask(task: FieldTask) {
    if (ffdb === null || syncClient === null || session === null) return;
    setBusy(`toggle:${task.id}`);
    const status = task.status === "open" ? "done" : "open";
    const now = Date.now();
    try {
      await ffdb.transaction({ statements: [
        {
          sql: "UPDATE field_tasks SET status = ?1, updated_at_ms = ?2 WHERE id = ?3 AND owner_id = auth.uid()",
          parameters: [
            { type: "text", value: status },
            { type: "integer", value: now },
            { type: "text", value: task.id },
          ],
        },
        {
          sql: "INSERT INTO field_task_events (id, task_id, owner_id, kind, message, created_at_ms) VALUES (?1, ?2, auth.uid(), 'status', ?3, ?4)",
          parameters: [
            { type: "text", value: `event_${Crypto.randomUUID()}` },
            { type: "text", value: task.id },
            { type: "text", value: status === "done" ? "Completed from Expo" : "Reopened from Expo" },
            { type: "integer", value: now },
          ],
        },
      ] }, { idempotencyKey: `native-toggle:${task.id}:${now}` });
      await syncNow(syncClient, session, "Task and audit event committed atomically.");
    } catch (cause) {
      setNotice(errorMessage(cause));
      setBusy(null);
    }
  }

  async function queueFieldNote(task: FieldTask) {
    if (syncClient === null) return;
    const now = Date.now();
    try {
      await syncClient.mutate({
        mutation_id: `mutation_${Crypto.randomUUID()}`,
        table: "field_tasks",
        primary_key: task.id,
        operation: "update",
        values: {
          notes: `Offline field note queued at ${new Date(now).toLocaleTimeString()}`,
          updated_at_ms: now,
        },
        base_row_version: task.rowVersion,
        client_timestamp_ms: now,
      });
      await loadLocal(syncClient);
      setNotice("Saved to native SQLite. Live sync will push this field note automatically.");
    } catch (cause) {
      setNotice(errorMessage(cause));
    }
  }

  async function queueDelete(task: FieldTask) {
    if (syncClient === null) return;
    try {
      await syncClient.mutate({
        mutation_id: `mutation_${Crypto.randomUUID()}`,
        table: "field_tasks",
        primary_key: task.id,
        operation: "delete",
        values: null,
        base_row_version: task.rowVersion,
        client_timestamp_ms: Date.now(),
      });
      await loadLocal(syncClient);
      setNotice("Deletion is local and queued. Live sync will apply it automatically.");
    } catch (cause) {
      setNotice(errorMessage(cause));
    }
  }

  async function uploadSample() {
    if (ffdb === null || session === null) return;
    setBusy("upload");
    try {
      const body = new Blob([
        `FFDB Field Notes native upload\nUser: ${session.user.id}\nCreated: ${new Date().toISOString()}\n`,
      ], { type: "text/plain" });
      const key = `users/${session.user.id}/native-samples/note_${Crypto.randomUUID()}.txt`;
      await ffdb.storage.upload("field-notes", key, body, {
        sizeBytes: body.size,
        contentType: "text/plain",
      });
      await loadAccountEvidence(session);
      setNotice("Protected text sample uploaded and committed.");
    } catch (cause) {
      setNotice(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  }

  async function downloadSample(object: StorageObjectItem) {
    if (ffdb === null) return;
    setBusy(`download:${object.id}`);
    try {
      const signed = await ffdb.storage.downloadUrl("field-notes", object.object_key);
      const response = await ffdb.providerFetch(signed.url, {
        method: signed.method,
        headers: new Headers(signed.headers.map(([name, value]) => [name, value])),
      });
      if (!response.ok) throw new Error(`Object provider returned ${response.status}`);
      const bytes = (await response.arrayBuffer()).byteLength;
      setNotice(`Authorized download returned ${bytes.toLocaleString()} bytes.`);
    } catch (cause) {
      setNotice(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  }

  async function runDiagnostics() {
    if (ffdb === null || session === null || syncClient === null) return;
    const client = ffdb;
    setBusy("diagnostics");
    const next: Diagnostic[] = [];
    const check = async (label: string, work: () => Promise<string>) => {
      try {
        next.push({ label, status: "ready", detail: await work() });
      } catch (cause) {
        next.push({ label, status: "error", detail: errorMessage(cause) });
      }
      setDiagnostics([...next]);
    };
    await check("API readiness", async () => {
      await Promise.all([client.health(), client.readiness()]);
      return "Health and readiness endpoints passed";
    });
    await check("Authenticated SQL", async () => {
      const result = await client.query({
        sql: "SELECT COUNT(*) FROM field_tasks WHERE owner_id = auth.uid()",
      }, { idempotencyKey: `native-diagnostic:${Crypto.randomUUID()}` });
      return `${String(result.rows[0]?.[0] ?? 0)} owner-scoped rows`;
    });
    await check("Native SQLite sync", async () => {
      await syncClient.sync();
      await loadLocal(syncClient);
      return `${syncClient.state.pending} pending mutations`;
    });
    await check("Protected storage", async () => {
      const page = await client.storage.list("field-notes", {
        prefix: `users/${session.user.id}/`,
        limit: 1,
      });
      return `${page.items.length} object sampled through RLS`;
    });
    await check("Session management", async () => `${(await client.auth.sessions()).length} sessions visible`);
    setConnection(next.every((item) => item.status === "ready") ? "ready" : "error");
    setBusy(null);
  }

  async function signOut() {
    if (ffdb === null) return;
    setBusy("signout");
    try {
      await ffdb.auth.signOut();
    } finally {
      setSession(null);
      setBusy(null);
    }
  }

  return (<>
    <AppHead />
    <SafeAreaView style={styles.safeArea} edges={["top", "left", "right"]}>
      <StatusBar style="dark" />
      <View style={styles.shell}>
        <View style={styles.header}>
          <View style={styles.brandLockup}>
            <View style={styles.brandMark}><Text style={styles.brandMarkText}>F</Text></View>
            <View><Text style={styles.eyebrow}>FFDB NATIVE LAB</Text><Text style={styles.brandTitle}>Field Notes</Text></View>
          </View>
          <Pressable style={styles.avatar} onPress={() => setTab("diagnostics")}>
            <Text style={styles.avatarText}>{initials(session.user.email)}</Text>
          </Pressable>
        </View>

        {notice !== null && (
          <Pressable style={styles.notice} onPress={() => setNotice(null)}>
            <Text style={styles.noticeText}>{notice}</Text><Text style={styles.noticeClose}>×</Text>
          </Pressable>
        )}

        <ScrollView
          style={styles.scroll}
          contentContainerStyle={styles.scrollContent}
          keyboardShouldPersistTaps="handled"
          refreshControl={<RefreshControl
            refreshing={busy === "sync"}
            onRefresh={() => { if (syncClient !== null) void syncNow(syncClient, session, "Server state refreshed."); }}
            tintColor={palette.green}
          />}
        >
          <ConnectionCard
            connection={connection}
            syncState={syncState}
            onSync={() => { if (syncClient !== null) void syncNow(syncClient, session, "All native changes are server-confirmed."); }}
          />

          {tab === "fieldwork" && (
            <FieldworkView
              tasks={tasks}
              pendingIds={pendingIds}
              newTitle={newTitle}
              busy={busy}
              connection={connection}
              onTitle={setNewTitle}
              onCreate={() => void createTask()}
              onToggle={(task) => void toggleTask(task)}
              onQueueNote={(task) => void queueFieldNote(task)}
              onDelete={(task) => void queueDelete(task)}
            />
          )}
          {tab === "storage" && (
            <StorageView
              objects={objects}
              busy={busy}
              onUpload={() => void uploadSample()}
              onDownload={(object) => void downloadSample(object)}
            />
          )}
          {tab === "diagnostics" && (
            <DiagnosticsView
              diagnostics={diagnostics}
              sessions={sessions}
              session={session}
              busy={busy}
              onRun={() => void runDiagnostics()}
              onRefreshToken={async () => {
                if (ffdb === null) return;
                setSession(await ffdb.auth.refresh());
                setNotice("Access and refresh tokens rotated through SecureStore.");
              }}
              onSignOut={() => void signOut()}
            />
          )}
        </ScrollView>

        <View style={styles.tabBar}>
          <TabButton label="Fieldwork" glyph="✓" active={tab === "fieldwork"} onPress={() => setTab("fieldwork")} />
          <TabButton label="Storage" glyph="□" active={tab === "storage"} onPress={() => setTab("storage")} />
          <TabButton label="Evidence" glyph="⌁" active={tab === "diagnostics"} onPress={() => setTab("diagnostics")} />
        </View>
      </View>
    </SafeAreaView>
  </>);
}

function AppHead() {
  return <Head><title>FFDB Field Notes</title><meta name="description" content="An Expo reference app for FFDB auth, offline sync, SQL, storage, and sessions." /><meta name="theme-color" content="#13261A" /></Head>;
}

function AuthScreen({
  callbackFinished,
  busy,
  notice,
  onNotice,
  onBusy,
  onAuthenticated,
}: {
  readonly callbackFinished: boolean;
  readonly busy: string | null;
  readonly notice: string | null;
  onNotice(message: string | null): void;
  onBusy(value: string | null): void;
  onAuthenticated(session: AuthTokenPair): void;
}) {
  const [mode, setMode] = useState<AuthMode>("sign_in");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");

  async function submit() {
    if (ffdb === null || email.trim() === "") return;
    onBusy("auth");
    onNotice(null);
    try {
      if (mode === "sign_in") {
        if (password === "") throw new Error("Enter your password.");
        onAuthenticated(await ffdb.auth.signIn(email.trim(), password));
      } else if (mode === "register") {
        if (password.length < 8) throw new Error("Use at least 8 characters for the password.");
        const result = await ffdb.auth.register({
          email: email.trim(),
          password,
          redirect_to: nativeAuthRedirect,
        });
        onNotice(result.verification_required
          ? "Check your email. FFDB will verify the address, then return to this app."
          : "Account created. You can sign in now.");
        setMode("sign_in");
      } else {
        await ffdb.auth.startPasswordReset(email.trim(), { redirectTo: nativeAuthRedirect });
        onNotice("If the account exists, FFDB sent a secure password-reset email.");
        setMode("sign_in");
      }
    } catch (cause) {
      onNotice(errorMessage(cause));
    } finally {
      onBusy(null);
    }
  }

  return (
    <KeyboardAvoidingView style={styles.authBackground} behavior={Platform.OS === "ios" ? "padding" : undefined}>
      <StatusBar style="light" />
      <SafeAreaView style={styles.authSafeArea}>
        <View style={styles.authHero}>
          <View style={styles.authMonogram}><Text style={styles.authMonogramText}>F</Text></View>
          <Text style={styles.authEyebrow}>FFDB · EXPO REFERENCE APP</Text>
          <Text style={styles.authTitle}>Carry the whole database into the field.</Text>
          <Text style={styles.authLead}>Secure sessions, native SQLite sync, RLS, storage, and diagnostics—on one small screen.</Text>
        </View>
        <View style={styles.authCard}>
          <View style={styles.authModes}>
            <AuthModeButton label="Sign in" active={mode === "sign_in"} onPress={() => setMode("sign_in")} />
            <AuthModeButton label="Create" active={mode === "register"} onPress={() => setMode("register")} />
            <AuthModeButton label="Reset" active={mode === "reset"} onPress={() => setMode("reset")} />
          </View>
          {(callbackFinished || notice !== null) && (
            <Text style={styles.authNotice}>{callbackFinished ? "Email action completed. Sign in to continue." : notice}</Text>
          )}
          <Text style={styles.inputLabel}>EMAIL</Text>
          <TextInput
            style={styles.input}
            value={email}
            onChangeText={setEmail}
            autoCapitalize="none"
            keyboardType="email-address"
            autoComplete="email"
            placeholder="you@example.com"
            placeholderTextColor={palette.muted}
          />
          {mode !== "reset" && <>
            <Text style={styles.inputLabel}>PASSWORD</Text>
            <TextInput
              style={styles.input}
              value={password}
              onChangeText={setPassword}
              secureTextEntry
              autoComplete={mode === "register" ? "new-password" : "current-password"}
              placeholder="••••••••••••"
              placeholderTextColor={palette.muted}
            />
          </>}
          <Pressable style={[styles.primaryButton, busy === "auth" && styles.disabled]} disabled={busy === "auth"} onPress={() => void submit()}>
            <Text style={styles.primaryButtonText}>{busy === "auth" ? "Working…" : mode === "sign_in" ? "Open field notes" : mode === "register" ? "Create verified account" : "Send reset email"}</Text>
          </Pressable>
          <Text style={styles.authFootnote}>Callback to allowlist: {nativeAuthRedirect}</Text>
        </View>
      </SafeAreaView>
    </KeyboardAvoidingView>
  );
}

function ConnectionCard({ connection, syncState, onSync }: {
  readonly connection: ConnectionState;
  readonly syncState: SyncState;
  onSync(): void;
}) {
  const ready = connection === "ready" && syncState.phase !== "error" && syncState.autoSync !== "backoff";
  const label = syncState.autoSync === "watching" && ready
    ? "Live sync"
    : syncState.autoSync === "syncing"
      ? "Syncing FFDB"
      : syncState.autoSync === "backoff"
        ? "Retrying automatically"
        : ready
          ? "Server confirmed"
          : connection === "checking"
            ? "Checking FFDB"
            : connection === "offline"
              ? "Working offline"
              : "Needs attention";
  return <View style={styles.connectionCard}>
    <View style={styles.connectionCopy}>
      <View style={styles.connectionHeading}><View style={[styles.dot, ready ? styles.dotReady : styles.dotError]} /><Text style={styles.connectionTitle}>{label}</Text></View>
      <Text style={styles.connectionMeta}>Expo · SecureStore · native SQLite</Text>
      <Text style={styles.connectionMeta}>{syncState.pending} pending · Last sync {formatTime(syncState.lastSyncedAtMs)}</Text>
    </View>
    <Pressable style={styles.syncButton} onPress={onSync}><Text style={styles.syncGlyph}>↻</Text><Text style={styles.syncButtonText}>Sync now</Text></Pressable>
  </View>;
}

function FieldworkView({ tasks, pendingIds, newTitle, busy, connection, onTitle, onCreate, onToggle, onQueueNote, onDelete }: {
  readonly tasks: readonly FieldTask[];
  readonly pendingIds: ReadonlySet<string>;
  readonly newTitle: string;
  readonly busy: string | null;
  readonly connection: ConnectionState;
  onTitle(value: string): void;
  onCreate(): void;
  onToggle(task: FieldTask): void;
  onQueueNote(task: FieldTask): void;
  onDelete(task: FieldTask): void;
}) {
  return <View>
    <View style={styles.sectionHeading}>
      <Text style={styles.sectionEyebrow}>TODAY’S FIELDWORK</Text>
      <Text style={styles.sectionTitle}>A durable list with the database underneath.</Text>
    </View>
    <View style={styles.composer}>
      <TextInput style={styles.composerInput} value={newTitle} onChangeText={onTitle} placeholder="What needs to get done?" placeholderTextColor={palette.muted} returnKeyType="done" onSubmitEditing={onCreate} />
      <Pressable style={[styles.addButton, (busy === "create" || newTitle.trim() === "") && styles.disabled]} disabled={busy === "create" || newTitle.trim() === ""} onPress={onCreate}><Text style={styles.addButtonText}>＋ Add</Text></Pressable>
    </View>
    <View style={styles.listHeader}><Text style={styles.listHeaderText}>ALL TASKS</Text><Text style={styles.countPill}>{tasks.length}</Text></View>
    {tasks.map((task) => {
      const local = pendingIds.has(task.id) || task.serverSequence < 0;
      return <View style={styles.taskCard} key={task.id}>
        <Pressable style={[styles.checkButton, task.status === "done" && styles.checkButtonDone]} onPress={() => onToggle(task)}>
          <Text style={[styles.checkText, task.status === "done" && styles.checkTextDone]}>{task.status === "done" ? "✓" : ""}</Text>
        </Pressable>
        <View style={styles.taskBody}>
          <Text style={[styles.taskTitle, task.status === "done" && styles.taskTitleDone]}>{task.title}</Text>
          <Text style={styles.taskMeta}>{task.priority.toUpperCase()} · {task.notes || "No field note yet"}</Text>
          <Text style={[styles.persistence, local ? styles.persistenceLocal : connection === "ready" ? styles.persistenceReady : styles.persistenceUnknown]}>
            {local ? "● LOCAL CHANGE" : connection === "ready" ? "✓ SERVER CONFIRMED" : "! LAST CONFIRMED"}
          </Text>
          <View style={styles.taskActions}>
            <Pressable onPress={() => onQueueNote(task)}><Text style={styles.textAction}>Queue note</Text></Pressable>
            <Pressable onPress={() => onDelete(task)}><Text style={styles.deleteAction}>Queue delete</Text></Pressable>
          </View>
        </View>
      </View>;
    })}
    {tasks.length === 0 && <View style={styles.emptyCard}><Text style={styles.emptyGlyph}>☷</Text><Text style={styles.emptyTitle}>No fieldwork yet</Text><Text style={styles.emptyText}>Create a task to exercise authenticated SQL and atomic events.</Text></View>}
  </View>;
}

function StorageView({ objects, busy, onUpload, onDownload }: {
  readonly objects: readonly StorageObjectItem[];
  readonly busy: string | null;
  onUpload(): void;
  onDownload(object: StorageObjectItem): void;
}) {
  return <View>
    <View style={styles.sectionHeading}><Text style={styles.sectionEyebrow}>PROTECTED OBJECTS</Text><Text style={styles.sectionTitle}>Signed operations without shipping cloud credentials.</Text></View>
    <Pressable style={styles.uploadCard} onPress={onUpload} disabled={busy === "upload"}>
      <Text style={styles.uploadGlyph}>↑</Text><View style={styles.uploadCopy}><Text style={styles.uploadTitle}>{busy === "upload" ? "Authorizing…" : "Upload a native sample"}</Text><Text style={styles.uploadText}>Creates a text Blob, signs the provider request, then commits metadata.</Text></View>
    </Pressable>
    {objects.map((object) => <Pressable key={object.id} style={styles.objectRow} onPress={() => onDownload(object)}>
      <View style={styles.objectIcon}><Text style={styles.objectIconText}>TXT</Text></View>
      <View style={styles.objectCopy}><Text style={styles.objectName} numberOfLines={1}>{object.object_key.split("/").at(-1)}</Text><Text style={styles.objectMeta}>{formatBytes(object.size_bytes)} · {object.content_type ?? "file"}</Text></View>
      <Text style={styles.chevron}>{busy === `download:${object.id}` ? "…" : "↓"}</Text>
    </Pressable>)}
    {objects.length === 0 && <View style={styles.emptyCard}><Text style={styles.emptyGlyph}>□</Text><Text style={styles.emptyTitle}>No protected objects</Text><Text style={styles.emptyText}>Upload the generated sample to test storage authorization.</Text></View>}
  </View>;
}

function DiagnosticsView({ diagnostics, sessions, session, busy, onRun, onRefreshToken, onSignOut }: {
  readonly diagnostics: readonly Diagnostic[];
  readonly sessions: readonly SessionSummary[];
  readonly session: AuthTokenPair;
  readonly busy: string | null;
  onRun(): void;
  onRefreshToken(): Promise<void>;
  onSignOut(): void;
}) {
  return <View>
    <View style={styles.sectionHeading}><Text style={styles.sectionEyebrow}>FEATURE EVIDENCE</Text><Text style={styles.sectionTitle}>Make every network boundary explain itself.</Text></View>
    <View style={styles.identityCard}><Text style={styles.identityLabel}>SECURE SESSION</Text><Text style={styles.identityEmail}>{session.user.email}</Text><Text style={styles.identityMeta}>{session.user.email_verified ? "Verified email" : "Verification pending"} · {sessions.length} session(s)</Text><Text style={styles.identityMeta}>Project {ffdbProjectId.slice(0, 18)}…</Text></View>
    <View style={styles.buttonRow}>
      <Pressable style={styles.secondaryButton} onPress={() => void onRefreshToken()}><Text style={styles.secondaryButtonText}>Rotate token</Text></Pressable>
      <Pressable style={styles.primarySmall} onPress={onRun}><Text style={styles.primarySmallText}>{busy === "diagnostics" ? "Running…" : "Run checks"}</Text></Pressable>
    </View>
    {diagnostics.map((diagnostic, index) => <View style={styles.diagnosticRow} key={diagnostic.label}>
      <Text style={styles.diagnosticNumber}>0{index + 1}</Text><View style={styles.diagnosticCopy}><Text style={styles.diagnosticLabel}>{diagnostic.label}</Text><Text style={styles.diagnosticDetail}>{diagnostic.detail}</Text></View><Text style={diagnostic.status === "ready" ? styles.diagnosticReady : styles.diagnosticError}>{diagnostic.status === "ready" ? "PASS" : "FAIL"}</Text>
    </View>)}
    {diagnostics.length === 0 && <View style={styles.emptyCard}><Text style={styles.emptyGlyph}>⌁</Text><Text style={styles.emptyTitle}>Evidence is ready to collect</Text><Text style={styles.emptyText}>Run checks for auth.uid(), sync, sessions, readiness, and storage.</Text></View>}
    <Pressable style={styles.signOutButton} onPress={onSignOut}><Text style={styles.signOutText}>Sign out and clear SecureStore</Text></Pressable>
  </View>;
}

function TabButton({ label, glyph, active, onPress }: { readonly label: string; readonly glyph: string; readonly active: boolean; onPress(): void }) {
  return <Pressable style={[styles.tabButton, active && styles.tabButtonActive]} onPress={onPress}><Text style={[styles.tabGlyph, active && styles.tabTextActive]}>{glyph}</Text><Text style={[styles.tabLabel, active && styles.tabTextActive]}>{label}</Text></Pressable>;
}

function AuthModeButton({ label, active, onPress }: { readonly label: string; readonly active: boolean; onPress(): void }) {
  return <Pressable style={[styles.authMode, active && styles.authModeActive]} onPress={onPress}><Text style={[styles.authModeText, active && styles.authModeTextActive]}>{label}</Text></Pressable>;
}

function LoadingScreen({ label }: { readonly label: string }) {
  return <SafeAreaView style={styles.loading}><StatusBar style="light" /><View style={styles.authMonogram}><Text style={styles.authMonogramText}>F</Text></View><Text style={styles.loadingText}>{label}</Text></SafeAreaView>;
}

function ConfigurationScreen({ message }: { readonly message: string }) {
  return <SafeAreaView style={styles.configuration}><Text style={styles.configEyebrow}>CONFIGURATION REQUIRED</Text><Text style={styles.configTitle}>Connect this native build to an FFDB project.</Text><Text style={styles.configText}>{message}</Text><Text style={styles.configCode}>Copy .env.example to .env.local, then restart Expo.</Text></SafeAreaView>;
}

function errorMessage(cause: unknown): string {
  if (cause instanceof FFDBError) {
    const request = cause.requestId === null ? "" : ` · Request ${cause.requestId}`;
    return `${cause.message} · ${cause.code}${request}`;
  }
  return cause instanceof Error ? cause.message : "The FFDB request failed.";
}

function initials(email: string): string {
  return email.split("@")[0]?.split(/[._-]/).map((part) => part[0]).filter(Boolean).join("").slice(0, 2).toUpperCase() || "FN";
}

function formatTime(timestamp: number | null): string {
  return timestamp === null ? "never" : new Date(timestamp).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const palette = {
  ink: "#1E211D",
  cream: "#F3F0E7",
  paper: "#FAF8F1",
  rule: "#D6D0C2",
  green: "#175C37",
  greenSoft: "#DCE8DC",
  orange: "#D87913",
  red: "#A74D43",
  muted: "#74776F",
  white: "#FFFFFF",
};

const styles = StyleSheet.create({
  safeArea: { flex: 1, backgroundColor: palette.paper },
  shell: { flex: 1, backgroundColor: palette.cream },
  header: { height: 74, paddingHorizontal: 20, borderBottomWidth: 1, borderBottomColor: palette.rule, backgroundColor: palette.paper, flexDirection: "row", alignItems: "center", justifyContent: "space-between" },
  brandLockup: { flexDirection: "row", alignItems: "center", gap: 12 },
  brandMark: { width: 38, height: 38, borderBottomWidth: 3, borderBottomColor: palette.green, alignItems: "center", justifyContent: "center" },
  brandMarkText: { fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), fontSize: 29, fontWeight: "700", fontStyle: "italic", color: palette.green },
  eyebrow: { fontSize: 9, fontWeight: "800", letterSpacing: 1.5, color: palette.muted },
  brandTitle: { fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), fontSize: 22, fontWeight: "700", color: palette.ink },
  avatar: { width: 38, height: 38, borderRadius: 19, backgroundColor: palette.green, alignItems: "center", justifyContent: "center" },
  avatarText: { color: palette.white, fontWeight: "800", fontSize: 12 },
  notice: { marginHorizontal: 14, marginTop: 12, padding: 13, borderWidth: 1, borderColor: "#B7C9B9", borderRadius: 8, backgroundColor: "#EAF2E8", flexDirection: "row", alignItems: "flex-start", gap: 10 },
  noticeText: { flex: 1, color: palette.green, fontSize: 12, lineHeight: 18 },
  noticeClose: { color: palette.green, fontSize: 19, lineHeight: 19 },
  scroll: { flex: 1 },
  scrollContent: { padding: 14, paddingBottom: 110, gap: 18 },
  connectionCard: { backgroundColor: palette.ink, borderRadius: 12, padding: 16, flexDirection: "row", alignItems: "center", justifyContent: "space-between" },
  connectionCopy: { gap: 4, flexShrink: 1 },
  connectionHeading: { flexDirection: "row", alignItems: "center", gap: 8 },
  dot: { width: 8, height: 8, borderRadius: 4 },
  dotReady: { backgroundColor: "#75C990" },
  dotError: { backgroundColor: "#E37B6F" },
  connectionTitle: { color: palette.white, fontWeight: "800", fontSize: 15 },
  connectionMeta: { color: "#B8BDB4", fontSize: 11 },
  syncButton: { minHeight: 42, paddingHorizontal: 14, borderWidth: 1, borderColor: "#4D554B", borderRadius: 8, flexDirection: "row", alignItems: "center", gap: 6 },
  syncGlyph: { color: palette.white, fontSize: 21 },
  syncButtonText: { color: palette.white, fontWeight: "800", fontSize: 12 },
  sectionHeading: { paddingTop: 8, marginBottom: 14, gap: 6 },
  sectionEyebrow: { fontSize: 10, letterSpacing: 1.8, fontWeight: "900", color: palette.green },
  sectionTitle: { fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), fontSize: 30, lineHeight: 35, fontWeight: "700", color: palette.ink, maxWidth: 380 },
  composer: { flexDirection: "row", gap: 8, marginBottom: 18 },
  composerInput: { flex: 1, minHeight: 52, borderWidth: 1, borderColor: palette.rule, borderRadius: 8, backgroundColor: palette.paper, paddingHorizontal: 14, fontSize: 15, color: palette.ink },
  addButton: { minWidth: 82, borderRadius: 8, backgroundColor: palette.green, alignItems: "center", justifyContent: "center", paddingHorizontal: 12 },
  addButtonText: { color: palette.white, fontWeight: "800", fontSize: 14 },
  disabled: { opacity: 0.5 },
  listHeader: { flexDirection: "row", alignItems: "center", gap: 8, marginBottom: 8 },
  listHeaderText: { fontSize: 10, letterSpacing: 1.5, fontWeight: "800", color: palette.muted },
  countPill: { minWidth: 24, textAlign: "center", overflow: "hidden", borderRadius: 12, backgroundColor: palette.greenSoft, color: palette.green, fontSize: 11, fontWeight: "800", paddingVertical: 3 },
  taskCard: { borderTopWidth: 1, borderTopColor: palette.rule, paddingVertical: 16, flexDirection: "row", gap: 12 },
  checkButton: { width: 32, height: 32, borderRadius: 6, borderWidth: 1.5, borderColor: "#9C9D96", alignItems: "center", justifyContent: "center", backgroundColor: palette.paper },
  checkButtonDone: { backgroundColor: palette.green, borderColor: palette.green },
  checkText: { fontSize: 18, color: palette.muted },
  checkTextDone: { color: palette.white },
  taskBody: { flex: 1, gap: 5 },
  taskTitle: { color: palette.ink, fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), fontSize: 19, fontWeight: "700" },
  taskTitleDone: { textDecorationLine: "line-through", color: palette.muted },
  taskMeta: { color: palette.muted, fontSize: 11, lineHeight: 16 },
  persistence: { fontSize: 9, fontWeight: "900", letterSpacing: 1 },
  persistenceLocal: { color: palette.orange },
  persistenceReady: { color: palette.green },
  persistenceUnknown: { color: palette.red },
  taskActions: { flexDirection: "row", gap: 18, paddingTop: 4 },
  textAction: { color: palette.green, fontSize: 11, fontWeight: "800" },
  deleteAction: { color: palette.red, fontSize: 11, fontWeight: "800" },
  emptyCard: { borderWidth: 1, borderColor: palette.rule, borderRadius: 10, borderStyle: "dashed", alignItems: "center", padding: 30, backgroundColor: "rgba(250,248,241,0.55)" },
  emptyGlyph: { color: palette.green, fontSize: 34, marginBottom: 8 },
  emptyTitle: { fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), color: palette.ink, fontSize: 20, fontWeight: "700" },
  emptyText: { textAlign: "center", color: palette.muted, fontSize: 12, lineHeight: 18, marginTop: 5, maxWidth: 280 },
  uploadCard: { borderWidth: 1, borderColor: palette.green, borderRadius: 12, padding: 17, flexDirection: "row", alignItems: "center", gap: 14, marginBottom: 18, backgroundColor: "#EDF3EA" },
  uploadGlyph: { width: 44, height: 44, borderRadius: 22, textAlign: "center", lineHeight: 42, overflow: "hidden", backgroundColor: palette.green, color: palette.white, fontSize: 25 },
  uploadCopy: { flex: 1, gap: 4 },
  uploadTitle: { color: palette.green, fontSize: 15, fontWeight: "800" },
  uploadText: { color: palette.muted, fontSize: 11, lineHeight: 16 },
  objectRow: { minHeight: 68, borderTopWidth: 1, borderTopColor: palette.rule, flexDirection: "row", alignItems: "center", gap: 12 },
  objectIcon: { width: 38, height: 42, borderRadius: 5, backgroundColor: palette.greenSoft, alignItems: "center", justifyContent: "center" },
  objectIconText: { fontSize: 9, fontWeight: "900", color: palette.green },
  objectCopy: { flex: 1, gap: 3 },
  objectName: { color: palette.ink, fontSize: 13, fontWeight: "700" },
  objectMeta: { color: palette.muted, fontSize: 11 },
  chevron: { color: palette.green, fontSize: 20 },
  identityCard: { borderWidth: 1, borderColor: palette.rule, borderRadius: 12, padding: 18, backgroundColor: palette.paper, gap: 5 },
  identityLabel: { fontSize: 9, letterSpacing: 1.5, color: palette.green, fontWeight: "900" },
  identityEmail: { fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), color: palette.ink, fontSize: 20, fontWeight: "700" },
  identityMeta: { color: palette.muted, fontSize: 11 },
  buttonRow: { flexDirection: "row", gap: 8, marginVertical: 14 },
  secondaryButton: { flex: 1, minHeight: 46, borderWidth: 1, borderColor: palette.green, borderRadius: 8, alignItems: "center", justifyContent: "center" },
  secondaryButtonText: { color: palette.green, fontWeight: "800", fontSize: 12 },
  primarySmall: { flex: 1, minHeight: 46, backgroundColor: palette.green, borderRadius: 8, alignItems: "center", justifyContent: "center" },
  primarySmallText: { color: palette.white, fontWeight: "800", fontSize: 12 },
  diagnosticRow: { minHeight: 74, borderTopWidth: 1, borderTopColor: palette.rule, flexDirection: "row", alignItems: "center", gap: 12 },
  diagnosticNumber: { color: palette.green, fontFamily: Platform.select({ ios: "Menlo", default: "monospace" }), fontSize: 11 },
  diagnosticCopy: { flex: 1, gap: 3 },
  diagnosticLabel: { color: palette.ink, fontWeight: "800", fontSize: 13 },
  diagnosticDetail: { color: palette.muted, fontSize: 10, lineHeight: 14 },
  diagnosticReady: { color: palette.green, fontSize: 9, fontWeight: "900", letterSpacing: 1 },
  diagnosticError: { color: palette.red, fontSize: 9, fontWeight: "900", letterSpacing: 1 },
  signOutButton: { marginTop: 24, minHeight: 48, borderTopWidth: 1, borderBottomWidth: 1, borderColor: palette.rule, alignItems: "center", justifyContent: "center" },
  signOutText: { color: palette.red, fontSize: 12, fontWeight: "800" },
  tabBar: {
    position: "absolute",
    left: 12,
    right: 12,
    bottom: 10,
    minHeight: 66,
    borderRadius: 15,
    backgroundColor: palette.ink,
    padding: 6,
    flexDirection: "row",
    ...(Platform.OS === "web"
      ? { boxShadow: "0 6px 14px rgba(0, 0, 0, 0.18)" }
      : { shadowColor: "#000", shadowOpacity: 0.18, shadowRadius: 14, shadowOffset: { width: 0, height: 6 }, elevation: 9 }),
  },
  tabButton: { flex: 1, borderRadius: 11, alignItems: "center", justifyContent: "center", gap: 2 },
  tabButtonActive: { backgroundColor: "#343A32" },
  tabGlyph: { color: "#90978D", fontSize: 18 },
  tabLabel: { color: "#90978D", fontSize: 9, fontWeight: "800", letterSpacing: 0.5 },
  tabTextActive: { color: palette.white },
  authBackground: { flex: 1, backgroundColor: "#13261A" },
  authSafeArea: { flex: 1, paddingHorizontal: 20, justifyContent: "center" },
  authHero: { paddingVertical: 28, gap: 10 },
  authMonogram: { width: 52, height: 52, borderBottomWidth: 3, borderBottomColor: "#8DB89A", alignItems: "center", justifyContent: "center", marginBottom: 8 },
  authMonogramText: { fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), fontSize: 42, lineHeight: 48, color: palette.white, fontWeight: "700", fontStyle: "italic" },
  authEyebrow: { color: "#90B49B", letterSpacing: 1.8, fontSize: 9, fontWeight: "900" },
  authTitle: { color: palette.white, fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), fontSize: 36, lineHeight: 40, fontWeight: "700" },
  authLead: { color: "#B4C3B7", fontSize: 13, lineHeight: 19, maxWidth: 380 },
  authCard: { backgroundColor: palette.paper, borderRadius: 14, padding: 18, gap: 8 },
  authModes: { flexDirection: "row", backgroundColor: "#EAE7DE", padding: 3, borderRadius: 8, marginBottom: 8 },
  authMode: { flex: 1, minHeight: 36, borderRadius: 6, alignItems: "center", justifyContent: "center" },
  authModeActive: { backgroundColor: palette.paper },
  authModeText: { color: palette.muted, fontSize: 11, fontWeight: "800" },
  authModeTextActive: { color: palette.green },
  authNotice: { color: palette.green, backgroundColor: palette.greenSoft, padding: 10, borderRadius: 7, fontSize: 11, lineHeight: 16, marginBottom: 4 },
  inputLabel: { fontSize: 9, letterSpacing: 1.3, color: palette.muted, fontWeight: "900", marginTop: 4 },
  input: { minHeight: 48, borderWidth: 1, borderColor: palette.rule, borderRadius: 7, paddingHorizontal: 13, color: palette.ink, fontSize: 14, backgroundColor: palette.white },
  primaryButton: { minHeight: 50, borderRadius: 8, backgroundColor: palette.green, alignItems: "center", justifyContent: "center", marginTop: 8 },
  primaryButtonText: { color: palette.white, fontWeight: "900", fontSize: 13 },
  authFootnote: { color: palette.muted, fontSize: 9, textAlign: "center", marginTop: 4 },
  loading: { flex: 1, backgroundColor: "#13261A", alignItems: "center", justifyContent: "center", gap: 18 },
  loadingText: { color: "#B4C3B7", fontSize: 12 },
  configuration: { flex: 1, backgroundColor: palette.cream, padding: 28, justifyContent: "center", gap: 12 },
  configEyebrow: { color: palette.red, fontSize: 10, letterSpacing: 1.5, fontWeight: "900" },
  configTitle: { color: palette.ink, fontFamily: Platform.select({ ios: "Georgia", default: "serif" }), fontSize: 32, lineHeight: 37, fontWeight: "700" },
  configText: { color: palette.muted, fontSize: 13, lineHeight: 20 },
  configCode: { color: palette.green, backgroundColor: palette.greenSoft, borderRadius: 7, padding: 12, fontFamily: Platform.select({ ios: "Menlo", default: "monospace" }), fontSize: 10 },
});

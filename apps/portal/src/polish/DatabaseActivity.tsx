import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import {
  acceptCompletion,
  autocompletion,
  completionStatus,
} from "@codemirror/autocomplete";
import { indentWithTab } from "@codemirror/commands";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { sql as sqlLanguage } from "@codemirror/lang-sql";
import { Prec } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { tags } from "@lezer/highlight";
import CodeMirror from "@uiw/react-codemirror";
import { format } from "sql-formatter";
import {
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  Check,
  ChevronLeft,
  ChevronRight,
  Clock3,
  Copy,
  Database,
  FileClock,
  FilterX,
  Play,
  RefreshCw,
  RotateCcw,
  Save,
  Search,
  Table2,
  TerminalSquare,
  Trash2,
  Undo2,
  WandSparkles,
  X,
} from "lucide-react";
import {
  Group as ResizablePanelGroup,
  Panel as ResizablePanel,
  Separator as ResizableHandle,
} from "react-resizable-panels";
import {
  FFDBError,
  type AuditLogEntry,
  type FFDBClient,
  type MigrationSpec,
  type MigrationSummary,
  type QueryResult,
  type ResultCell,
  type SchemaSnapshot,
  type SqlParameter,
  type TableDefinition,
} from "@ffdb/client";

import "./database-activity.css";

const CODEMIRROR_STYLE_NONCE = "ffdb-codemirror";

type Loadable<T> =
  | { readonly status: "loading" }
  | { readonly status: "error"; readonly error: string }
  | { readonly status: "ready"; readonly data: T };

type QueryRun = {
  readonly id: number;
  readonly sql: string;
  readonly startedAt: number;
  readonly durationMs: number;
  readonly statements: readonly string[];
  readonly results: readonly QueryResult[] | null;
  readonly error: string | null;
};

export interface SqlEditorPanelProps {
  readonly client: FFDBClient;
  readonly initialSql?: string;
  readonly sql?: string;
  readonly onSqlChange?: (value: string) => void;
}

export function SqlEditorPanel({ client, initialSql = "SELECT sqlite_version() AS version;", sql, onSqlChange }: SqlEditorPanelProps) {
  const [draft, setDraftState] = useState(() => sql ?? initialSql);
  const [schema, setSchema] = useState<Loadable<SchemaSnapshot>>({ status: "loading" });
  const [runs, setRuns] = useState<readonly QueryRun[]>([]);
  const [running, setRunning] = useState(false);
  const [selectedRunId, setSelectedRunId] = useState<number | null>(null);
  const [bottomTab, setBottomTab] = useState<"results" | "history">("results");
  const selectedRun = runs.find((run) => run.id === selectedRunId) ?? runs[0] ?? null;
  const completionTables = useMemo<Record<string, readonly string[]>>(() => schema.status === "ready" ? Object.fromEntries(schema.data.tables.map((table) => [table.name, []])) : {}, [schema]);

  const setDraft = useCallback((next: string) => {
    setDraftState(next);
    onSqlChange?.(next);
  }, [onSqlChange]);

  useEffect(() => {
    if (sql === undefined) return;
    setDraftState((current) => current === sql ? current : sql);
  }, [sql]);

  const loadSchema = useCallback(async () => {
    setSchema({ status: "loading" });
    try {
      setSchema({ status: "ready", data: await client.schema() });
    } catch (cause) {
      setSchema({ status: "error", error: errorMessage(cause) });
    }
  }, [client]);

  useEffect(() => { void loadSchema(); }, [loadSchema]);

  const execute = useCallback(async () => {
    const batchSql = draft.trim();
    if (batchSql === "" || running) return;
    setRunning(true);
    const startedAt = Date.now();
    const id = startedAt + Math.random();
    let statements: readonly string[] = [];
    try {
      statements = splitSqlStatements(batchSql);
      if (statements.length === 0) throw new Error("Enter at least one executable SQL statement.");
      const results = statements.length === 1
        ? [await client.query({ sql: statements[0]!, options: { max_rows: 500 } })]
        : await client.transaction({ statements: statements.map((statement) => ({ sql: statement, options: { max_rows: 500 } })) });
      const next: QueryRun = { id, sql: batchSql, statements, startedAt, durationMs: Date.now() - startedAt, results, error: null };
      setRuns((current) => [next, ...current].slice(0, 20));
      setSelectedRunId(id);
      setBottomTab("results");
      if (statements.some(mightChangeSchema)) void loadSchema();
    } catch (cause) {
      const next: QueryRun = { id, sql: batchSql, statements, startedAt, durationMs: Date.now() - startedAt, results: null, error: errorMessage(cause) };
      setRuns((current) => [next, ...current].slice(0, 20));
      setSelectedRunId(id);
      setBottomTab("results");
    } finally {
      setRunning(false);
    }
  }, [client, draft, loadSchema, running]);

  const insertTable = (table: string) => setDraft(`SELECT *\nFROM ${quoteIdentifier(table)}\nLIMIT 100;`);

  return (
    <div className="ffdb-data-page ffdb-query-workbench">
      <section className="ffdb-surface ffdb-workbench-shell ffdb-sql-studio" aria-label="SQL query editor">
        <header className="ffdb-surface-header ffdb-editor-toolbar">
          <div className="ffdb-editor-title">
            <TerminalSquare size={15} />
            <strong>SQL editor</strong>
            <span>Run one statement or a semicolon-delimited batch</span>
          </div>
          <div className="ffdb-toolbar-actions">
            <button className="ffdb-button ffdb-button-secondary" type="button" onClick={() => setDraft(formatSql(draft))}>
              <WandSparkles size={15} /> Format
            </button>
            <button className="ffdb-button ffdb-button-primary" type="button" disabled={running || draft.trim() === ""} onClick={() => void execute()}>
              {running ? <RefreshCw className="ffdb-spin" size={15} /> : <Play size={15} fill="currentColor" />}
              {running ? "Running…" : "Run query"}
              <kbd>⌘↵</kbd>
            </button>
          </div>
        </header>

        <div className="ffdb-studio-grid">
          <aside className="ffdb-schema-browser" aria-label="Database schema">
            <div className="ffdb-aside-heading">
              <span><Database size={15} /> Schema</span>
              <button type="button" aria-label="Refresh schema" title="Refresh schema" onClick={() => void loadSchema()}><RefreshCw size={14} /></button>
            </div>
            {schema.status === "loading" ? <InlineLoading label="Loading tables" /> : schema.status === "error" ? <InlineError message={schema.error} /> : (
              <div className="ffdb-schema-list">
                <small>Version {schema.data.version} · {schema.data.tables.length} {schema.data.tables.length === 1 ? "table" : "tables"}</small>
                {schema.data.tables.length === 0 ? <p className="ffdb-quiet">No application tables yet.</p> : schema.data.tables.map((table) => (
                  <button key={table.name} type="button" onClick={() => insertTable(table.name)} title={`Query ${table.name}`}>
                    <Table2 size={14} /><span>{table.name}</span>{table.rls_enabled ? <em>RLS</em> : null}
                  </button>
                ))}
              </div>
            )}
          </aside>
          <div className="ffdb-studio-workspace">
            <ResizablePanelGroup className="ffdb-resizable-group" orientation="vertical">
              <ResizablePanel defaultSize={58} minSize={25}>
                <div className="ffdb-editor-stage">
                  <CodeEditor value={draft} onChange={setDraft} onRun={() => void execute()} ariaLabel="SQL query" minLines={13} height="100%" tables={completionTables} />
                  <div className="ffdb-editor-statusbar">
                    <span>SQLite</span><span>Spaces: 2</span><span>UTF-8</span><span>Cmd/Ctrl + Enter</span>
                  </div>
                </div>
              </ResizablePanel>

              <ResizableHandle className="ffdb-resize-handle" aria-label="Resize query and results panels">
                <span />
              </ResizableHandle>

              <ResizablePanel defaultSize={42} minSize={20}>
                <section className="ffdb-studio-output" aria-live="polite">
                  <div className="ffdb-output-tabs" role="tablist" aria-label="Query output">
                    <button type="button" role="tab" aria-selected={bottomTab === "results"} className={bottomTab === "results" ? "is-active" : ""} onClick={() => setBottomTab("results")}>Results</button>
                    <button type="button" role="tab" aria-selected={bottomTab === "history"} className={bottomTab === "history" ? "is-active" : ""} onClick={() => setBottomTab("history")}>History <span>{runs.length}</span></button>
                    {selectedRun === null ? null : <QueryRunMeta run={selectedRun} />}
                  </div>
                  <div className="ffdb-output-content">
                    {bottomTab === "results" ? (
                      selectedRun === null ? <EmptyState icon={<Play size={20} />} title="Ready to run" detail="Run one statement or a semicolon-delimited batch to inspect every result." /> : selectedRun.error !== null ? <InlineError message={selectedRun.error} /> : selectedRun.results === null ? null : <QueryResultSet key={selectedRun.id} statements={selectedRun.statements} results={selectedRun.results} />
                    ) : runs.length === 0 ? <p className="ffdb-quiet ffdb-panel-padding">Your 20 most recent runs will appear here.</p> : (
                      <ol className="ffdb-run-history">
                        {runs.map((run) => (
                          <li key={run.id}>
                            <button type="button" className={selectedRun?.id === run.id ? "is-selected" : ""} onClick={() => { setSelectedRunId(run.id); setBottomTab("results"); }}>
                              <span className={run.error === null ? "ffdb-run-dot is-success" : "ffdb-run-dot is-error"}>{run.error === null ? <Check size={10} /> : <X size={10} />}</span>
                              <span><strong>{singleLine(run.sql)}</strong><small>{new Date(run.startedAt).toLocaleTimeString()} · {run.durationMs} ms</small></span>
                            </button>
                            <button className="ffdb-history-reuse" type="button" title="Open in editor" aria-label={`Open ${singleLine(run.sql)} in editor`} onClick={() => setDraft(run.sql)}><RotateCcw size={13} /></button>
                          </li>
                        ))}
                      </ol>
                    )}
                  </div>
                </section>
              </ResizablePanel>
            </ResizablePanelGroup>
          </div>
        </div>
      </section>
    </div>
  );
}

export interface DatabasePanelProps {
  readonly client: FFDBClient;
  readonly onOpenMigrations?: () => void;
}

export function DatabasePanel({ client, onOpenMigrations }: DatabasePanelProps) {
  const [resource, setResource] = useState<Loadable<{ readonly schema: SchemaSnapshot; readonly migrations: readonly MigrationSummary[] }>>({ status: "loading" });
  const [search, setSearch] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [rows, setRows] = useState<Loadable<QueryResult> | null>(null);
  const [view, setView] = useState<"data" | "structure">("data");
  const [gridActionTarget, setGridActionTarget] = useState<HTMLDivElement | null>(null);

  const loadRows = useCallback(async (table: string) => {
    setRows({ status: "loading" });
    try {
      setRows({ status: "ready", data: await client.query({ sql: `SELECT * FROM ${quoteIdentifier(table)} LIMIT 500;`, options: { max_rows: 500 } }) });
    } catch (cause) {
      setRows({ status: "error", error: errorMessage(cause) });
    }
  }, [client]);

  const refresh = useCallback(async () => {
    setResource({ status: "loading" });
    setRows(null);
    try {
      const [schema, migrations] = await Promise.all([client.schema(), client.migrationHistory()]);
      setResource({ status: "ready", data: { schema, migrations } });
      setSelected((current) => current !== null && schema.tables.some((table) => table.name === current) ? current : schema.tables[0]?.name ?? null);
    } catch (cause) {
      setResource({ status: "error", error: errorMessage(cause) });
    }
  }, [client]);

  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => {
    if (resource.status === "ready" && selected !== null) void loadRows(selected);
  }, [loadRows, resource, selected]);

  const browse = useCallback((table: string) => {
    setSelected(table);
    setView("data");
  }, []);

  if (resource.status === "loading") return <PageLoading label="Loading database schema" />;
  if (resource.status === "error") return <PageError title="Database unavailable" message={resource.error} onRetry={() => void refresh()} />;

  const tables = resource.data.schema.tables.filter((table) => table.name.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase()));
  const selectedTable = resource.data.schema.tables.find((table) => table.name === selected) ?? null;
  const protectedTables = resource.data.schema.tables.filter((table) => table.rls_enabled).length;

  return (
    <div className="ffdb-data-page ffdb-database-page">
      <section className="ffdb-surface ffdb-database-workbench">
        <header className="ffdb-database-toolbar">
          <div className="ffdb-database-title"><Database size={16} /><span><strong>Data explorer</strong><small>Schema v{resource.data.schema.version} · {resource.data.schema.tables.length} tables · {protectedTables} RLS protected</small></span></div>
          <div className="ffdb-toolbar-actions">
            <button className="ffdb-button ffdb-button-secondary" type="button" onClick={() => void refresh()}><RefreshCw size={15} /> Refresh schema</button>
            {onOpenMigrations === undefined ? null : <button className="ffdb-button ffdb-button-primary" type="button" onClick={onOpenMigrations}><FileClock size={15} /> Migrations</button>}
          </div>
        </header>
        <div className="ffdb-database-layout">
        <aside className="ffdb-table-directory">
          <label className="ffdb-search-field"><Search size={15} /><span className="ffdb-sr-only">Search tables</span><input value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Find a table…" /></label>
          <span className="ffdb-directory-label">Tables</span>
          <div className="ffdb-table-list">
            {tables.length === 0 ? <p className="ffdb-quiet">No tables match “{search}”.</p> : tables.map((table) => (
              <button key={table.name} className={selected === table.name ? "is-selected" : ""} type="button" onClick={() => browse(table.name)}>
                <Table2 size={15} /><span><strong>{table.name}</strong><small>{table.rls_enabled ? "Row security enabled" : "Row security disabled"}</small></span><ChevronRight size={14} />
              </button>
            ))}
          </div>
        </aside>
        <div className="ffdb-table-workspace">
          {selectedTable === null ? <EmptyState icon={<Table2 size={20} />} title="Select a table" detail="Choose a table to inspect its definition and preview rows." /> : (
            <>
              <header className="ffdb-table-toolbar">
                <div className="ffdb-selected-table"><Table2 size={15} /><span><strong>{selectedTable.name}</strong><small>{rows?.status === "ready" ? `${rows.data.rows.length}${rows.data.truncated ? "+" : ""} rows loaded` : "SQLite table"}</small></span></div>
                <nav className="ffdb-table-tabs" aria-label={`${selectedTable.name} views`}>
                  <button type="button" aria-current={view === "data" ? "page" : undefined} onClick={() => setView("data")}><Table2 size={13} /> Data</button>
                  <button type="button" aria-current={view === "structure" ? "page" : undefined} onClick={() => setView("structure")}><Database size={13} /> Structure</button>
                </nav>
                <div className="ffdb-badge-row"><StatusBadge tone={selectedTable.rls_enabled ? "success" : "warning"}>{selectedTable.rls_enabled ? "RLS enabled" : "RLS disabled"}</StatusBadge>{selectedTable.rls_forced ? <StatusBadge tone="success">RLS forced</StatusBadge> : null}</div>
              </header>
              {view === "structure" ? <div className="ffdb-structure-view"><div className="ffdb-definition-block"><div><span>CREATE statement</span><CopyButton value={selectedTable.sql} label="Copy table definition" /></div><pre><code>{highlightSql(selectedTable.sql)}</code></pre></div><p>Schema changes are versioned through Migrations. The explorer keeps this definition read-only.</p></div> : (
                <div className="ffdb-data-view">
                  <div className="ffdb-data-toolbar"><span>Live rows · reads at most 500</span><div className="ffdb-data-toolbar-actions"><div className="ffdb-grid-toolbar-slot" ref={setGridActionTarget} /><button className="ffdb-button ffdb-button-secondary" type="button" onClick={() => void loadRows(selectedTable.name)}><RefreshCw size={14} /> Reload data</button></div></div>
                  {rows === null || rows.status === "loading" ? <InlineLoading label="Reading table rows" /> : rows.status === "error" ? <InlineError message={rows.error} /> : <EditableTableGrid key={`${selectedTable.name}-${rows.data.columns.map((column) => column.name).join(":")}`} actionTarget={gridActionTarget} client={client} result={rows.data} table={selectedTable} onReload={() => loadRows(selectedTable.name)} />}
                </div>
              )}
            </>
          )}
        </div>
        </div>
      </section>
    </div>
  );
}

export interface MigrationsPanelProps { readonly client: FFDBClient }

export function MigrationsPanel({ client }: MigrationsPanelProps) {
  const [history, setHistory] = useState<Loadable<readonly MigrationSummary[]>>({ status: "loading" });
  const [schemaVersion, setSchemaVersion] = useState<number | null>(null);
  const [name, setName] = useState("");
  const [id, setId] = useState(() => migrationId());
  const [upSql, setUpSql] = useState("CREATE TABLE example (\n  id TEXT PRIMARY KEY,\n  created_at INTEGER NOT NULL\n);");
  const [downSql, setDownSql] = useState("DROP TABLE example;");
  const [checksum, setChecksum] = useState("");
  const [phase, setPhase] = useState<"edit" | "review">("edit");
  const [submitting, setSubmitting] = useState(false);
  const [message, setMessage] = useState<{ readonly tone: "success" | "error"; readonly text: string } | null>(null);
  const [rollbackId, setRollbackId] = useState<string | null>(null);
  const [historySearch, setHistorySearch] = useState("");
  const [activeView, setActiveView] = useState<"new" | "history">("new");
  const historyTableRef = useRef<HTMLDivElement>(null);
  const [historyTableScroll, setHistoryTableScroll] = useState({ left: false, right: false });

  const updateHistoryTableScroll = useCallback(() => {
    const table = historyTableRef.current;
    if (table === null) return;
    const maximum = Math.max(0, table.scrollWidth - table.clientWidth);
    const next = { left: table.scrollLeft > 1, right: table.scrollLeft < maximum - 1 };
    setHistoryTableScroll((current) => current.left === next.left && current.right === next.right ? current : next);
  }, []);

  const scrollHistoryTable = (direction: -1 | 1) => {
    const table = historyTableRef.current;
    if (table === null) return;
    table.scrollBy({ behavior: "smooth", left: direction * Math.max(320, table.clientWidth * 0.75) });
  };

  const refresh = useCallback(async () => {
    setHistory({ status: "loading" });
    try {
      const [migrations, schema] = await Promise.all([client.migrationHistory(), client.schema()]);
      setHistory({ status: "ready", data: migrations });
      setSchemaVersion(schema.version);
    } catch (cause) {
      setHistory({ status: "error", error: errorMessage(cause) });
    }
  }, [client]);
  useEffect(() => { void refresh(); }, [refresh]);

  useEffect(() => {
    let active = true;
    const cleanName = name.trim();
    if (cleanName === "" || upSql.trim() === "" || downSql.trim() === "") { setChecksum(""); return undefined; }
    void migrationChecksum(id.trim(), cleanName, upSql.trim(), downSql.trim()).then((value) => { if (active) setChecksum(value); });
    return () => { active = false; };
  }, [downSql, id, name, upSql]);

  const review = (event: FormEvent) => {
    event.preventDefault();
    setMessage(null);
    const validation = validateMigration(id, name, upSql, downSql);
    if (validation !== null) { setMessage({ tone: "error", text: validation }); return; }
    setPhase("review");
  };

  const apply = async () => {
    if (checksum === "" || submitting) return;
    setSubmitting(true); setMessage(null);
    const spec: MigrationSpec = { id: id.trim(), name: name.trim(), up_sql: upSql.trim(), down_sql: downSql.trim(), checksum, created_at_ms: Date.now() };
    try {
      await client.migrate(spec, { idempotencyKey: `migration:${spec.id}:${spec.checksum}` });
      setMessage({ tone: "success", text: `Migration ${spec.id} applied successfully. Replaying this exact migration is idempotent.` });
      setPhase("edit");
      await refresh();
    } catch (cause) {
      setMessage({ tone: "error", text: errorMessage(cause) });
    } finally {
      setSubmitting(false);
    }
  };

  const rollback = async (migration: MigrationSummary) => {
    if (submitting) return;
    setSubmitting(true); setMessage(null);
    try {
      await client.rollbackMigration(migration.id, { idempotencyKey: `migration-rollback:${migration.id}:${Date.now()}` });
      setMessage({ tone: "success", text: `Migration ${migration.id} rolled back using its stored down SQL.` });
      setRollbackId(null);
      await refresh();
    } catch (cause) {
      setMessage({ tone: "error", text: errorMessage(cause) });
    } finally { setSubmitting(false); }
  };

  const startAnother = () => {
    setName(""); setId(migrationId()); setUpSql(""); setDownSql(""); setChecksum(""); setPhase("edit"); setMessage(null);
  };

  const visibleHistory = history.status === "ready" ? history.data.filter((migration) => `${migration.id} ${migration.name} ${migration.status}`.toLocaleLowerCase().includes(historySearch.trim().toLocaleLowerCase())) : [];

  useEffect(() => {
    const table = historyTableRef.current;
    if (activeView !== "history" || table === null || visibleHistory.length === 0) return;
    updateHistoryTableScroll();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(updateHistoryTableScroll);
    observer.observe(table);
    const content = table.firstElementChild;
    if (content !== null) observer.observe(content);
    return () => observer.disconnect();
  }, [activeView, historySearch, updateHistoryTableScroll, visibleHistory.length]);

  return (
    <div className="ffdb-data-page ffdb-migrations-page">
      <section className="ffdb-migration-workbench">
        <div className="ffdb-migration-tabs" role="tablist" aria-label="Migration workspace">
          <button id="migration-new-tab" type="button" role="tab" aria-selected={activeView === "new"} aria-controls="migration-new-panel" onClick={() => setActiveView("new")}><ArrowUp size={14} /> New migration</button>
          <button id="migration-history-tab" type="button" role="tab" aria-selected={activeView === "history"} aria-controls="migration-history-panel" onClick={() => setActiveView("history")}><FileClock size={14} /> History {history.status === "ready" ? <span>{history.data.length}</span> : null}</button>
          <div className="ffdb-migration-schema"><span>Schema</span><strong>{schemaVersion === null ? "—" : `v${schemaVersion}`}</strong></div>
        </div>
        {activeView === "new" ? <div id="migration-new-panel" className="ffdb-migration-panel" role="tabpanel" aria-labelledby="migration-new-tab">
        <header className="ffdb-migration-context">
          <p>Create an atomic, reversible, checksummed schema change. Use the SQL editor for ad-hoc queries.</p>
        </header>
        {message === null ? null : <Notice tone={message.tone}>{message.text}</Notice>}
        {phase === "review" ? (
          <MigrationReview id={id.trim()} name={name.trim()} upSql={upSql.trim()} downSql={downSql.trim()} checksum={checksum} submitting={submitting} onBack={() => setPhase("edit")} onApply={() => void apply()} />
        ) : (
          <form className="ffdb-migration-form" onSubmit={review}>
            <div className="ffdb-migration-meta">
              <label><span>Migration name</span><small>A short human-readable description</small><input required maxLength={256} value={name} onChange={(event) => setName(event.target.value)} placeholder="Create customer profiles" /></label>
              <label><span>Migration ID</span><small>Stable identifier; never reuse it for different SQL</small><input required maxLength={128} value={id} onChange={(event) => setId(event.target.value.replace(/\s+/gu, "-"))} /></label>
            </div>
            <div className="ffdb-migration-editors">
              <div><div className="ffdb-direction-heading"><span className="is-up"><ArrowUp size={14} /> Up SQL</span><small>Applied atomically</small></div><CodeEditor ariaLabel="Migration up SQL" value={upSql} onChange={setUpSql} minLines={11} height="100%" /></div>
              <div><div className="ffdb-direction-heading"><span className="is-down"><ArrowDown size={14} /> Down SQL</span><small>Used by rollback</small></div><CodeEditor ariaLabel="Migration down SQL" value={downSql} onChange={setDownSql} minLines={11} height="100%" /></div>
            </div>
            <footer className="ffdb-migration-footer">
              <div className="ffdb-checksum-preview"><span>SHA-256 checksum</span><code>{checksum === "" ? "Complete all fields to calculate" : checksum}</code></div>
              <button className="ffdb-button ffdb-button-primary" type="submit">Review migration <ChevronRight size={15} /></button>
            </footer>
          </form>
        )}
      </div> : null}

      {activeView === "history" ? <div id="migration-history-panel" className="ffdb-migration-panel ffdb-migration-history" role="tabpanel" aria-labelledby="migration-history-tab">
        <header className="ffdb-migration-history-toolbar">
          <p>Applied and rolled-back schema changes for this project.</p>
          <div className="ffdb-toolbar-actions"><label className="ffdb-search-field ffdb-search-compact"><Search size={14} /><span className="ffdb-sr-only">Search migration history</span><input value={historySearch} onChange={(event) => setHistorySearch(event.target.value)} placeholder="Search history…" /></label><button type="button" className="ffdb-icon-button" aria-label="Refresh migration history" onClick={() => void refresh()}><RefreshCw size={15} /></button></div>
        </header>
        {history.status === "loading" ? <InlineLoading label="Loading migration history" /> : history.status === "error" ? <InlineError message={history.error} /> : visibleHistory.length === 0 ? <EmptyState icon={<FileClock size={20} />} title={history.data.length === 0 ? "No migrations yet" : "No matching migrations"} detail={history.data.length === 0 ? "Your first successful migration will establish schema history here." : "Adjust the search to find another migration."} /> : (
          <>
          {historyTableScroll.left || historyTableScroll.right ? <div className="ffdb-table-scroll-tools" role="group" aria-label="Scroll migration history table">
            <span>Scroll columns</span>
            <button type="button" aria-label="Scroll migration history left" disabled={!historyTableScroll.left} onClick={() => scrollHistoryTable(-1)}><ChevronLeft size={15} /></button>
            <button type="button" aria-label="Scroll migration history right" disabled={!historyTableScroll.right} onClick={() => scrollHistoryTable(1)}><ChevronRight size={15} /></button>
          </div> : null}
          <div className="ffdb-table-wrap ffdb-migration-table-wrap" role="region" aria-label="Migration history records" tabIndex={0} ref={historyTableRef} onScroll={updateHistoryTableScroll}><table className="ffdb-data-table ffdb-migration-table"><thead><tr><th>Migration</th><th>Status</th><th>Schema</th><th>Applied</th><th>Checksum</th><th><span className="ffdb-sr-only">Actions</span></th></tr></thead><tbody>{visibleHistory.map((migration) => (
            <tr key={migration.id}>
              <td data-label="Migration"><strong>{migration.name}</strong><small>{migration.id}</small></td>
              <td data-label="Status"><StatusBadge tone={migration.status.toLocaleLowerCase() === "applied" ? "success" : "neutral"}>{sentenceCase(migration.status)}</StatusBadge></td>
              <td data-label="Schema">v{migration.schema_version_before} → v{migration.schema_version_after}</td>
              <td data-label="Applied">{migration.applied_at_ms === null ? "—" : formatDateTime(migration.applied_at_ms)}</td>
              <td data-label="Checksum"><code title={migration.checksum}>{migration.checksum.slice(0, 10)}…</code></td>
              <td className="ffdb-row-action">{rollbackId === migration.id ? <span className="ffdb-inline-confirm"><strong>Roll back?</strong><button type="button" disabled={submitting} onClick={() => void rollback(migration)}>Confirm</button><button type="button" onClick={() => setRollbackId(null)}>Cancel</button></span> : <button className="ffdb-button ffdb-button-danger-quiet" type="button" disabled={migration.status.toLocaleLowerCase() !== "applied"} onClick={() => setRollbackId(migration.id)}><RotateCcw size={14} /> Roll back</button>}</td>
            </tr>
          ))}</tbody></table></div>
          </>
        )}
        {message?.tone === "success" ? <div className="ffdb-section-footer"><button className="ffdb-button ffdb-button-secondary" type="button" onClick={startAnother}>Start another migration</button></div> : null}
      </div> : null}
      </section>
    </div>
  );
}

type ActivitySortKey = "occurred_at_ms" | "actor" | "action" | "resource" | "outcome";
type SortDirection = "asc" | "desc";

export interface ActivityPanelProps { readonly client: FFDBClient }

export function ActivityPanel({ client }: ActivityPanelProps) {
  const [resource, setResource] = useState<Loadable<readonly AuditLogEntry[]>>({ status: "loading" });
  const [search, setSearch] = useState("");
  const [action, setAction] = useState("all");
  const [outcome, setOutcome] = useState("all");
  const [sortKey, setSortKey] = useState<ActivitySortKey>("occurred_at_ms");
  const [direction, setDirection] = useState<SortDirection>("desc");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(25);
  const [selected, setSelected] = useState<AuditLogEntry | null>(null);
  const activityTableRef = useRef<HTMLDivElement>(null);
  const [tableScroll, setTableScroll] = useState({ left: false, right: false });

  const refresh = useCallback(async () => {
    setResource({ status: "loading" });
    try { setResource({ status: "ready", data: await client.logs({ limit: 500 }) }); }
    catch (cause) { setResource({ status: "error", error: errorMessage(cause) }); }
  }, [client]);
  const updateTableScroll = useCallback(() => {
    const table = activityTableRef.current;
    if (table === null) return;
    const maximum = Math.max(0, table.scrollWidth - table.clientWidth);
    const next = { left: table.scrollLeft > 1, right: table.scrollLeft < maximum - 1 };
    setTableScroll((current) => current.left === next.left && current.right === next.right ? current : next);
  }, []);
  const scrollTable = (direction: -1 | 1) => {
    const table = activityTableRef.current;
    if (table === null) return;
    table.scrollBy({ behavior: "smooth", left: direction * Math.max(320, table.clientWidth * 0.75) });
  };
  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => { setPage(1); }, [action, outcome, pageSize, search, sortKey, direction]);
  useEffect(() => {
    const table = activityTableRef.current;
    if (table === null || resource.status !== "ready" || resource.data.length === 0) return;
    updateTableScroll();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(updateTableScroll);
    observer.observe(table);
    const content = table.firstElementChild;
    if (content !== null) observer.observe(content);
    return () => observer.disconnect();
  }, [action, outcome, pageSize, resource, search, updateTableScroll]);

  if (resource.status === "loading") return <PageLoading label="Loading audit activity" />;
  if (resource.status === "error") return <PageError title="Activity unavailable" message={resource.error} onRetry={() => void refresh()} />;

  const actionOptions = [...new Set(resource.data.map((entry) => entry.action))].sort((left, right) => left.localeCompare(right));
  const normalizedSearch = search.trim().toLocaleLowerCase();
  const filtered = resource.data.filter((entry) => {
    if (action !== "all" && entry.action !== action) return false;
    if (outcome !== "all" && entry.outcome !== outcome) return false;
    return normalizedSearch === "" || `${entry.actor} ${entry.action} ${entry.resource} ${entry.request_id ?? ""}`.toLocaleLowerCase().includes(normalizedSearch);
  }).sort((left, right) => compareActivity(left, right, sortKey, direction));
  const pageCount = Math.max(1, Math.ceil(filtered.length / pageSize));
  const safePage = Math.min(page, pageCount);
  const visible = filtered.slice((safePage - 1) * pageSize, safePage * pageSize);
  const filtersActive = search !== "" || action !== "all" || outcome !== "all";
  const clear = () => { setSearch(""); setAction("all"); setOutcome("all"); };
  const changeSort = (key: ActivitySortKey) => { if (sortKey === key) setDirection((current) => current === "asc" ? "desc" : "asc"); else { setSortKey(key); setDirection(key === "occurred_at_ms" ? "desc" : "asc"); } };

  return (
    <div className="ffdb-data-page">
      <section className="ffdb-surface ffdb-activity-surface">
        <header className="ffdb-surface-header">
          <div><span className="ffdb-eyebrow"><FileClock size={14} /> Security and operations</span><h2>Activity log</h2><p>Search and inspect the 500 most recent project events.</p></div>
          <button className="ffdb-button ffdb-button-secondary" type="button" onClick={() => void refresh()}><RefreshCw size={15} /> Refresh</button>
        </header>
        <div className="ffdb-filter-bar" role="search" aria-label="Filter activity">
          <label className="ffdb-search-field"><Search size={15} /><span className="ffdb-sr-only">Search activity</span><input value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search actor, action, resource, request ID…" /></label>
          <label><span className="ffdb-sr-only">Filter by action</span><select value={action} onChange={(event) => setAction(event.target.value)}><option value="all">All actions</option>{actionOptions.map((value) => <option value={value} key={value}>{friendlyLabel(value)}</option>)}</select></label>
          <label><span className="ffdb-sr-only">Filter by outcome</span><select value={outcome} onChange={(event) => setOutcome(event.target.value)}><option value="all">All outcomes</option><option value="success">Success</option><option value="denied">Denied</option><option value="failed">Failed</option></select></label>
          <button className="ffdb-button ffdb-button-quiet" type="button" disabled={!filtersActive} onClick={clear}><FilterX size={15} /> Reset</button>
        </div>

        {filtered.length === 0 ? <EmptyState icon={<Search size={20} />} title={resource.data.length === 0 ? "No activity yet" : "No events match"} detail={resource.data.length === 0 ? "Security and lifecycle events will appear here as your project is used." : "Clear or adjust the filters to broaden the results."} action={filtersActive ? <button className="ffdb-button ffdb-button-secondary" type="button" onClick={clear}>Clear filters</button> : undefined} /> : (
          <>
            {tableScroll.left || tableScroll.right ? <div className="ffdb-table-scroll-tools" role="group" aria-label="Scroll activity table">
              <span>Scroll columns</span>
              <button type="button" aria-label="Scroll activity table left" disabled={!tableScroll.left} onClick={() => scrollTable(-1)}><ChevronLeft size={15} /></button>
              <button type="button" aria-label="Scroll activity table right" disabled={!tableScroll.right} onClick={() => scrollTable(1)}><ChevronRight size={15} /></button>
            </div> : null}
            <div
              aria-label="Activity records"
              className="ffdb-table-wrap ffdb-activity-table-wrap"
              onScroll={updateTableScroll}
              ref={activityTableRef}
              role="region"
              tabIndex={0}
            >
              <table className="ffdb-data-table ffdb-activity-table"><thead><tr>
                <SortableHeader label="Time" column="occurred_at_ms" active={sortKey} direction={direction} onSort={changeSort} />
                <SortableHeader label="Actor" column="actor" active={sortKey} direction={direction} onSort={changeSort} />
                <SortableHeader label="Action" column="action" active={sortKey} direction={direction} onSort={changeSort} />
                <SortableHeader label="Resource" column="resource" active={sortKey} direction={direction} onSort={changeSort} />
                <SortableHeader label="Outcome" column="outcome" active={sortKey} direction={direction} onSort={changeSort} />
                <th><span className="ffdb-sr-only">Details</span></th>
              </tr></thead><tbody>{visible.map((entry) => (
                <tr key={entry.id}>
                  <td data-label="Time"><time dateTime={new Date(entry.occurred_at_ms).toISOString()}>{formatDateTime(entry.occurred_at_ms)}</time></td>
                  <td data-label="Actor"><span className="ffdb-actor"><i>{initials(entry.actor)}</i><span>{entry.actor}</span></span></td>
                  <td data-label="Action"><strong>{friendlyLabel(entry.action)}</strong></td>
                  <td data-label="Resource"><code>{entry.resource}</code></td>
                  <td data-label="Outcome"><StatusBadge tone={entry.outcome === "success" ? "success" : entry.outcome === "denied" ? "warning" : "danger"}>{sentenceCase(entry.outcome)}</StatusBadge></td>
                  <td className="ffdb-row-action"><button className="ffdb-button ffdb-button-quiet" type="button" onClick={() => setSelected(entry)}>Details</button></td>
                </tr>
              ))}</tbody></table>
            </div>
            <Pagination page={safePage} pageCount={pageCount} pageSize={pageSize} total={filtered.length} onPage={setPage} onPageSize={setPageSize} />
          </>
        )}
      </section>
      {selected === null ? null : <ActivityDetail entry={selected} onClose={() => setSelected(null)} />}
    </div>
  );
}

function CodeEditor({ value, onChange, onRun, ariaLabel, minLines, height, tables = {} }: { readonly value: string; onChange(value: string): void; readonly onRun?: () => void; readonly ariaLabel: string; readonly minLines: number; readonly height?: string; readonly tables?: Readonly<Record<string, readonly string[]>> }) {
  const valueRef = useRef(value);
  const onChangeRef = useRef(onChange);
  const onRunRef = useRef(onRun);
  valueRef.current = value;
  onChangeRef.current = onChange;
  onRunRef.current = onRun;

  const extensions = useMemo(() => [
    EditorView.cspNonce.of(CODEMIRROR_STYLE_NONCE),
    sqlLanguage({ schema: tables, upperCaseKeywords: true }),
    syntaxHighlighting(ffdbSqlHighlightStyle),
    autocompletion({ activateOnTyping: true, interactionDelay: 0 }),
    Prec.highest(keymap.of([
      { key: "Meta-Enter", stopPropagation: true, run: () => { onRunRef.current?.(); return onRunRef.current !== undefined; } },
      { key: "Ctrl-Enter", stopPropagation: true, run: () => { onRunRef.current?.(); return onRunRef.current !== undefined; } },
      { key: "Mod-Shift-f", run: () => { onChangeRef.current(formatSql(valueRef.current)); return true; } },
      { key: "Tab", run: acceptSqlCompletionOnTab },
      indentWithTab,
    ])),
    EditorView.contentAttributes.of({ "aria-label": ariaLabel, "aria-multiline": "true", spellcheck: "false" }),
  ], [ariaLabel, tables]);
  const editorHeight = `${Math.max(265, minLines * 22 + 32)}px`;
  return <CodeMirror className="ffdb-code-editor" value={value} height={height ?? editorHeight} theme={ffdbEditorTheme} extensions={extensions} indentWithTab={false} onChange={onChange} basicSetup={{ bracketMatching: true, closeBrackets: true, autocompletion: false, foldGutter: true, highlightActiveLine: true, highlightActiveLineGutter: true, history: true, lineNumbers: true, searchKeymap: true }} />;
}

/** Keep this ahead of indentation keymaps so Tab accepts an open SQL suggestion. */
export function acceptSqlCompletionOnTab(view: EditorView): boolean {
  return completionStatus(view.state) === "active" && acceptCompletion(view);
}

// A neutral editor surface keeps the product accent out of the chrome while
// retaining enough token contrast to scan complex queries quickly.
const ffdbSqlHighlightStyle = HighlightStyle.define([
  { tag: [tags.meta, tags.comment], color: "var(--ffdb-syntax-comment)", fontStyle: "italic" },
  { tag: [tags.attributeName, tags.keyword, tags.controlKeyword, tags.operatorKeyword], color: "var(--ffdb-syntax-keyword)", fontWeight: "650" },
  { tag: tags.function(tags.variableName), color: "var(--ffdb-syntax-function)" },
  { tag: [tags.string, tags.regexp, tags.attributeValue], color: "var(--ffdb-syntax-string)" },
  { tag: [tags.operator, tags.punctuation], color: "var(--ffdb-syntax-punctuation)" },
  { tag: [tags.tagName, tags.modifier], color: "var(--ffdb-syntax-modifier)" },
  { tag: [tags.number, tags.definition(tags.tagName), tags.className, tags.definition(tags.variableName)], color: "var(--ffdb-syntax-number)" },
  { tag: [tags.atom, tags.bool, tags.null, tags.special(tags.variableName)], color: "var(--ffdb-syntax-modifier)" },
  { tag: tags.variableName, color: "var(--ffdb-syntax-variable)" },
  { tag: [tags.propertyName, tags.typeName], color: "var(--ffdb-syntax-function)" },
]);

const ffdbEditorTheme = EditorView.theme({
  "&": { backgroundColor: "var(--ffdb-editor-bg)", color: "var(--ffdb-editor-text)", fontSize: "13px" },
  ".cm-content": { caretColor: "var(--ffdb-editor-caret)", fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace", padding: "14px 0" },
  ".cm-line": { padding: "0 16px" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--ffdb-editor-caret)" },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection": { backgroundColor: "var(--ffdb-editor-selection) !important" },
  ".cm-activeLine": { backgroundColor: "var(--ffdb-editor-active)" },
  ".cm-activeLineGutter": { backgroundColor: "var(--ffdb-editor-active)", color: "var(--ffdb-editor-muted)" },
  ".cm-gutters": { backgroundColor: "var(--ffdb-editor-raised)", color: "var(--ffdb-editor-muted)", borderRight: "1px solid var(--ffdb-editor-border)" },
  ".cm-foldPlaceholder": { backgroundColor: "var(--ffdb-editor-active)", border: "0", color: "var(--ffdb-editor-muted)" },
  ".cm-tooltip": { border: "1px solid var(--ffdb-editor-border)", backgroundColor: "var(--ffdb-editor-tooltip)", color: "var(--ffdb-editor-text)" },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--ffdb-editor-tooltip-selected)", color: "var(--ffdb-editor-text)" },
});

function QueryResultSet({ statements, results }: { readonly statements: readonly string[]; readonly results: readonly QueryResult[] }) {
  const [selected, setSelected] = useState(0);
  const safeSelected = Math.min(selected, Math.max(0, results.length - 1));
  const result = results[safeSelected];
  if (result === undefined) return <EmptyState icon={<Check size={20} />} title="Batch completed" detail="The server completed the transaction without returning statement results." />;
  if (results.length === 1) return <QueryResults result={result} />;
  return (
    <div className="ffdb-batch-results">
      <div className="ffdb-statement-tabs" role="tablist" aria-label="Statement results">
        {results.map((statementResult, index) => (
          <button key={`${index}-${statements[index] ?? "statement"}`} type="button" role="tab" aria-selected={safeSelected === index} onClick={() => setSelected(index)}>
            <span>Statement {index + 1}</span>
            <small>{statementResult.columns.length === 0 ? `${statementResult.affected_rows} affected` : `${statementResult.rows.length}${statementResult.truncated ? "+" : ""} rows`}</small>
          </button>
        ))}
      </div>
      <div className="ffdb-statement-summary"><span>Executed atomically</span><code>{singleLine(statements[safeSelected] ?? "")}</code></div>
      <div role="tabpanel" aria-label={`Statement ${safeSelected + 1} result`}><QueryResults result={result} /></div>
    </div>
  );
}

function QueryResults({ result, page = 1, pageSize = 50, onPage }: { readonly result: QueryResult; readonly page?: number; readonly pageSize?: number; readonly onPage?: (page: number) => void }) {
  if (result.columns.length === 0) return <div className="ffdb-write-result"><span className="ffdb-success-icon"><Check size={17} /></span><div><strong>Statement completed</strong><p>{result.affected_rows.toLocaleString()} {result.affected_rows === 1 ? "row" : "rows"} affected{result.last_insert_rowid === null ? "" : ` · row ID ${result.last_insert_rowid}`}</p></div></div>;
  const pageCount = Math.max(1, Math.ceil(result.rows.length / pageSize));
  const safePage = Math.min(page, pageCount);
  const rows = result.rows.slice((safePage - 1) * pageSize, safePage * pageSize);
  return (
    <>
      {result.truncated ? <Notice tone="warning">Results reached the server row limit. Add a narrower WHERE clause or LIMIT before making decisions from this sample.</Notice> : null}
      <div className="ffdb-table-wrap portal-table-scroll" role="region" aria-label="Query result rows" tabIndex={0}><table className="ffdb-data-table ffdb-result-table"><thead><tr>{result.columns.map((column) => <th key={column.name}><span>{column.name}</span><small>{column.type}</small></th>)}</tr></thead><tbody>{rows.map((row, rowIndex) => <tr key={`${safePage}-${rowIndex}`}>{result.columns.map((column, columnIndex) => <td key={`${column.name}-${columnIndex}`} data-label={column.name}>{formatCell(row[columnIndex])}</td>)}</tr>)}</tbody></table></div>
      {onPage === undefined || pageCount <= 1 ? null : <Pagination page={safePage} pageCount={pageCount} pageSize={pageSize} total={result.rows.length} onPage={onPage} />}
    </>
  );
}

function EditableTableGrid({ actionTarget, client, result, table, onReload }: { readonly actionTarget: HTMLDivElement | null; readonly client: FFDBClient; readonly result: QueryResult; readonly table: TableDefinition; readonly onReload: () => Promise<void> }) {
  const [draftRows, setDraftRows] = useState<readonly (readonly ResultCell[])[]>(() => cloneRows(result.rows));
  const [selectedRows, setSelectedRows] = useState<ReadonlySet<number>>(() => new Set());
  const [pendingDeletes, setPendingDeletes] = useState<ReadonlySet<number>>(() => new Set());
  const [page, setPage] = useState(1);
  const [saving, setSaving] = useState(false);
  const [deleteArmed, setDeleteArmed] = useState(false);
  const [message, setMessage] = useState<{ readonly tone: "success" | "error"; readonly text: string } | null>(null);
  const pageSize = 50;
  const primaryKeys = useMemo(() => primaryKeyColumns(table), [table]);
  const primaryKeyIndexes = useMemo(() => primaryKeys.map((name) => result.columns.findIndex((column) => column.name === name)).filter((index) => index >= 0), [primaryKeys, result.columns]);
  const editable = primaryKeyIndexes.length > 0 && primaryKeyIndexes.length === primaryKeys.length;
  const dirtyCells = useMemo(() => {
    const cells = new Set<string>();
    for (let rowIndex = 0; rowIndex < draftRows.length; rowIndex += 1) {
      for (let columnIndex = 0; columnIndex < result.columns.length; columnIndex += 1) {
        if (!sameCell(draftRows[rowIndex]?.[columnIndex], result.rows[rowIndex]?.[columnIndex])) cells.add(`${rowIndex}:${columnIndex}`);
      }
    }
    return cells;
  }, [draftRows, result.columns.length, result.rows]);
  const dirtyRows = useMemo(() => new Set([...dirtyCells].map((cell) => Number(cell.split(":", 1)[0]))), [dirtyCells]);
  const pageCount = Math.max(1, Math.ceil(draftRows.length / pageSize));
  const safePage = Math.min(page, pageCount);
  const firstRow = (safePage - 1) * pageSize;
  const visibleRows = draftRows.slice(firstRow, firstRow + pageSize);
  const visibleIndexes = visibleRows.map((_, index) => firstRow + index);
  const allVisibleSelected = visibleIndexes.length > 0 && visibleIndexes.every((index) => selectedRows.has(index));
  const hasWork = dirtyRows.size > 0 || pendingDeletes.size > 0;

  useEffect(() => {
    setDraftRows(cloneRows(result.rows));
    setSelectedRows(new Set());
    setPendingDeletes(new Set());
    setPage(1);
    setDeleteArmed(false);
  }, [result]);

  const toggleRow = (rowIndex: number) => {
    setSelectedRows((current) => {
      const next = new Set(current);
      if (next.has(rowIndex)) next.delete(rowIndex); else next.add(rowIndex);
      return next;
    });
    setDeleteArmed(false);
  };

  const toggleVisible = () => {
    setSelectedRows((current) => {
      const next = new Set(current);
      if (allVisibleSelected) visibleIndexes.forEach((index) => next.delete(index)); else visibleIndexes.forEach((index) => next.add(index));
      return next;
    });
    setDeleteArmed(false);
  };

  const updateCell = (rowIndex: number, columnIndex: number, raw: string) => {
    const column = result.columns[columnIndex];
    if (column === undefined) return;
    setDraftRows((current) => current.map((row, index) => index === rowIndex ? row.map((value, valueIndex) => valueIndex === columnIndex ? editableCellValue(raw, column.type) : value) : row));
    setMessage(null);
  };

  const discard = () => {
    setDraftRows(cloneRows(result.rows));
    setSelectedRows(new Set());
    setPendingDeletes(new Set());
    setDeleteArmed(false);
    setMessage(null);
  };

  const markSelectedForDeletion = () => {
    if (!deleteArmed) { setDeleteArmed(true); return; }
    setPendingDeletes((current) => new Set([...current, ...selectedRows]));
    setSelectedRows(new Set());
    setDeleteArmed(false);
  };

  const save = async () => {
    if (!editable || saving || !hasWork) return;
    const statements: { readonly sql: string; readonly parameters: readonly SqlParameter[] }[] = [];
    for (const rowIndex of dirtyRows) {
      if (pendingDeletes.has(rowIndex)) continue;
      const changedIndexes = result.columns.map((_, index) => index).filter((columnIndex) => dirtyCells.has(`${rowIndex}:${columnIndex}`) && !primaryKeyIndexes.includes(columnIndex));
      if (changedIndexes.length === 0) continue;
      const original = result.rows[rowIndex];
      const draft = draftRows[rowIndex];
      if (original === undefined || draft === undefined) continue;
      const setClause = changedIndexes.map((columnIndex) => `${quoteIdentifier(result.columns[columnIndex]?.name ?? "")} = ?`).join(", ");
      const whereClause = primaryKeyIndexes.map((columnIndex) => `${quoteIdentifier(result.columns[columnIndex]?.name ?? "")} IS ?`).join(" AND ");
      statements.push({
        sql: `UPDATE ${quoteIdentifier(table.name)} SET ${setClause} WHERE ${whereClause};`,
        parameters: [...changedIndexes.map((columnIndex) => sqlParameter(draft[columnIndex])), ...primaryKeyIndexes.map((columnIndex) => sqlParameter(original[columnIndex]))],
      });
    }
    for (const rowIndex of pendingDeletes) {
      const original = result.rows[rowIndex];
      if (original === undefined) continue;
      const whereClause = primaryKeyIndexes.map((columnIndex) => `${quoteIdentifier(result.columns[columnIndex]?.name ?? "")} IS ?`).join(" AND ");
      statements.push({ sql: `DELETE FROM ${quoteIdentifier(table.name)} WHERE ${whereClause};`, parameters: primaryKeyIndexes.map((columnIndex) => sqlParameter(original[columnIndex])) });
    }
    if (statements.length === 0) return;
    setSaving(true);
    setMessage(null);
    try {
      await client.transaction({ statements });
      await onReload();
      setMessage({ tone: "success", text: `${statements.length} ${statements.length === 1 ? "change" : "changes"} saved atomically.` });
    } catch (cause) {
      setMessage({ tone: "error", text: errorMessage(cause) });
    } finally {
      setSaving(false);
    }
  };

  if (result.columns.length === 0) return <QueryResults result={result} />;

  return (
    <div className="ffdb-editable-grid">
      {result.truncated ? <Notice tone="warning">Only the first 500 rows are loaded. Narrow the table with SQL before making broad edits.</Notice> : null}
      {editable ? <p className="ffdb-grid-hint">Edit cells directly. Changes are staged locally until you save once.</p> : <Notice tone="warning">This table has no detectable primary key, so its preview is read-only.</Notice>}
      {message === null ? null : <Notice tone={message.tone}>{message.text}</Notice>}
      <div className="ffdb-table-wrap portal-table-scroll" role="region" aria-label="Editable table rows" tabIndex={0}>
        <table className="ffdb-data-table ffdb-result-table ffdb-edit-table">
          <thead><tr><th className="ffdb-select-cell"><input type="checkbox" aria-label="Select visible rows" checked={allVisibleSelected} onChange={toggleVisible} disabled={!editable} /></th>{result.columns.map((column) => <th key={column.name}><span>{column.name}</span><small>{primaryKeys.includes(column.name) ? "primary key" : column.type}</small></th>)}</tr></thead>
          <tbody>{visibleRows.map((row, visibleIndex) => {
            const rowIndex = firstRow + visibleIndex;
            const deleting = pendingDeletes.has(rowIndex);
            return <tr key={rowIdentity(row, primaryKeyIndexes, rowIndex)} className={deleting ? "is-pending-delete" : dirtyRows.has(rowIndex) ? "is-dirty" : ""}>
              <td className="ffdb-select-cell" data-label="Select"><input type="checkbox" aria-label={`Select row ${rowIndex + 1}`} checked={selectedRows.has(rowIndex)} disabled={!editable || deleting} onChange={() => toggleRow(rowIndex)} /></td>
              {result.columns.map((column, columnIndex) => {
                const value = row[columnIndex];
                const canEdit = editable && !deleting && !primaryKeyIndexes.includes(columnIndex) && column.type !== "blob";
                return <td key={`${column.name}-${columnIndex}`} data-label={column.name} className={dirtyCells.has(`${rowIndex}:${columnIndex}`) ? "is-edited" : ""}>{canEdit ? <input aria-label={`Edit ${column.name} row ${rowIndex + 1}`} type={column.type === "integer" || column.type === "real" ? "number" : "text"} step={column.type === "integer" ? "1" : column.type === "real" ? "any" : undefined} value={editableCellText(value)} onChange={(event) => updateCell(rowIndex, columnIndex, event.target.value)} /> : formatCell(value)}</td>;
              })}
            </tr>;
          })}</tbody>
        </table>
      </div>
      {pageCount <= 1 ? null : <Pagination page={safePage} pageCount={pageCount} pageSize={pageSize} total={draftRows.length} onPage={setPage} />}
      {editable && actionTarget !== null && (hasWork || selectedRows.size > 0) ? createPortal(<div className="ffdb-grid-toolbar-actions" role="toolbar" aria-label="Pending table changes">
        <span className="ffdb-grid-toolbar-summary">{dirtyRows.size} modified · {pendingDeletes.size} marked{selectedRows.size > 0 ? ` · ${selectedRows.size} selected` : ""}</span>
        <div className="ffdb-grid-toolbar-buttons">
          <button className="ffdb-button ffdb-button-quiet" type="button" onClick={discard}><Undo2 size={14} /> Discard</button>
          {selectedRows.size === 0 ? null : <button className="ffdb-button ffdb-button-danger-quiet" type="button" onClick={markSelectedForDeletion}><Trash2 size={14} /> {deleteArmed ? `Confirm ${selectedRows.size}` : "Delete selected"}</button>}
          <button className="ffdb-button ffdb-button-primary" type="button" disabled={!hasWork || saving} onClick={() => void save()}>{saving ? <RefreshCw className="ffdb-spin" size={14} /> : <Save size={14} />}{saving ? "Saving…" : "Save changes"}</button>
        </div>
      </div>, actionTarget) : null}
    </div>
  );
}

function QueryRunMeta({ run }: { readonly run: QueryRun }) {
  const results = run.results ?? [];
  const rows = results.reduce((total, result) => total + result.rows.length, 0);
  return <div className="ffdb-run-meta"><span><Clock3 size={13} /> {run.durationMs} ms</span>{run.results === null ? null : <span>{results.length > 1 ? `${results.length} statements` : results[0]?.columns.length === 0 ? `${results[0]?.affected_rows ?? 0} affected` : `${rows} ${rows === 1 ? "row" : "rows"}`}</span>}</div>;
}

function MigrationReview({ id, name, upSql, downSql, checksum, submitting, onBack, onApply }: { readonly id: string; readonly name: string; readonly upSql: string; readonly downSql: string; readonly checksum: string; readonly submitting: boolean; readonly onBack: () => void; readonly onApply: () => void }) {
  return <div className="ffdb-migration-review"><Notice tone="warning"><strong>Review before applying.</strong> The up SQL changes the live schema atomically. Rollback will execute the stored down SQL.</Notice><dl><div><dt>Name</dt><dd>{name}</dd></div><div><dt>ID</dt><dd><code>{id}</code></dd></div><div><dt>Idempotency key</dt><dd><code>migration:{id}:{checksum.slice(0, 12)}…</code></dd></div><div><dt>Checksum</dt><dd><code>{checksum}</code></dd></div></dl><div className="ffdb-review-diff"><div><span className="is-up"><ArrowUp size={14} /> Apply</span><pre><code>{highlightSql(upSql)}</code></pre></div><div><span className="is-down"><ArrowDown size={14} /> Rollback</span><pre><code>{highlightSql(downSql)}</code></pre></div></div><footer><button className="ffdb-button ffdb-button-secondary" type="button" disabled={submitting} onClick={onBack}>Back to edit</button><button className="ffdb-button ffdb-button-primary" type="button" disabled={submitting || checksum === ""} onClick={onApply}>{submitting ? <RefreshCw className="ffdb-spin" size={15} /> : <Check size={15} />}{submitting ? "Applying…" : "Apply migration"}</button></footer></div>;
}

function ActivityDetail({ entry, onClose }: { readonly entry: AuditLogEntry; readonly onClose: () => void }) {
  return <aside className="ffdb-detail-drawer" aria-label="Activity event details"><div className="ffdb-drawer-backdrop" onClick={onClose} /><section role="dialog" aria-modal="true" aria-labelledby="ffdb-event-title"><header><div><span className="ffdb-eyebrow">Audit event</span><h2 id="ffdb-event-title">{friendlyLabel(entry.action)}</h2></div><button type="button" aria-label="Close event details" onClick={onClose}><X size={18} /></button></header><StatusBadge tone={entry.outcome === "success" ? "success" : entry.outcome === "denied" ? "warning" : "danger"}>{sentenceCase(entry.outcome)}</StatusBadge><dl><div><dt>Occurred</dt><dd>{formatDateTime(entry.occurred_at_ms)}</dd></div><div><dt>Actor</dt><dd>{entry.actor}</dd></div><div><dt>Resource</dt><dd><code>{entry.resource}</code></dd></div><div><dt>Event ID</dt><dd><code>{entry.id}</code></dd></div><div><dt>Request ID</dt><dd>{entry.request_id === null ? "Not recorded" : <code>{entry.request_id}</code>}</dd></div></dl>{entry.request_id === null ? null : <CopyButton value={entry.request_id} label="Copy request ID" />}</section></aside>;
}

function SortableHeader({ label, column, active, direction, onSort }: { readonly label: string; readonly column: ActivitySortKey; readonly active: ActivitySortKey; readonly direction: SortDirection; readonly onSort: (column: ActivitySortKey) => void }) {
  const icon = active !== column ? <ArrowUpDown size={13} /> : direction === "asc" ? <ArrowUp size={13} /> : <ArrowDown size={13} />;
  return <th aria-sort={active === column ? direction === "asc" ? "ascending" : "descending" : "none"}><button type="button" onClick={() => onSort(column)}>{label}{icon}</button></th>;
}

function Pagination({ page, pageCount, pageSize, total, onPage, onPageSize }: { readonly page: number; readonly pageCount: number; readonly pageSize: number; readonly total: number; readonly onPage: (page: number) => void; readonly onPageSize?: (pageSize: number) => void }) {
  const first = total === 0 ? 0 : (page - 1) * pageSize + 1;
  const last = Math.min(page * pageSize, total);
  return <footer className="ffdb-pagination"><span>{first.toLocaleString()}–{last.toLocaleString()} of {total.toLocaleString()}</span><div>{onPageSize === undefined ? null : <label><span>Rows</span><select value={pageSize} onChange={(event) => onPageSize(Number(event.target.value))}><option value={10}>10</option><option value={25}>25</option><option value={50}>50</option></select></label>}<button type="button" aria-label="Previous page" disabled={page <= 1} onClick={() => onPage(page - 1)}><ChevronLeft size={16} /></button><span>Page {page} of {pageCount}</span><button type="button" aria-label="Next page" disabled={page >= pageCount} onClick={() => onPage(page + 1)}><ChevronRight size={16} /></button></div></footer>;
}

function StatusBadge({ tone, children }: { readonly tone: "success" | "warning" | "danger" | "neutral"; readonly children: ReactNode }) { return <span className={`ffdb-status-badge is-${tone}`}>{children}</span>; }
function Notice({ tone, children }: { readonly tone: "success" | "error" | "warning"; readonly children: ReactNode }) { return <div className={`ffdb-notice is-${tone}`} role={tone === "error" ? "alert" : "status"}>{children}</div>; }
function InlineLoading({ label }: { readonly label: string }) { return <div className="ffdb-inline-state"><RefreshCw className="ffdb-spin" size={16} /><span>{label}…</span></div>; }
function InlineError({ message }: { readonly message: string }) { return <div className="ffdb-inline-error" role="alert"><strong>Request failed</strong><span>{message}</span></div>; }
function PageLoading({ label }: { readonly label: string }) { return <section className="ffdb-surface"><InlineLoading label={label} /></section>; }
function PageError({ title, message, onRetry }: { readonly title: string; readonly message: string; readonly onRetry: () => void }) { return <section className="ffdb-surface"><div className="ffdb-page-error" role="alert"><X size={20} /><div><h2>{title}</h2><p>{message}</p><button className="ffdb-button ffdb-button-secondary" type="button" onClick={onRetry}><RefreshCw size={15} /> Try again</button></div></div></section>; }
function EmptyState({ icon, title, detail, action }: { readonly icon: ReactNode; readonly title: string; readonly detail: string; readonly action?: ReactNode }) { return <div className="ffdb-empty-state"><span>{icon}</span><h3>{title}</h3><p>{detail}</p>{action}</div>; }

function CopyButton({ value, label }: { readonly value: string; readonly label: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => { await globalThis.navigator.clipboard.writeText(value); setCopied(true); globalThis.setTimeout(() => setCopied(false), 1_500); };
  return <button className="ffdb-icon-button" type="button" aria-label={label} title={label} onClick={() => void copy()}>{copied ? <Check size={14} /> : <Copy size={14} />}</button>;
}

function formatCell(value: ResultCell | undefined): ReactNode {
  if (value === null) return <em className="ffdb-null">NULL</em>;
  if (value === undefined) return "—";
  if (typeof value === "object") return <code className="ffdb-blob">blob:{value.$blob.slice(0, 18)}{value.$blob.length > 18 ? "…" : ""}</code>;
  if (typeof value === "number") return <span className="ffdb-number">{value.toLocaleString()}</span>;
  return value;
}

function cloneRows(rows: readonly (readonly ResultCell[])[]): readonly (readonly ResultCell[])[] {
  return rows.map((row) => [...row]);
}

function sameCell(left: ResultCell | undefined, right: ResultCell | undefined): boolean {
  if (typeof left === "object" && left !== null && typeof right === "object" && right !== null) return left.$blob === right.$blob;
  return Object.is(left, right);
}

function editableCellText(value: ResultCell | undefined): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "object") return value.$blob;
  return String(value);
}

function editableCellValue(value: string, type: QueryResult["columns"][number]["type"]): ResultCell {
  if (value === "") return type === "text" || type === "date" || type === "timestamp" || type === "unknown" ? "" : null;
  if (type === "integer") {
    const parsed = Number.parseInt(value, 10);
    return Number.isFinite(parsed) ? parsed : null;
  }
  if (type === "real") {
    const parsed = Number.parseFloat(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return value;
}

function sqlParameter(value: ResultCell | undefined): SqlParameter {
  if (value === null || value === undefined) return { type: "null" };
  if (typeof value === "number") return Number.isInteger(value) ? { type: "integer", value } : { type: "real", value };
  if (typeof value === "object") return { type: "blob", value: value.$blob };
  return { type: "text", value };
}

function rowIdentity(row: readonly ResultCell[], primaryKeyIndexes: readonly number[], fallback: number): string {
  if (primaryKeyIndexes.length === 0) return `row-${fallback}`;
  return primaryKeyIndexes.map((index) => editableCellText(row[index])).join("\u0000");
}

function primaryKeyColumns(table: TableDefinition): readonly string[] {
  const tableLevel = /\bPRIMARY\s+KEY\s*\(([^)]+)\)/iu.exec(table.sql)?.[1];
  if (tableLevel !== undefined) return identifiers(tableLevel);
  const open = table.sql.indexOf("(");
  const close = table.sql.lastIndexOf(")");
  if (open < 0 || close <= open) return [];
  const definitions = splitSqlDefinitions(table.sql.slice(open + 1, close));
  for (const definition of definitions) {
    if (!/\bPRIMARY\s+KEY\b/iu.test(definition)) continue;
    const match = /^\s*(?:"([^"]+)"|`([^`]+)`|\[([^\]]+)\]|([A-Za-z_][A-Za-z0-9_]*))/u.exec(definition);
    const name = match?.slice(1).find((value) => value !== undefined);
    if (name !== undefined && name.toLocaleUpperCase() !== "CONSTRAINT") return [name];
  }
  return [];
}

function identifiers(value: string): readonly string[] {
  return [...value.matchAll(/"([^"]+)"|`([^`]+)`|\[([^\]]+)\]|([A-Za-z_][A-Za-z0-9_]*)/gu)].map((match) => match.slice(1).find((part) => part !== undefined) ?? "").filter(Boolean);
}

function splitSqlDefinitions(value: string): readonly string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let index = 0; index < value.length; index += 1) {
    const character = value[index];
    if (character === "(") depth += 1;
    else if (character === ")") depth = Math.max(0, depth - 1);
    else if (character === "," && depth === 0) { parts.push(value.slice(start, index)); start = index + 1; }
  }
  parts.push(value.slice(start));
  return parts;
}

/** Split an editor batch without treating semicolons in SQLite literals,
 * identifiers, comments, parentheses, or trigger bodies as boundaries. */
export function splitSqlStatements(sql: string): readonly string[] {
  const statements: string[] = [];
  let start = 0;
  let index = 0;
  let parenthesisDepth = 0;
  let triggerBodyDepth = 0;
  let possibleTrigger = false;
  let words: string[] = [];

  while (index < sql.length) {
    const character = sql[index]!;
    if (character === "'" || character === '"' || character === "`") {
      index = skipSqlQuoted(sql, index, character);
    } else if (character === "[") {
      index = skipSqlBracketIdentifier(sql, index);
    } else if (character === "-" && sql[index + 1] === "-") {
      index += 2;
      while (index < sql.length && sql[index] !== "\n" && sql[index] !== "\r") index += 1;
    } else if (character === "/" && sql[index + 1] === "*") {
      index = skipSqlBlockComment(sql, index);
    } else if (character === "(") {
      parenthesisDepth += 1;
      index += 1;
    } else if (character === ")") {
      if (parenthesisDepth === 0) throw new Error("SQL contains an unbalanced parenthesis.");
      parenthesisDepth -= 1;
      index += 1;
    } else if (isSqlWordStart(character)) {
      const wordStart = index;
      index += 1;
      while (index < sql.length && isSqlWordContinue(sql[index]!)) index += 1;
      const word = sql.slice(wordStart, index).toLocaleUpperCase();
      if (parenthesisDepth === 0) {
        if (words.length < 3) {
          words = [...words, word];
          possibleTrigger ||= words[0] === "CREATE" && (words[1] === "TRIGGER" || ((words[1] === "TEMP" || words[1] === "TEMPORARY") && words[2] === "TRIGGER"));
        }
        if ((possibleTrigger && word === "BEGIN") || (triggerBodyDepth > 0 && word === "CASE")) triggerBodyDepth += 1;
        else if (triggerBodyDepth > 0 && word === "END") triggerBodyDepth -= 1;
      }
    } else if (character === ";" && parenthesisDepth === 0 && triggerBodyDepth === 0) {
      const candidate = sql.slice(start, index).trim();
      if (hasExecutableSql(candidate)) statements.push(candidate);
      index += 1;
      start = index;
      words = [];
      possibleTrigger = false;
    } else {
      index += 1;
    }
  }

  if (parenthesisDepth !== 0) throw new Error("SQL contains an unbalanced parenthesis.");
  const tail = sql.slice(start).trim();
  if (hasExecutableSql(tail)) statements.push(tail);
  return statements;
}

function skipSqlQuoted(sql: string, start: number, quote: string): number {
  let index = start + 1;
  while (index < sql.length) {
    if (sql[index] !== quote) { index += 1; continue; }
    if (sql[index + 1] === quote) { index += 2; continue; }
    return index + 1;
  }
  throw new Error(quote === "'" ? "SQL contains an unterminated string." : "SQL contains an unterminated quoted identifier.");
}

function skipSqlBracketIdentifier(sql: string, start: number): number {
  let index = start + 1;
  while (index < sql.length) {
    if (sql[index] !== "]") { index += 1; continue; }
    if (sql[index + 1] === "]") { index += 2; continue; }
    return index + 1;
  }
  throw new Error("SQL contains an unterminated quoted identifier.");
}

function skipSqlBlockComment(sql: string, start: number): number {
  const end = sql.indexOf("*/", start + 2);
  if (end < 0) throw new Error("SQL contains an unterminated block comment.");
  return end + 2;
}

function hasExecutableSql(sql: string): boolean {
  let index = 0;
  while (index < sql.length) {
    const character = sql[index]!;
    if (/\s/u.test(character)) index += 1;
    else if (character === "-" && sql[index + 1] === "-") {
      index += 2;
      while (index < sql.length && sql[index] !== "\n" && sql[index] !== "\r") index += 1;
    } else if (character === "/" && sql[index + 1] === "*") index = skipSqlBlockComment(sql, index);
    else return true;
  }
  return false;
}

function isSqlWordStart(character: string): boolean { return /[A-Za-z_]/u.test(character); }
function isSqlWordContinue(character: string): boolean { return /[A-Za-z0-9_$]/u.test(character); }

function errorMessage(cause: unknown): string {
  if (cause instanceof FFDBError) return `${cause.code}: ${cause.message}${cause.requestId === null ? "" : ` · Request ${cause.requestId}`}`;
  return cause instanceof Error ? cause.message : String(cause);
}

function singleLine(value: string): string { const compact = value.replace(/\s+/gu, " ").trim(); return compact.length > 52 ? `${compact.slice(0, 49)}…` : compact; }
function quoteIdentifier(value: string): string { return `"${value.replaceAll('"', '""')}"`; }
function mightChangeSchema(value: string): boolean { return /\b(?:CREATE|ALTER|DROP|REINDEX|VACUUM)\b/iu.test(value); }
function sentenceCase(value: string): string { const normalized = value.replaceAll("_", " "); return `${normalized[0]?.toLocaleUpperCase() ?? ""}${normalized.slice(1)}`; }
function friendlyLabel(value: string): string { return sentenceCase(value.split(/[.:]/u).filter(Boolean).slice(-2).join(" ")); }
function formatDateTime(value: number): string { return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(value); }
function initials(value: string): string { const parts = value.split(/[\s@._:-]+/u).filter(Boolean); return parts.slice(0, 2).map((part) => part[0]?.toLocaleUpperCase() ?? "").join("") || "FF"; }
function migrationId(date = new Date()): string { return date.toISOString().replace(/[-:T.Z]/gu, "").slice(0, 14); }
function validateMigration(id: string, name: string, up: string, down: string): string | null { if (id.trim() === "") return "Migration ID is required."; if (name.trim() === "") return "Migration name is required."; if (up.trim() === "") return "Up SQL is required."; if (down.trim() === "") return "Down SQL is required so this change can be rolled back."; return null; }
async function migrationChecksum(id: string, name: string, up: string, down: string): Promise<string> { const source = new TextEncoder().encode(`${id}\0${name}\0${up}\0${down}`); const digest = await globalThis.crypto.subtle.digest("SHA-256", source); return [...new Uint8Array(digest)].map((value) => value.toString(16).padStart(2, "0")).join(""); }

function compareActivity(left: AuditLogEntry, right: AuditLogEntry, key: ActivitySortKey, direction: SortDirection): number {
  const a = left[key]; const b = right[key]; const result = typeof a === "number" && typeof b === "number" ? a - b : String(a).localeCompare(String(b)); return direction === "asc" ? result : -result;
}

const SQL_KEYWORDS = new Set(["ABORT", "ACTION", "ADD", "AFTER", "ALL", "ALTER", "ANALYZE", "AND", "AS", "ASC", "ATTACH", "AUTOINCREMENT", "BEFORE", "BEGIN", "BETWEEN", "BY", "CASCADE", "CASE", "CAST", "CHECK", "COLLATE", "COLUMN", "COMMIT", "CONFLICT", "CONSTRAINT", "CREATE", "CROSS", "CURRENT", "DATABASE", "DEFAULT", "DEFERRABLE", "DELETE", "DESC", "DETACH", "DISTINCT", "DO", "DROP", "EACH", "ELSE", "END", "ESCAPE", "EXCEPT", "EXCLUDE", "EXISTS", "EXPLAIN", "FAIL", "FILTER", "FOLLOWING", "FOR", "FOREIGN", "FROM", "FULL", "GENERATED", "GLOB", "GROUP", "HAVING", "IF", "IGNORE", "IMMEDIATE", "IN", "INDEX", "INDEXED", "INITIALLY", "INNER", "INSERT", "INSTEAD", "INTERSECT", "INTO", "IS", "ISNULL", "JOIN", "KEY", "LEFT", "LIKE", "LIMIT", "MATCH", "MATERIALIZED", "NATURAL", "NO", "NOT", "NOTHING", "NOTNULL", "NULL", "OF", "OFFSET", "ON", "OR", "ORDER", "OTHERS", "OUTER", "OVER", "PARTITION", "PLAN", "PRAGMA", "PRIMARY", "QUERY", "RAISE", "RANGE", "RECURSIVE", "REFERENCES", "REGEXP", "REINDEX", "RELEASE", "RENAME", "REPLACE", "RESTRICT", "RETURNING", "RIGHT", "ROLLBACK", "ROW", "ROWS", "SAVEPOINT", "SELECT", "SET", "TABLE", "TEMP", "TEMPORARY", "THEN", "TIES", "TO", "TRANSACTION", "TRIGGER", "UNBOUNDED", "UNION", "UNIQUE", "UPDATE", "USING", "VACUUM", "VALUES", "VIEW", "VIRTUAL", "WHEN", "WHERE", "WINDOW", "WITH", "WITHOUT"]);

function highlightSql(value: string): ReactNode[] {
  const tokens = value.match(/--[^\n]*|\/\*[\s\S]*?\*\/|'(?:''|[^'])*'|"(?:""|[^"])*"|\b\d+(?:\.\d+)?\b|\b[A-Za-z_][A-Za-z0-9_]*\b|\s+|./gu) ?? [];
  return tokens.map((token, index) => {
    let className = "";
    if (token.startsWith("--") || token.startsWith("/*")) className = "ffdb-sql-comment";
    else if (token.startsWith("'") || token.startsWith('"')) className = "ffdb-sql-string";
    else if (/^\d/u.test(token)) className = "ffdb-sql-number";
    else if (SQL_KEYWORDS.has(token.toLocaleUpperCase())) className = "ffdb-sql-keyword";
    else if (/^[A-Za-z_]/u.test(token)) className = "ffdb-sql-name";
    return className === "" ? token : <span className={className} key={index}>{token}</span>;
  });
}

function formatSql(value: string): string {
  try { return format(value, { language: "sqlite", tabWidth: 2, keywordCase: "upper" }); }
  catch { return value; }
}

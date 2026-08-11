import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { completionStatus, startCompletion } from "@codemirror/autocomplete";
import { EditorView } from "@codemirror/view";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { FFDBClient, MemorySessionStore, type AuditLogEntry } from "@ffdb/client";

import { ActivityPanel, DatabasePanel, MigrationsPanel, SqlEditorPanel, splitSqlStatements } from "./DatabaseActivity.js";

beforeAll(() => {
  if (Range.prototype.getClientRects === undefined) {
    Object.defineProperty(Range.prototype, "getClientRects", { value: () => ({ length: 0, item: () => null, [Symbol.iterator]: function* iterator() { return; } }) });
  }
});
afterEach(cleanup);

function clientFor(fetchImpl: typeof fetch): FFDBClient {
  return new FFDBClient({ baseUrl: "https://ffdb.example.test", projectId: "project-1", developerKey: "ffdb_dev_test.secret", fetch: fetchImpl });
}

describe("polished database workflows", () => {
  it("executes SQL from the CodeMirror workbench and renders typed results", async () => {
    const calls: Request[] = [];
    const client = clientFor(async (input, init) => {
      const request = new Request(input, init); calls.push(request);
      if (request.url.endsWith("/schema")) return Response.json({ version: 2, tables: [{ name: "documents", sql: "CREATE TABLE documents(id TEXT)", rls_enabled: true, rls_forced: true }] });
      if (request.url.endsWith("/query")) return Response.json({ columns: [{ name: "version", type: "text" }], rows: [["3.49.0"]], affected_rows: 0, last_insert_rowid: null, truncated: false });
      return Response.json({ error: { code: "missing", message: "missing", request_id: "request-1" } }, { status: 404 });
    });

    render(<SqlEditorPanel client={client} />);
    expect(await screen.findByRole("button", { name: /documents/i })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Run query/i }));

    expect(await screen.findByText("3.49.0")).toBeInTheDocument();
    const request = calls.find((call) => call.url.endsWith("/query"));
    expect(request).toBeDefined();
    await expect(request?.json()).resolves.toMatchObject({ sql: "SELECT sqlite_version() AS version" });
  });

  it("reads RLS-protected tables with the portal credential even after the Auth tools create an end-user session", async () => {
    const calls: Request[] = [];
    const sessions = new MemorySessionStore("portal-operator-rls-test");
    await sessions.set({
      access_token: "end-user-access",
      refresh_token: "end-user-refresh",
      token_type: "Bearer",
      expires_in_seconds: 900,
      session_id: "end-user-session",
      user: {
        id: "user-1",
        email: "user@example.test",
        email_verified: true,
        disabled: false,
        role: "authenticated",
        custom_claims: {},
        created_at_ms: 1,
      },
    });
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      developerKey: "ffdb_dev_portal.secret",
      sessionStore: sessions,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [{ name: "documents", sql: "CREATE TABLE documents(id TEXT PRIMARY KEY)", rls_enabled: true, rls_forced: true }] });
        if (request.url.endsWith("/migrations")) return Response.json([]);
        if (request.url.endsWith("/query")) return Response.json({ columns: [{ name: "id", type: "text" }], rows: [["doc-1"]], affected_rows: 0, last_insert_rowid: null, truncated: false });
        return Response.json({ error: { code: "missing", message: "missing", request_id: "request-1" } }, { status: 404 });
      },
    });

    render(<DatabasePanel client={client} />);

    expect(await screen.findByText("doc-1")).toBeInTheDocument();
    const query = calls.find((call) => call.url.endsWith("/query"));
    expect(query?.headers.get("authorization")).toBe("Bearer ffdb_dev_portal.secret");
  });

  it("accepts autocomplete with Tab and runs a semicolon-delimited batch with Command-Enter", async () => {
    const calls: Request[] = [];
    const client = clientFor(async (input, init) => {
      const request = new Request(input, init); calls.push(request);
      if (request.url.endsWith("/schema")) return Response.json({ version: 4, tables: [{ name: "documents", sql: "CREATE TABLE documents(id TEXT)", rls_enabled: true, rls_forced: true }] });
      if (request.url.endsWith("/transaction")) return Response.json([
        { columns: [{ name: "first", type: "text" }], rows: [["a;b"]], affected_rows: 0, last_insert_rowid: null, truncated: false },
        { columns: [{ name: "second", type: "integer" }], rows: [[2]], affected_rows: 0, last_insert_rowid: null, truncated: false },
      ]);
      return Response.json({ error: { code: "missing", message: "missing", request_id: "request-1" } }, { status: 404 });
    });

    const batch = "SELECT 'a;b' AS first; -- this semicolon stays in the comment ;\nSELECT 2 AS second;";
    render(<SqlEditorPanel client={client} initialSql={batch} />);
    await screen.findByText(/Version 4 · 1 table/u);
    expect(screen.queryByRole("tablist", { name: "Open queries" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "New query" })).not.toBeInTheDocument();
    const editor = screen.getByRole("textbox", { name: "SQL query" });
    fireEvent.keyDown(editor, { key: "Enter", code: "Enter", metaKey: true });

    const transaction = await waitFor(() => {
      const call = calls.find((request) => request.url.endsWith("/transaction"));
      expect(call).toBeDefined();
      return call!;
    });
    await expect(transaction.json()).resolves.toMatchObject({ statements: [
      { sql: "SELECT 'a;b' AS first" },
      { sql: "-- this semicolon stays in the comment ;\nSELECT 2 AS second" },
    ] });
    expect(await screen.findByRole("tab", { name: /Statement 1/u })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: /Statement 2/u }));
    expect(within(screen.getByRole("tabpanel", { name: "Statement 2 result" })).getByText("2")).toBeInTheDocument();

    cleanup();
    render(<SqlEditorPanel client={client} initialSql="SELECT * FROM doc" />);
    await screen.findByText(/Version 4 · 1 table/u);
    const completionEditor = screen.getByRole("textbox", { name: "SQL query" });
    const completionView = EditorView.findFromDOM(completionEditor);
    if (completionView === null) throw new Error("CodeMirror view was not mounted.");
    completionView.focus();
    completionView.dispatch({ selection: { anchor: completionView.state.doc.length } });
    expect(startCompletion(completionView)).toBe(true);
    await waitFor(() => expect(completionStatus(completionView.state)).toBe("active"));
    fireEvent.keyDown(completionEditor, { key: "Tab", code: "Tab" });
    await waitFor(() => expect(completionView.state.doc.toString()).toBe("SELECT * FROM documents"));
  });

  it("splits trigger bodies, comments, and quoted semicolons without corrupting a batch", () => {
    expect(splitSqlStatements("SELECT ';' AS value; SELECT \"semi;colon\";"))
      .toEqual(["SELECT ';' AS value", "SELECT \"semi;colon\""]);
    expect(splitSqlStatements("CREATE TRIGGER audit AFTER INSERT ON docs BEGIN INSERT INTO log VALUES ('a;b'); UPDATE stats SET total = total + 1; END; SELECT 1;"))
      .toEqual(["CREATE TRIGGER audit AFTER INSERT ON docs BEGIN INSERT INTO log VALUES ('a;b'); UPDATE stats SET total = total + 1; END", "SELECT 1"]);
    expect(() => splitSqlStatements("SELECT 'unterminated;")).toThrow("unterminated string");
  });

  it("filters audit activity and opens a useful event detail drawer", async () => {
    const entries: readonly AuditLogEntry[] = [
      { id: "event-1", occurred_at_ms: 2_000, actor: "owner@example.test", action: "storage.object.read", resource: "bucket/docs", outcome: "success", request_id: "request-success" },
      { id: "event-2", occurred_at_ms: 1_000, actor: "viewer@example.test", action: "database.query", resource: "documents", outcome: "denied", request_id: "request-denied" },
    ];
    const client = clientFor(async () => Response.json(entries));
    render(<ActivityPanel client={client} />);

    expect(await screen.findByText("owner@example.test")).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Filter by outcome" }), { target: { value: "denied" } });
    expect(screen.queryByText("owner@example.test")).not.toBeInTheDocument();
    expect(screen.getByText("viewer@example.test")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Details" }));

    const drawer = screen.getByRole("dialog", { name: "Database query" });
    expect(within(drawer).getByText("request-denied")).toBeInTheDocument();
    expect(within(drawer).getByText("viewer@example.test")).toBeInTheDocument();
  });

  it("searches, sorts, and paginates a growing activity log", async () => {
    const entries = Array.from({ length: 26 }, (_, index): AuditLogEntry => ({
      id: `event-${index + 1}`,
      occurred_at_ms: index + 1,
      actor: `actor-${String(index + 1).padStart(2, "0")}@example.test`,
      action: index % 2 === 0 ? "database.query" : "storage.object.read",
      resource: `resource-${index + 1}`,
      outcome: index % 3 === 0 ? "denied" : "success",
      request_id: `request-${index + 1}`,
    }));
    const client = clientFor(async () => Response.json(entries));
    render(<ActivityPanel client={client} />);

    expect(await screen.findByText("actor-26@example.test")).toBeInTheDocument();
    expect(screen.queryByText("actor-01@example.test")).not.toBeInTheDocument();
    expect(screen.getByText("Page 1 of 2")).toBeInTheDocument();

    const actorSort = screen.getByRole("button", { name: "Actor" });
    fireEvent.click(actorSort);
    expect(actorSort.closest("th")).toHaveAttribute("aria-sort", "ascending");
    expect(screen.getByText("actor-01@example.test")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText(/Search actor, action, resource/i), { target: { value: "request-26" } });
    expect(screen.getByText("actor-26@example.test")).toBeInTheDocument();
    expect(screen.queryByText("actor-01@example.test")).not.toBeInTheDocument();
    expect(screen.getByText("Page 1 of 1")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Reset" }));
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    expect(screen.getByText("Page 2 of 2")).toBeInTheDocument();
  });

  it("exposes usable horizontal controls when the activity table overflows", async () => {
    const client = clientFor(async () => Response.json([{
      id: "event-wide",
      occurred_at_ms: 2_000,
      actor: "owner@example.test",
      action: "database.query",
      resource: "a-very-wide-resource-name-that-needs-horizontal-space",
      outcome: "success",
      request_id: "request-wide",
    } satisfies AuditLogEntry]));
    render(<ActivityPanel client={client} />);

    const table = await screen.findByRole("region", { name: "Activity records" });
    Object.defineProperties(table, {
      clientWidth: { configurable: true, value: 600 },
      scrollLeft: { configurable: true, value: 0, writable: true },
      scrollWidth: { configurable: true, value: 1_200 },
    });
    const scrollBy = vi.fn();
    Object.defineProperty(table, "scrollBy", { configurable: true, value: scrollBy });
    fireEvent.scroll(table);

    const next = await screen.findByRole("button", { name: "Scroll activity table right" });
    expect(next).toBeEnabled();
    expect(screen.getByRole("button", { name: "Scroll activity table left" })).toBeDisabled();
    fireEvent.click(next);
    expect(scrollBy).toHaveBeenCalledWith({ behavior: "smooth", left: 450 });
  });

  it("calculates the protocol checksum and applies a reviewed migration", async () => {
    const calls: Request[] = [];
    const client = clientFor(async (input, init) => {
      const request = new Request(input, init); calls.push(request);
      if (request.method === "GET" && request.url.endsWith("/migrations")) return Response.json([]);
      if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
      if (request.method === "POST" && request.url.endsWith("/migrations")) return Response.json({ status: "applied" });
      return Response.json({ error: { code: "missing", message: "missing", request_id: "request-1" } }, { status: 404 });
    });
    render(<MigrationsPanel client={client} />);

    fireEvent.click(screen.getByRole("tab", { name: /History/i }));
    await screen.findByText("No migrations yet");
    fireEvent.click(screen.getByRole("tab", { name: /New migration/i }));
    fireEvent.change(screen.getByLabelText(/Migration name/i), { target: { value: "Create example table" } });
    await waitFor(() => expect(screen.getByText(/^[a-f0-9]{64}$/u)).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Review migration" }));
    fireEvent.click(screen.getByRole("button", { name: "Apply migration" }));

    expect(await screen.findByText(/applied successfully/i)).toBeInTheDocument();
    const request = calls.find((call) => call.method === "POST" && call.url.endsWith("/migrations"));
    expect(request).toBeDefined();
    const body = await request?.json() as { readonly checksum?: string; readonly up_sql?: string; readonly down_sql?: string };
    expect(body.checksum).toMatch(/^[a-f0-9]{64}$/u);
    expect(body.up_sql).toContain("CREATE TABLE example");
    expect(body.down_sql).toContain("DROP TABLE example");
    expect(request?.headers.get("idempotency-key")).toContain(body.checksum);
  });

  it("offers controls to reach migration history columns when the table overflows", async () => {
    const client = clientFor(async (input, init) => {
      const request = new Request(input, init);
      if (request.url.endsWith("/schema")) return Response.json({ version: 1, tables: [] });
      if (request.url.endsWith("/migrations")) return Response.json([{
        id: "20260804023127",
        name: "Create customer profiles",
        status: "applied",
        checksum: "e0d405809b000000000000000000000000000000000000000000000000000000",
        schema_version_before: 0,
        schema_version_after: 1,
        applied_at_ms: 1_775_444_000_000,
      }]);
      return Response.json({ error: { code: "missing", message: "missing", request_id: "request-1" } }, { status: 404 });
    });

    render(<MigrationsPanel client={client} />);
    fireEvent.click(await screen.findByRole("tab", { name: /History/i }));
    const table = await screen.findByRole("region", { name: "Migration history records" });
    Object.defineProperties(table, {
      clientWidth: { configurable: true, value: 720 },
      scrollLeft: { configurable: true, value: 0, writable: true },
      scrollWidth: { configurable: true, value: 1_050 },
    });
    const scrollBy = vi.fn();
    Object.defineProperty(table, "scrollBy", { configurable: true, value: scrollBy });
    fireEvent.scroll(table);

    const next = await screen.findByRole("button", { name: "Scroll migration history right" });
    expect(next).toBeEnabled();
    expect(screen.getByRole("button", { name: "Scroll migration history left" })).toBeDisabled();
    fireEvent.click(next);
    expect(scrollBy).toHaveBeenCalledWith({ behavior: "smooth", left: 540 });
  });

  it("stages multiple table edits and deletions into one atomic transaction", async () => {
    const calls: Request[] = [];
    const client = clientFor(async (input, init) => {
      const request = new Request(input, init); calls.push(request);
      if (request.method === "GET" && request.url.endsWith("/schema")) return Response.json({ version: 3, tables: [{ name: "documents", sql: "CREATE TABLE documents (id TEXT PRIMARY KEY, title TEXT NOT NULL, views INTEGER NOT NULL)", rls_enabled: true, rls_forced: true }] });
      if (request.method === "GET" && request.url.endsWith("/migrations")) return Response.json([]);
      if (request.url.endsWith("/query")) return Response.json({ columns: [{ name: "id", type: "text" }, { name: "title", type: "text" }, { name: "views", type: "integer" }], rows: [["doc-1", "First", 1], ["doc-2", "Second", 2]], affected_rows: 0, last_insert_rowid: null, truncated: false });
      if (request.url.endsWith("/transaction")) return Response.json([]);
      return Response.json({ error: { code: "missing", message: "missing", request_id: "request-1" } }, { status: 404 });
    });

    render(<DatabasePanel client={client} />);
    fireEvent.change(await screen.findByRole("textbox", { name: "Edit title row 1" }), { target: { value: "First updated" } });
    await waitFor(
      () => expect(screen.getByRole("textbox", { name: "Edit title row 1" }).closest("tr")).toHaveClass("is-dirty"),
      { timeout: 3_000 },
    );
    expect(screen.getByRole("toolbar", { name: "Pending table changes" }).closest(".ffdb-data-toolbar")).not.toBeNull();
    fireEvent.click(screen.getByRole("checkbox", { name: "Select row 2" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete selected" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm 1" }));
    await waitFor(
      () => expect(screen.getByRole("checkbox", { name: "Select row 2" }).closest("tr")).toHaveClass("is-pending-delete"),
      { timeout: 3_000 },
    );
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));

    const transaction = await waitFor(() => {
      const call = calls.find((request) => request.url.endsWith("/transaction"));
      expect(call).toBeDefined();
      return call!;
    });
    const body = await transaction.json() as { readonly statements: readonly { readonly sql: string; readonly parameters: readonly unknown[] }[] };
    expect(body.statements).toHaveLength(2);
    expect(body.statements[0]?.sql).toContain("UPDATE \"documents\"");
    expect(body.statements[0]?.parameters).toHaveLength(2);
    expect(body.statements[1]?.sql).toContain("DELETE FROM \"documents\"");
  });
});

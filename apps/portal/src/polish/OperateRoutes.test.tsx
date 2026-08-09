import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FFDBClient, MemorySessionStore, type AuthTokenPair } from "@ffdb/client";

import { BackupsPanel, EmailPanel, PoliciesPanel, StoragePanel } from "./OperateRoutes.js";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("polished project operate routes", () => {
  it("opens complete policy details and generates an executable table-specific SQL draft", async () => {
    const onEdit = vi.fn();
    const client = testClient(async (request) => {
      if (request.url.endsWith("/policies")) return Response.json([{ name: "documents_owner", table: "documents", kind: "permissive", command: "update", roles: ["authenticated"], using_expression: "owner_id = auth.uid()", check_expression: "owner_id = auth.uid()", enabled: true, forced: true }]);
      return Response.json({});
    });

    render(<PoliciesPanel client={client} onEdit={onEdit} />);
    expect(screen.queryByLabelText("Policy command template")).not.toBeInTheDocument();
    const inventoryTab = await screen.findByRole("tab", { name: "Inventory" });
    fireEvent.keyDown(inventoryTab, { key: "ArrowRight" });
    expect(screen.getByRole("tab", { name: "New policy" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByLabelText("Policy command template")).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("tab", { name: "New policy" }), { key: "ArrowLeft" });
    fireEvent.click(await screen.findByRole("button", { name: "View details" }));
    const dialog = screen.getByRole("dialog", { name: "documents_owner" });
    expect(within(dialog).getAllByText("owner_id = auth.uid()", { selector: "code" })).toHaveLength(2);
    fireEvent.click(within(dialog).getByRole("button", { name: "Open SQL draft" }));

    expect(onEdit).toHaveBeenCalledOnce();
    const draft = String(onEdit.mock.calls[0]?.[0]);
    expect(draft).toContain('ALTER TABLE "documents" ENABLE ROW LEVEL SECURITY');
    expect(draft).toContain('ON "documents"');
    expect(draft).not.toContain("table_name");
    expect(draft).not.toContain("policy_name");

    fireEvent.click(screen.getByRole("tab", { name: "New policy" }));
    fireEvent.change(screen.getByLabelText("Policy command template"), { target: { value: "delete" } });
    fireEvent.click(screen.getByRole("button", { name: "Open SQL draft" }));
    expect(onEdit).toHaveBeenCalledTimes(2);
    expect(String(onEdit.mock.calls[1]?.[0])).toContain("FOR DELETE");
  });

  it("gates object operations behind an application-user session without leaking the raw auth error", async () => {
    const calls: Request[] = [];
    const client = testClient(async (request) => {
      calls.push(request);
      if (request.url.endsWith("/storage/buckets")) return Response.json([bucket()]);
      throw new Error(`Unexpected request ${request.url}`);
    });
    const onManageSession = vi.fn();

    render(<StoragePanel client={client} onManageSession={onManageSession} />);
    fireEvent.click(await screen.findByRole("button", { name: "Browse objects in documents" }));

    expect(await screen.findByRole("heading", { name: "Sign in as an application user to manage objects" })).toBeInTheDocument();
    expect(screen.queryByText(/auth\.session_missing|user session required/iu)).not.toBeInTheDocument();
    expect(calls.some((request) => request.url.includes("/storage/objects"))).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Open Auth" }));
    expect(onManageSession).toHaveBeenCalledOnce();
  });

  it("recovers from an expired object session with the same purposeful sign-in state", async () => {
    const sessionStore = new MemorySessionStore("operate-expired-user");
    await sessionStore.set(userSession());
    const client = testClient(async (request) => {
      if (request.url.endsWith("/storage/buckets")) return Response.json([bucket()]);
      if (request.url.includes("/storage/objects") || request.url.endsWith("/auth/refresh")) return errorResponse("auth.session_missing", "User session required", 401);
      return Response.json({});
    }, sessionStore);

    render(<StoragePanel client={client} onManageSession={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Browse objects in documents" }));

    expect(await screen.findByRole("heading", { name: "Sign in as an application user to manage objects" })).toBeInTheDocument();
    expect(screen.queryByText(/auth\.session_missing|user session required/iu)).not.toBeInTheDocument();
  });

  it("creates buckets, cleans reservations, and keeps server object pagination reversible", async () => {
    const sessionStore = new MemorySessionStore("operate-storage-actions");
    await sessionStore.set(userSession());
    const calls: Request[] = [];
    let buckets = [bucket()];
    const client = testClient(async (request) => {
      calls.push(request);
      if (request.url.endsWith("/storage/cleanup")) return Response.json({ removed: 2, retried: 1 });
      if (request.url.endsWith("/storage/buckets") && request.method === "POST") { const created = { ...bucket(), id: "bucket-2", name: "media" }; buckets = [...buckets, created]; return Response.json(created); }
      if (request.url.endsWith("/storage/buckets")) return Response.json(buckets);
      if (request.url.includes("/storage/objects")) {
        const cursor = new URL(request.url).searchParams.get("cursor");
        return Response.json(cursor === "page-2" ? { items: [objectItem("second/page.txt")], next_cursor: null } : { items: [objectItem("first/page.txt")], next_cursor: "page-2" });
      }
      return Response.json({});
    }, sessionStore);

    render(<StoragePanel client={client} />);
    expect(screen.queryByRole("button", { name: "Run cleanup" })).not.toBeInTheDocument();
    fireEvent.click(await screen.findByRole("tab", { name: "Maintenance" }));
    fireEvent.click(screen.getByRole("button", { name: "Run cleanup" }));
    expect(await screen.findByText("Reservation cleanup completed")).toBeInTheDocument();
    expect(screen.getByText("2 removed · 1 retried")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "New bucket" }));
    fireEvent.change(screen.getByPlaceholderText("user-uploads"), { target: { value: "media" } });
    fireEvent.click(screen.getByRole("button", { name: "Create bucket" }));
    expect(await screen.findByText("Bucket media created")).toBeInTheDocument();

    expect(await screen.findByText("first/page.txt")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next 100" }));
    expect(await screen.findByText("second/page.txt")).toBeInTheDocument();
    expect(screen.getByText("Server page 2 · end of results")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Previous 100" }));
    expect(await screen.findByText("first/page.txt")).toBeInTheDocument();
    expect(calls.filter((request) => request.url.includes("/storage/objects")).length).toBeGreaterThanOrEqual(3);
  });

  it("renders a server email preview and publishes the selected validated version", async () => {
    const calls: Request[] = [];
    const client = testClient(async (request) => {
      calls.push(request);
      if (request.url.includes("/email/templates/verification/2/preview")) return Response.json({ subject: "Verify your email", html: "<h1>Verify</h1>", text: "Verify" });
      if (request.url.includes("/email/templates/verification/2/publish")) return Response.json({ ...template(), published_at_ms: 20 });
      if (request.url.endsWith("/email/templates")) return Response.json([template()]);
      return Response.json({});
    });
    vi.spyOn(globalThis, "confirm").mockReturnValue(true);

    render(<EmailPanel client={client} />);
    await screen.findByText("Verification");
    expect(screen.queryByLabelText("Preview variables JSON")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open Verification version 2" }));
    fireEvent.change(screen.getByLabelText("Preview variables JSON"), { target: { value: '{"verification_url":"https://example.test/verify"}' } });
    fireEvent.click(screen.getByRole("button", { name: "Render preview" }));

    expect(await screen.findByRole("heading", { name: "Rendered preview" })).toBeInTheDocument();
    expect(screen.getByTitle("Rendered email HTML")).toHaveAttribute("sandbox", "");
    fireEvent.click(screen.getByRole("button", { name: "Publish version…" }));
    await waitFor(() => expect(calls.some((request) => request.url.includes("/publish") && request.method === "POST")).toBe(true));
    expect(await screen.findByText("Verification version 2 published")).toBeInTheDocument();
  });

  it("requires exact typed confirmation before a backup restore and reports the verified result", async () => {
    const calls: Request[] = [];
    const client = testClient(async (request) => {
      calls.push(request);
      if (request.url.endsWith("/backups/backup-production-001/restore")) return Response.json({ backup_id: "backup-production-001", integrity_ok: true, schema_version: 8 });
      if (request.url.endsWith("/backups")) return Response.json([backup()]);
      return Response.json({});
    });

    render(<BackupsPanel client={client} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restore…" }));
    const dialog = screen.getByRole("dialog", { name: "Restore project database?" });
    const confirmButton = within(dialog).getByRole("button", { name: "Restore and replace data" });
    expect(confirmButton).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText("Type the backup ID to confirm"), { target: { value: "backup-production-001" } });
    expect(confirmButton).toBeEnabled();
    fireEvent.click(confirmButton);

    await waitFor(() => expect(calls.some((request) => request.url.endsWith("/restore") && request.method === "POST")).toBe(true));
    expect(await screen.findByText("Restore verified")).toBeInTheDocument();
    expect(screen.getByText(/schema version 8/iu)).toBeInTheDocument();
  });

  it("runs backup creation and live database integrity as visible operations", async () => {
    const calls: Request[] = [];
    const client = testClient(async (request) => {
      calls.push(request);
      if (request.url.endsWith("/integrity-check")) return Response.json({ ok: true, messages: ["ok"] });
      if (request.url.endsWith("/backups") && request.method === "POST") return Response.json({ backup_id: "backup-new", size_bytes: 1_048_576, sha256: "abcdef0123456789abcdef0123456789" });
      if (request.url.endsWith("/backups")) return Response.json([]);
      return Response.json({});
    });

    render(<BackupsPanel client={client} />);
    fireEvent.click(await screen.findByRole("tab", { name: "Integrity" }));
    fireEvent.click(screen.getByRole("button", { name: "Run integrity check" }));
    expect(await screen.findByRole("heading", { name: "Live database integrity passed" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Request backup" }));
    expect(await screen.findByText("Backup requested")).toBeInTheDocument();
    expect(calls.some((request) => request.url.endsWith("/integrity-check") && request.method === "POST")).toBe(true);
    expect(calls.some((request) => request.url.endsWith("/backups") && request.method === "POST")).toBe(true);
  });
});

function testClient(fetcher: (request: Request) => Promise<Response>, sessionStore = new MemorySessionStore(`operate-${Math.random()}`)): FFDBClient {
  return new FFDBClient({
    baseUrl: "https://ffdb.example.test",
    projectId: "project-1",
    developerKey: "ffdb_dev_test.secret",
    sessionStore,
    fetch: async (input, init) => fetcher(new Request(input, init)),
  });
}

function bucket() {
  return { id: "bucket-1", name: "documents", public: false, max_object_bytes: 26_214_400, project_quota_bytes: 1_073_741_824, versioning: true, created_at_ms: 1_750_000_000_000 };
}

function template() {
  return { kind: "verification", version: 2, source: "source", source_sha256: "abcdef0123456789abcdef0123456789", subject_template: "Verify", html_template: "<h1>Verify</h1>", text_template: "Verify", allowed_variables: ["verification_url"], artifact_status: "validated", compilation_errors: [], compiled_at_ms: 1_750_000_000_000, published_at_ms: null };
}

function backup() {
  return { id: "backup-production-001", project_id: "project-1", status: "complete", size_bytes: 4_194_304, sha256: "abcdef0123456789abcdef0123456789", created_at_ms: 1_750_000_000_000, completed_at_ms: 1_750_000_010_000, last_restore_test_ms: null };
}

function objectItem(objectKey: string) {
  return { id: `object-${objectKey}`, object_key: objectKey, owner_id: "user-1", size_bytes: 128, content_type: "text/plain", checksum_sha256: null, etag: "etag-1", version_id: null, created_at_ms: 1_750_000_000_000, updated_at_ms: 1_750_000_000_000 };
}

function userSession(): AuthTokenPair {
  return { access_token: "expired-user-access", refresh_token: "expired-user-refresh", token_type: "Bearer", expires_in_seconds: 900, session_id: "session-1", user: { id: "user-1", email: "reader@example.test", email_verified: true, disabled: false, role: "authenticated", custom_claims: {}, created_at_ms: 1 } };
}

function errorResponse(code: string, message: string, status: number): Response {
  return Response.json({ error: { code, message, request_id: "request-1" } }, { status });
}

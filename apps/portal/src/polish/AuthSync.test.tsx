import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FFDBClient, MemorySessionStore, type AuthTokenPair, type AuthUser } from "@ffdb/client";

import { AuthRoute, SyncRoute } from "./AuthSync.js";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("polished Auth and Sync routes", () => {
  it("gates sync calls behind a purposeful signed-out state", async () => {
    const calls: Request[] = [];
    const client = testClient(async (request) => { calls.push(request); return Response.json({}); });
    const onManageSession = vi.fn();

    render(<SyncRoute client={client} onManageSession={onManageSession} />);

    expect(await screen.findByRole("heading", { name: "Sign in as an application user first" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Fetch snapshot" })).not.toBeInTheDocument();
    expect(screen.queryByText(/auth\.session_missing/i)).not.toBeInTheDocument();
    expect(calls).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Sign in on Auth" }));
    expect(onManageSession).toHaveBeenCalledOnce();
  });

  it("fetches a snapshot only after an end-user session is available", async () => {
    const sessionStore = new MemorySessionStore("polished-sync-session");
    await sessionStore.set(userSession());
    const calls: Request[] = [];
    const client = testClient(async (request) => {
      calls.push(request);
      if (request.url.endsWith("/snapshot")) return Response.json({ schema_version: 4, cursor: "opaque-cursor", tables: { documents: { columns: [{ name: "id", type: "text" }], rows: [["doc-1"]], affected_rows: 0, last_insert_rowid: null, truncated: false } } });
      return Response.json({});
    }, sessionStore);

    render(<SyncRoute client={client} onManageSession={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Fetch snapshot" }));

    expect(await screen.findByRole("heading", { name: "1 table at schema v4" })).toBeInTheDocument();
    expect(screen.getByText("documents")).toBeInTheDocument();
    expect(calls.find((request) => request.url.endsWith("/snapshot"))?.headers.get("authorization")).toBe("Bearer user-access");
  });

  it("turns a server-side missing-session response into a recovery flow without exposing the raw code", async () => {
    const sessionStore = new MemorySessionStore("polished-expired-sync-session");
    await sessionStore.set(userSession());
    const client = testClient(async () => Response.json({
      error: { code: "auth.session_missing", message: "User session required", request_id: "expired-session-request" },
    }, { status: 401 }), sessionStore);

    render(<SyncRoute client={client} onManageSession={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Fetch snapshot" }));

    expect(await screen.findByRole("heading", { name: "Sign in as an application user first" })).toBeInTheDocument();
    expect(screen.getByText(/session ended.*sign in again/i)).toBeInTheDocument();
    expect(screen.queryByText(/auth\.session_missing/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/User session required/i)).not.toBeInTheDocument();
  });

  it("uses compact switches and searchable, paginated auth user management", async () => {
    const users = Array.from({ length: 12 }, (_, index): AuthUser => ({
      id: `user-${index + 1}`,
      email: index === 11 ? "target@example.test" : `member-${String(index + 1).padStart(2, "0")}@example.test`,
      email_verified: index % 3 !== 0,
      disabled: index === 3,
      role: index === 0 ? "admin" : "authenticated",
      custom_claims: {},
      created_at_ms: index + 1,
    }));
    const calls: Request[] = [];
    const client = testClient(async (request) => {
      calls.push(request);
      if (request.url.endsWith("/auth/settings") && request.method === "PATCH") return Response.json(await request.json());
      if (request.url.endsWith("/auth/settings")) return Response.json({ registration_enabled: true, email_verification_required: true, access_token_ttl_seconds: 900, refresh_token_ttl_seconds: 2_592_000, password_min_length: 12 });
      if (request.url.endsWith("/auth/users")) return Response.json(users);
      return new Response(null, { status: 204 });
    });

    render(<AuthRoute client={client} />);

    const usersTab = await screen.findByRole("tab", { name: /Users 12/i });
    const policyTab = screen.getByRole("tab", { name: "Policy" });
    fireEvent.keyDown(usersTab, { key: "ArrowRight" });
    expect(policyTab).toHaveAttribute("aria-selected", "true");
    const registration = await screen.findByRole("switch", { name: "Allow new registrations" });
    expect(registration).toHaveAttribute("aria-checked", "true");
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    fireEvent.click(registration);
    fireEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await waitFor(() => expect(calls.some((request) => request.url.endsWith("/auth/settings") && request.method === "PATCH")).toBe(true));

    fireEvent.click(screen.getByRole("tab", { name: /Users 12/i }));
    const usersCard = screen.getByRole("heading", { name: "Project auth users" }).closest("section");
    expect(usersCard).not.toBeNull();
    expect(within(usersCard!).getByText("Page 1 of 2")).toBeInTheDocument();
    fireEvent.change(within(usersCard!).getByPlaceholderText("Search email or role"), { target: { value: "target" } });
    expect(await within(usersCard!).findByText("target@example.test")).toBeInTheDocument();
    expect(within(usersCard!).queryByText("member-01@example.test")).not.toBeInTheDocument();
    fireEvent.click(within(usersCard!).getByRole("button", { name: "Test" }));
    const tester = screen.getByRole("dialog", { name: "Test an end-user session" });
    expect(within(tester).getByRole("textbox", { name: "Email" })).toHaveValue("target@example.test");
    expect(screen.queryByRole("button", { name: "Test session" })).not.toBeInTheDocument();
  });
});

function testClient(fetcher: (request: Request) => Promise<Response>, sessionStore = new MemorySessionStore(`polish-${Math.random()}`)): FFDBClient {
  return new FFDBClient({
    baseUrl: "https://ffdb.example.test",
    projectId: "project-1",
    developerKey: "ffdb_dev_test.secret",
    sessionStore,
    fetch: async (input, init) => fetcher(new Request(input, init)),
  });
}

function userSession(): AuthTokenPair {
  return {
    access_token: "user-access",
    refresh_token: "user-refresh",
    token_type: "Bearer",
    expires_in_seconds: 900,
    session_id: "session-1",
    user: {
      id: "user-1",
      email: "reader@example.test",
      email_verified: true,
      disabled: false,
      role: "authenticated",
      custom_claims: {},
      created_at_ms: 1,
    },
  };
}

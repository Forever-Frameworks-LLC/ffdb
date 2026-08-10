import { afterEach, describe, expect, it, vi } from "vitest";

import { FFDBClient, MemoryDeveloperSessionStore, MemorySessionStore, type AuthTokenPair } from "./index.js";

const session: AuthTokenPair = {
  access_token: "access-old",
  refresh_token: "refresh-old",
  token_type: "Bearer",
  expires_in_seconds: 900,
  session_id: "session-1",
  user: {
    id: "user-1",
    email: "sam@example.test",
    email_verified: true,
    disabled: false,
    role: "authenticated",
    custom_claims: {},
    created_at_ms: 1,
  },
};

describe("FFDBClient", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });
  it("rejects base URL credentials, query strings, and fragments while preserving a base path", async () => {
    for (const baseUrl of [
      "https://user:secret@ffdb.example.test",
      "https://ffdb.example.test?credential=secret",
      "https://ffdb.example.test#operator-secret",
    ]) {
      expect(() => new FFDBClient({ baseUrl })).toThrow(TypeError);
    }
    let requested = "";
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test/control/",
      fetch: async (input) => { requested = String(input); return Response.json({ status: "ok" }); },
    });
    await client.health();
    expect(requested).toBe("https://ffdb.example.test/control/healthz");
  });
  it("refreshes once after an expired access token and retries the query", async () => {
    const calls: Request[] = [];
    const fetchMock: typeof fetch = async (input, init) => {
      const request = new Request(input, init);
      calls.push(request);
      if (request.url.endsWith("/auth/refresh")) {
        return Response.json({ ...session, access_token: "access-new", refresh_token: "refresh-new" });
      }
      if (request.headers.get("authorization") === "Bearer access-old") {
        return Response.json(
          { error: { code: "auth.expired_credential", message: "expired", request_id: "request-1" } },
          { status: 401 },
        );
      }
      return Response.json({
        columns: [{ name: "value", type: "integer" }],
        rows: [[42]],
        affected_rows: 0,
        last_insert_rowid: null,
        truncated: false,
      });
    };
    const store = new MemorySessionStore("refresh-test");
    await store.set(session);
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      fetch: fetchMock,
      sessionStore: store,
    });

    const result = await client.query({ sql: "select ?1", parameters: [{ type: "integer", value: 42 }] });
    expect(result.rows).toEqual([[42]]);
    expect(calls).toHaveLength(3);
    expect(calls[2]?.headers.get("authorization")).toBe("Bearer access-new");
  });

  it("uses developer credentials for schema operations", async () => {
    let authorization: string | null = null;
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test/",
      projectId: "project-1",
      developerKey: "ffdb_dev_prefix.secret",
      fetch: async (input, init) => {
        const request = new Request(input, init);
        authorization = request.headers.get("authorization");
        return Response.json({ version: 1, tables: [] });
      },
    });
    await client.schema();
    expect(authorization).toBe("Bearer ffdb_dev_prefix.secret");
  });

  it("unwraps the tagged worker protocol without leaking transport envelopes", async () => {
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      developerKey: "ffdb_dev_prefix.secret",
      fetch: async () => Response.json({
        type: "schema",
        payload: { version: 7, tables: [] },
      }),
    });
    await expect(client.schema()).resolves.toEqual({ version: 7, tables: [] });
  });

  it("accepts an empty body from any successful endpoint", async () => {
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      fetch: async () => new Response(null, { status: 202 }),
    });

    await expect(client.auth.startPasswordReset("sam@example.test")).resolves.toBeUndefined();
  });

  it("carries browser auth actions back to a server-validated app URL", async () => {
    const bodies: Record<string, unknown>[] = [];
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      fetch: async (input, init) => {
        bodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
        const path = new URL(String(input)).pathname;
        if (path.endsWith("/auth/register")) return Response.json({ user_id: "user-1", verification_required: true });
        if (path.endsWith("/auth/password/reset")) return new Response(null, { status: 202 });
        return Response.json({ redirect_to: "https://app.example.test/auth/complete" });
      },
    });

    await client.auth.register({ email: "sam@example.test", password: "long-enough-password", redirect_to: "https://app.example.test/sign-in?source=register" });
    await client.auth.startPasswordReset("sam@example.test", { redirectTo: "https://app.example.test/sign-in?source=reset" });
    await client.auth.verifyEmail("synthetic-token", { redirectTo: "https://app.example.test/auth/complete" });
    await client.auth.completePasswordReset("synthetic-token", "replacement-password", { redirectTo: "https://app.example.test/auth/complete" });

    expect(bodies[0]?.redirect_to).toBe("https://app.example.test/sign-in?source=register");
    expect(bodies[1]?.redirect_to).toBe("https://app.example.test/sign-in?source=reset");
    expect(bodies[2]?.redirect_to).toBe("https://app.example.test/auth/complete");
    expect(bodies[3]?.redirect_to).toBe("https://app.example.test/auth/complete");
  });

  it("uses a rotating platform session for organization management", async () => {
    const authorizations: (string | null)[] = [];
    const developerSessions = new MemoryDeveloperSessionStore("platform-test");
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      developerSessionStore: developerSessions,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        authorizations.push(request.headers.get("authorization"));
        if (request.url.endsWith("/v1/developer/sign-in")) return Response.json({ session_token: "platform-session", user_id: "user-1", email: "dev@example.test", expires_at_ms: 99_999 });
        return Response.json([]);
      },
    });
    await client.developerSignIn("dev@example.test", "correct horse battery staple");
    await client.organizations();
    expect(authorizations).toEqual([null, "Bearer platform-session"]);
  });

  it("requests retained project and instance observability with platform authorization", async () => {
    const calls: Request[] = [];
    const developerSessions = new MemoryDeveloperSessionStore("observability-test");
    await developerSessions.set({ session_token: "platform-session", user_id: "user-1", email: "dev@example.test", expires_at_ms: 99_999 });
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      developerSessionStore: developerSessions,
      fetch: async (input, init) => {
        calls.push(new Request(input, init));
        return Response.json({ scope: "project", totals: { requests: 0 }, series: [] });
      },
    });

    await client.projectObservability("6h");
    await client.instanceObservability("7d", "project-2");

    expect(calls.map((call) => call.url)).toEqual([
      "https://ffdb.example.test/v1/projects/project-1/observability?range=6h",
      "https://ffdb.example.test/v1/instance/observability?range=7d&project_id=project-2",
    ]);
    expect(calls.every((call) => call.headers.get("authorization") === "Bearer platform-session")).toBe(true);
  });

  it("uses only fixed host update routes and exact version bodies", async () => {
    const calls: Request[] = [];
    const developerSessions = new MemoryDeveloperSessionStore("host-update-test");
    await developerSessions.set({
      session_token: "recent-owner-session",
      user_id: "owner-1",
      email: "owner@example.test",
      expires_at_ms: 99_999,
    });
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      developerSessionStore: developerSessions,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        return Response.json({
          job_id: "0191439c-37c4-70a1-8d88-1a81f5c0f461",
          operation: "install",
          requested_version: "0.3.3",
          state: "queued",
          phase: "queued",
          installed_version: "0.3.2",
          available_version: "0.3.3",
          previous_version: "0.3.2",
          backup_path: null,
          message: "Queued",
          error_code: null,
          retryable: false,
          created_at_ms: 1,
          updated_at_ms: 1,
        });
      },
    });

    await client.checkForHostUpdate();
    await client.installHostUpdate("0.3.3");
    await client.rollbackHostUpdate("0.3.2");
    await client.hostUpdateJob("0191439c-37c4-70a1-8d88-1a81f5c0f461");

    expect(calls.map((call) => [call.method, new URL(call.url).pathname])).toEqual([
      ["POST", "/v1/instance/updates/check"],
      ["POST", "/v1/instance/updates/install"],
      ["POST", "/v1/instance/updates/rollback"],
      ["GET", "/v1/instance/updates/jobs/0191439c-37c4-70a1-8d88-1a81f5c0f461"],
    ]);
    await expect(calls[1]?.clone().json()).resolves.toEqual({ version: "0.3.3" });
    await expect(calls[2]?.clone().json()).resolves.toEqual({ version: "0.3.2" });
    expect(calls.every((call) => call.headers.get("authorization") === "Bearer recent-owner-session")).toBe(true);
  });

  it("clears the local developer session when remote sign-out rejects an expired credential", async () => {
    const developerSessions = new MemoryDeveloperSessionStore("failed-signout-test");
    await developerSessions.set({
      session_token: "expired-platform-session",
      user_id: "user-1",
      email: "dev@example.test",
      expires_at_ms: 99_999,
    });
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      developerSessionStore: developerSessions,
      fetch: async () => Response.json(
        { error: { code: "auth.invalid_credential", message: "credential is invalid", request_id: "request-1" } },
        { status: 401 },
      ),
    });

    await expect(client.developerSignOut()).rejects.toThrow("credential is invalid");
    await expect(developerSessions.get()).resolves.toBeNull();
    await expect(client.developerSession()).resolves.toBeNull();
  });

  it("bootstraps and persists the instance owner before configuring the installation", async () => {
    const calls: Request[] = [];
    const developerSessions = new MemoryDeveloperSessionStore("instance-bootstrap-test");
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      developerSessionStore: developerSessions,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/v1/developer/bootstrap")) {
          return Response.json({
            session_token: "owner-session",
            user_id: "owner-1",
            email: "owner@example.test",
            expires_at_ms: 99_999,
          });
        }
        return Response.json({
          instance: {
            owner_user_id: "owner-1",
            current_user_role: "owner",
            deployment_mode: "private",
            organization_creation_policy: "owner_only",
            billing_enforcement_enabled: false,
            setup_completed_at_ms: 1,
            billing_account: null,
            administrator_count: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
          },
          onboarding: null,
        });
      },
    });

    await client.developerBootstrap(
      "bootstrap-secret",
      "owner@example.test",
      "correct horse battery staple",
    );
    await client.configureInstance({
      deployment_mode: "private",
      organization_creation_policy: "owner_only",
    });

    expect(calls[0]?.headers.get("x-ffdb-bootstrap-token")).toBe("bootstrap-secret");
    expect(calls[1]?.headers.get("authorization")).toBe("Bearer owner-session");
    expect(calls[1]?.headers.get("idempotency-key")).toMatch(/^instance-configure:/);
    await expect(developerSessions.get()).resolves.toMatchObject({ session_token: "owner-session" });
  });

  it("sends owner-supplied Connect credentials only in the authenticated setup request", async () => {
    const calls: Request[] = [];
    const developerSessions = new MemoryDeveloperSessionStore("instance-connect-test");
    await developerSessions.set({
      session_token: "owner-session",
      user_id: "owner-1",
      email: "owner@example.test",
      expires_at_ms: 99_999,
    });
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      developerSessionStore: developerSessions,
      fetch: async (input, init) => {
        calls.push(new Request(input, init));
        return Response.json({ instance: {}, onboarding: { url: "https://connect.stripe.test", expires_at_ms: 99_999 } });
      },
    });

    const response = await client.configureInstance({
      deployment_mode: "platform_connect",
      organization_creation_policy: "owner_only",
      secret_key: "sk_test_connect_platform",
      webhook_secret: "whsec_connect_platform",
      country: "US",
      email: "owner@example.test",
      return_url: "https://portal.example.test/instance",
      refresh_url: "https://portal.example.test/instance?retry=1",
    });

    expect(calls).toHaveLength(1);
    expect(calls[0]?.headers.get("authorization")).toBe("Bearer owner-session");
    expect(calls[0]?.headers.get("idempotency-key")).toMatch(/^instance-configure:/);
    await expect(calls[0]?.clone().json()).resolves.toMatchObject({
      secret_key: "sk_test_connect_platform",
      webhook_secret: "whsec_connect_platform",
    });
    expect(JSON.stringify(response)).not.toContain("sk_test_connect_platform");
    expect(JSON.stringify(response)).not.toContain("whsec_connect_platform");
    await expect(developerSessions.get()).resolves.not.toMatchObject({
      secret_key: expect.anything(),
      webhook_secret: expect.anything(),
    });
  });

  it("commits provider-verified storage metadata after a signed upload", async () => {
    const calls: Request[] = [];
    const store = new MemorySessionStore("storage-commit-test");
    await store.set(session);
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      sessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/storage/sign")) return Response.json({
          url: "https://objects.example.test/ffdb/object?signature=redacted",
          method: "PUT",
          headers: [
            ["content-length", "4"],
            ["content-type", "text/plain"],
          ],
          expires_at_ms: 10_000,
          authorization_token: "opaque-grant",
        });
        if (request.url.startsWith("https://objects.example.test/")) return new Response(null, { status: 200 });
        if (request.url.endsWith("/storage/commit")) return new Response(null, { status: 204 });
        return Response.json({}, { status: 500 });
      },
    });

    await client.storage.upload("documents", "report.txt", "body", {
      sizeBytes: 4,
      contentType: "text/plain",
    });

    expect(calls.map((call) => new URL(call.url).pathname)).toEqual([
      "/v1/projects/project-1/storage/sign",
      "/ffdb/object",
      "/v1/projects/project-1/storage/commit",
    ]);
    expect(calls[1]?.headers.get("content-type")).toBe("text/plain");
    expect(calls[2]?.headers.get("authorization")).toBe("Bearer access-old");
    await expect(calls[2]?.json()).resolves.toEqual({ authorization_token: "opaque-grant" });
  });

  it("releases a durable storage reservation when the provider rejects upload", async () => {
    const paths: string[] = [];
    const store = new MemorySessionStore("storage-release-test");
    await store.set(session);
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      sessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        paths.push(new URL(request.url).pathname);
        if (request.url.endsWith("/storage/sign")) return Response.json({
          url: "https://objects.example.test/ffdb/object?signature=redacted",
          method: "PUT",
          headers: [],
          expires_at_ms: 10_000,
          authorization_token: "opaque-grant",
        });
        if (request.url.startsWith("https://objects.example.test/")) return new Response(null, { status: 503 });
        if (request.url.endsWith("/storage/release")) return new Response(null, { status: 204 });
        return Response.json({}, { status: 500 });
      },
    });

    await expect(client.storage.upload("documents", "report.txt", "body", {
      sizeBytes: 4,
      contentType: "text/plain",
    })).rejects.toMatchObject({ code: "storage.upload_failed" });
    expect(paths).toEqual([
      "/v1/projects/project-1/storage/sign",
      "/ffdb/object",
      "/v1/projects/project-1/storage/release",
    ]);
  });

  it("retries metadata precommit failure without repeating the provider upload or releasing", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    const paths: string[] = [];
    let providerUploads = 0;
    let commits = 0;
    const store = new MemorySessionStore("storage-precommit-retry-test");
    await store.set(session);
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      sessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        paths.push(new URL(request.url).pathname);
        if (request.url.endsWith("/storage/sign")) return Response.json({
          url: "https://objects.example.test/ffdb/object",
          method: "PUT",
          headers: [],
          expires_at_ms: 10_000,
          authorization_token: "replayable-grant",
        });
        if (request.url.startsWith("https://objects.example.test/")) {
          providerUploads += 1;
          return new Response(null, { status: 200 });
        }
        if (request.url.endsWith("/storage/commit")) {
          commits += 1;
          return commits === 1
            ? new Response(null, { status: 503 })
            : new Response(null, { status: 204 });
        }
        return Response.json({}, { status: 500 });
      },
    });

    const upload = client.storage.upload("documents", "report.txt", "body", {
      sizeBytes: 4,
      contentType: "text/plain",
    });
    await vi.advanceTimersByTimeAsync(500);
    await expect(upload).resolves.toBeUndefined();
    expect(providerUploads).toBe(1);
    expect(commits).toBe(2);
    expect(paths).not.toContain("/v1/projects/project-1/storage/release");
  });

  it("replays the same metadata commit after response loss", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    const commitBodies: unknown[] = [];
    let providerUploads = 0;
    const store = new MemorySessionStore("storage-response-loss-test");
    await store.set(session);
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      sessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        if (request.url.endsWith("/storage/sign")) return Response.json({
          url: "https://objects.example.test/ffdb/object",
          method: "PUT",
          headers: [],
          expires_at_ms: 10_000,
          authorization_token: "response-loss-grant",
        });
        if (request.url.startsWith("https://objects.example.test/")) {
          providerUploads += 1;
          return new Response(null, { status: 200 });
        }
        if (request.url.endsWith("/storage/commit")) {
          commitBodies.push(await request.clone().json());
          if (commitBodies.length === 1) throw new TypeError("connection closed after commit");
          return new Response(null, { status: 204 });
        }
        return Response.json({}, { status: 500 });
      },
    });

    const upload = client.storage.upload("documents", "report.txt", "body", {
      sizeBytes: 4,
      contentType: "text/plain",
    });
    await vi.advanceTimersByTimeAsync(500);
    await expect(upload).resolves.toBeUndefined();
    expect(providerUploads).toBe(1);
    expect(commitBodies).toEqual([
      { authorization_token: "response-loss-grant" },
      { authorization_token: "response-loss-grant" },
    ]);
  });

  it("runs a multipart upload through signed provider requests and verified commits", async () => {
    const calls: Request[] = [];
    const signPayloads: unknown[] = [];
    const commitPayloads: unknown[] = [];
    let completionXml = "";
    const store = new MemorySessionStore("storage-multipart-test");
    await store.set(session);
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      sessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/storage/multipart/authorize")) {
          return Response.json({ authorization_token: "grant-create_multipart" });
        }
        if (request.url.endsWith("/storage/multipart/create")) {
          return Response.json({ upload_id: "upload&id" }, { status: 201 });
        }
        if (request.url.endsWith("/storage/sign")) {
          const payload = await request.clone().json() as { operation: string };
          signPayloads.push(payload);
          const methods: Readonly<Record<string, string>> = {
            upload_part: "PUT",
            complete_multipart: "POST",
          };
          return Response.json({
            url: `https://objects.example.test/ffdb/object?action=${payload.operation}`,
            method: methods[payload.operation],
            headers: payload.operation === "upload_part" ? [["content-length", "4"]] : [],
            expires_at_ms: 10_000,
            authorization_token: `grant-${payload.operation}`,
          });
        }
        if (request.url.endsWith("/storage/multipart/commit")) {
          commitPayloads.push(await request.clone().json());
          return new Response(null, { status: 204 });
        }
        const action = new URL(request.url).searchParams.get("action");
        if (action === "upload_part") {
          return new Response(null, { status: 200, headers: { etag: '"part-etag"' } });
        }
        if (action === "complete_multipart") {
          completionXml = await request.text();
          return new Response("<CompleteMultipartUploadResult />", { status: 200 });
        }
        return Response.json({}, { status: 500 });
      },
    });

    const upload = await client.storage.createMultipart("documents", "large.bin", {
      sizeBytes: 4,
      contentType: "application/octet-stream",
    });
    const part = await client.storage.uploadPart(upload, 1, "body", { sizeBytes: 4 });
    await client.storage.completeMultipart(upload, [part], {
      sizeBytes: 4,
      contentType: "application/octet-stream",
    });

    expect(upload.uploadId).toBe("upload&id");
    expect(part).toEqual({ partNumber: 1, etag: '"part-etag"' });
    expect(signPayloads).toMatchObject([
      { operation: "upload_part", upload_id: "upload&id", part_number: 1, size_bytes: 4 },
      { operation: "complete_multipart", upload_id: "upload&id", size_bytes: 4 },
    ]);
    expect(commitPayloads).toEqual([
      { authorization_token: "grant-upload_part", operation: "upload_part", upload_id: null, etag: '"part-etag"' },
      { authorization_token: "grant-complete_multipart", operation: "complete", upload_id: null, etag: null },
    ]);
    expect(completionXml).toBe(
      "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>&quot;part-etag&quot;</ETag></Part></CompleteMultipartUpload>",
    );
    expect(calls).toHaveLength(8);
  });

  it("uses the exact DELETE session route and supports runtime project selection", async () => {
    const calls: Request[] = [];
    const store = new MemorySessionStore("session-revoke-route-test");
    await store.set(session);
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      sessionStore: store,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        return new Response(null, { status: 204 });
      },
    });
    client.setProjectId("project-runtime");
    await client.auth.revokeSession("session/encoded");
    expect(new URL(calls[0]?.url ?? "").pathname).toBe(
      "/v1/projects/project-runtime/auth/sessions/session%2Fencoded",
    );
    expect(calls[0]?.method).toBe("DELETE");
  });

  it("aligns membership, auth-user, and email artifact routes", async () => {
    const calls: Request[] = [];
    const developerSessions = new MemoryDeveloperSessionStore("management-route-test");
    await developerSessions.set({
      session_token: "platform-session",
      user_id: "platform-user",
      email: "owner@example.test",
      expires_at_ms: 999_999,
    });
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      developerKey: "ffdb_dev_prefix.secret",
      developerSessionStore: developerSessions,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.method === "GET") return Response.json([]);
        if (request.method === "DELETE" || request.url.includes("/auth/users/")) {
          return new Response(null, { status: 204 });
        }
        return Response.json({});
      },
    });
    await client.organizationMembers("org-1");
    await client.removeOrganizationMember("org-1", "user-1");
    await client.authUsers();
    await client.setAuthUserDisabled("auth-user-1", true);
    await client.importEmailTemplateArtifact({
      kind: "verification",
      version: 2,
      source: "source",
      source_sha256: "0".repeat(64),
      subject_template: "Verify",
      html_template: "<p>Verify</p>",
      text_template: "Verify",
      allowed_variables: [],
    });
    await client.publishEmailTemplate("verification", 2);
    expect(calls.map((call) => `${call.method} ${new URL(call.url).pathname}`)).toEqual([
      "GET /v1/organizations/org-1/members",
      "DELETE /v1/organizations/org-1/members/user-1",
      "GET /v1/projects/project-1/auth/users",
      "PATCH /v1/projects/project-1/auth/users/auth-user-1",
      "POST /v1/projects/project-1/email/templates/artifacts",
      "POST /v1/projects/project-1/email/templates/verification/2/publish",
    ]);
  });

  it("uses isolated platform-billing and project-commerce routes", async () => {
    const calls: Request[] = [];
    const developerSessions = new MemoryDeveloperSessionStore("billing-route-test");
    await developerSessions.set({
      session_token: "platform-session",
      user_id: "platform-user",
      email: "owner@example.test",
      expires_at_ms: 999_999,
    });
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      developerSessionStore: developerSessions,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        calls.push(request);
        if (request.url.endsWith("/commerce/account")) {
          return Response.json({
            project_id: "project-1",
            mode: "bring_your_own_keys",
            status: "enabled",
            livemode: false,
            provider_account_id: "acct_test",
            capabilities: {
              one_time_payments: true,
              recurring_payments: true,
              refunds: true,
              customer_portal: true,
            },
            requirements_due: [],
            disabled_reason: null,
            webhook_url: "https://ffdb.example.test/v1/projects/project-1/commerce/webhooks/stripe",
            secrets_configured: true,
          });
        }
        if (request.method === "GET") {
          return Response.json({
            organization_id: "org-1",
            tier: "free",
            status: "free",
            billing_unit: "organization",
            seat_quantity: 1,
            project_limit: 2,
            usage_allowance: {
              storage_bytes: 1_000_000_000,
              monthly_reads: 1_000_000,
              monthly_writes: 50_000,
              monthly_active_users: 5_000,
              overage_enabled: false,
            },
            current_period_end_ms: null,
            cancel_at_period_end: false,
            provider_configured: true,
          });
        }
        return Response.json({ url: "https://checkout.stripe.com/test" }, { status: 201 });
      },
    });

    await client.organizationBilling("org/1");
    await client.organizationUsage("org/1");
    await client.organizationInvoices("org/1");
    await client.createBillingCheckout("org/1", { tier: "pro" }, { idempotencyKey: "checkout-key-1" });
    await client.createBillingPortal("org/1", { idempotencyKey: "portal-key-1" });
    await client.commerce.account();

    expect(calls.map((call) => `${call.method} ${new URL(call.url).pathname}`)).toEqual([
      "GET /v1/organizations/org%2F1/billing",
      "GET /v1/organizations/org%2F1/billing/usage",
      "GET /v1/organizations/org%2F1/billing/invoices",
      "POST /v1/organizations/org%2F1/billing/checkout",
      "POST /v1/organizations/org%2F1/billing/portal",
      "GET /v1/projects/project-1/commerce/account",
    ]);
    expect(calls.every((call) => call.headers.get("authorization") === "Bearer platform-session")).toBe(true);
    expect(calls[3]?.headers.get("idempotency-key")).toBe("checkout-key-1");
    expect(calls[4]?.headers.get("idempotency-key")).toBe("portal-key-1");
    await expect(calls[3]?.clone().json()).resolves.toEqual({ tier: "pro" });
  });

  it("exposes typed project-commerce mutations with durable idempotency", async () => {
    const calls: Request[] = [];
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project/1",
      developerKey: "ffdb_dev_commerce.secret",
      fetch: async (input, init) => {
        calls.push(new Request(input, init));
        return Response.json({});
      },
    });

    await client.commerce.disconnectAccount({ idempotencyKey: "disconnect-key" });
    await client.commerce.products();
    await client.commerce.createProduct(
      { name: "Pro", description: null, tax_code: null },
      { idempotencyKey: "product-key" },
    );
    await client.commerce.createPrice({
      product_id: "product-1",
      lookup_key: "pro_monthly",
      currency: "USD",
      unit_amount_minor: 1_500,
      billing: { type: "recurring", interval: "month", interval_count: 1 },
      entitlements: { projects: { type: "quantity", value: 10 } },
    }, { idempotencyKey: "price-key" });
    await client.commerce.oneTimeCheckout({
      lines: [{ price_id: "price-1", quantity: 2 }],
      subject: null,
      customer_email: "buyer@example.test",
      client_reference: "cart-1",
      success_url: "https://shop.example.test/success",
      cancel_url: "https://shop.example.test/cart",
    }, { idempotencyKey: "checkout-key" });
    await client.commerce.customerPortal({
      subject: { kind: "individual", id: "user-1" },
      return_url: "https://shop.example.test/account",
    }, { idempotencyKey: "customer-portal-key" });
    await client.commerce.updateFulfillment(
      "order/1",
      "fulfilled",
      "tracking-1",
      { idempotencyKey: "fulfillment-key" },
    );
    await client.commerce.refund(
      { payment_id: "payment-1", amount_minor: 500, reason: "requested_by_customer" },
      { idempotencyKey: "refund-key" },
    );
    await client.commerce.cancelSubscription(
      "subscription/1",
      { at_period_end: true },
      { idempotencyKey: "cancel-key" },
    );

    expect(calls.map((call) => `${call.method} ${new URL(call.url).pathname}`)).toEqual([
      "DELETE /v1/projects/project%2F1/commerce/account",
      "GET /v1/projects/project%2F1/commerce/products",
      "POST /v1/projects/project%2F1/commerce/products",
      "POST /v1/projects/project%2F1/commerce/prices",
      "POST /v1/projects/project%2F1/commerce/checkouts/one-time",
      "POST /v1/projects/project%2F1/commerce/customer-portal",
      "PATCH /v1/projects/project%2F1/commerce/orders/order%2F1/fulfillment",
      "POST /v1/projects/project%2F1/commerce/refunds",
      "POST /v1/projects/project%2F1/commerce/subscriptions/subscription%2F1/cancel",
    ]);
    expect(calls[1]?.headers.get("authorization")).toBeNull();
    expect(calls.filter((_, index) => index !== 1).every((call) => call.headers.get("authorization") === "Bearer ffdb_dev_commerce.secret")).toBe(true);
    expect(calls.filter((_, index) => index !== 1).map((call) => call.headers.get("idempotency-key"))).toEqual([
      "disconnect-key",
      "product-key",
      "price-key",
      "checkout-key",
      "customer-portal-key",
      "fulfillment-key",
      "refund-key",
      "cancel-key",
    ]);
  });

  it("honors Retry-After for safe reads with bounded retries", async () => {
    vi.useFakeTimers();
    let attempts = 0;
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      fetch: async () => {
        attempts += 1;
        if (attempts < 3) return new Response(null, { status: 503, headers: { "Retry-After": "1" } });
        return Response.json({ status: "ok" });
      },
    });
    const result = client.health();
    await vi.advanceTimersByTimeAsync(2_000);
    await expect(result).resolves.toEqual({ status: "ok" });
    expect(attempts).toBe(3);
  });

  it("retries keyed mutations but never retries unkeyed mutations", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    let keyedAttempts = 0;
    const keyed = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      developerKey: "ffdb_dev_prefix.secret",
      fetch: async () => {
        keyedAttempts += 1;
        return keyedAttempts === 1
          ? new Response(null, { status: 504 })
          : Response.json({ type: "migration", payload: { id: "migration-1" } });
      },
    });
    const migration = keyed.migrate({
      id: "migration-1",
      name: "create todos",
      up_sql: "CREATE TABLE todos(id TEXT PRIMARY KEY)",
      down_sql: "DROP TABLE todos",
      checksum: "abc",
      created_at_ms: 1,
    }, { idempotencyKey: "migration-1:abc" });
    await vi.advanceTimersByTimeAsync(100);
    await expect(migration).resolves.toEqual({ id: "migration-1" });
    expect(keyedAttempts).toBe(2);

    let unkeyedAttempts = 0;
    const unkeyed = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      developerKey: "ffdb_dev_prefix.secret",
      fetch: async () => {
        unkeyedAttempts += 1;
        return Response.json(
          { error: { code: "service.unavailable", message: "unavailable", request_id: "r-1" } },
          { status: 503 },
        );
      },
    });
    await expect(unkeyed.request("/v1/projects/project-1/seed", {
      method: "POST",
      body: JSON.stringify({ sql: "UPDATE notes SET value = 'unknown outcome'" }),
      credential: "developer",
    }))
      .rejects.toMatchObject({ status: 503 });
    expect(unkeyedAttempts).toBe(1);
  });

  it("uses a fresh secure lifecycle key per rollback and restore invocation", async () => {
    const keys: string[] = [];
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      projectId: "project-1",
      developerKey: "ffdb_dev_prefix.secret",
      fetch: async (input, init) => {
        const request = new Request(input, init);
        keys.push(request.headers.get("idempotency-key") ?? "");
        if (request.url.includes("/rollback")) {
          return Response.json({ type: "migration", payload: { status: "rolled_back" } });
        }
        return Response.json({
          type: "restore",
          payload: { backup_id: "backup-1", integrity_ok: true, schema_version: 7 },
        });
      },
    });

    await client.rollbackMigration("migration-1");
    await client.rollbackMigration("migration-1");
    await expect(client.restoreBackup("backup-1")).resolves.toEqual({
      backup_id: "backup-1",
      integrity_ok: true,
      schema_version: 7,
    });
    await client.restoreBackup("backup-1");

    expect(keys[0]).toMatch(/^migration-rollback:/u);
    expect(keys[1]).toMatch(/^migration-rollback:/u);
    expect(keys[2]).toMatch(/^backup-restore:/u);
    expect(keys[3]).toMatch(/^backup-restore:/u);
    expect(new Set(keys).size).toBe(4);
  });

  it("cancels a pending retry without issuing another request", async () => {
    vi.useFakeTimers();
    let attempts = 0;
    const controller = new AbortController();
    const client = new FFDBClient({
      baseUrl: "https://ffdb.example.test",
      fetch: async () => {
        attempts += 1;
        return new Response(null, { status: 503, headers: { "Retry-After": "5" } });
      },
    });
    const request = client.health({ signal: controller.signal });
    await vi.advanceTimersByTimeAsync(0);
    controller.abort();
    await expect(request).rejects.toMatchObject({ name: "AbortError" });
    expect(attempts).toBe(1);
  });
});

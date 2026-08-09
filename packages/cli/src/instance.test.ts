import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { FFDBClient, MemoryDeveloperSessionStore } from "@ffdb/client";

import { executeInstanceCommand, instanceConfiguration } from "./instance.js";

const temporaryDirectories: string[] = [];

describe("instance operator commands", () => {
  afterEach(async () => {
    await Promise.all(temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })));
  });

  it("keeps public setup discovery unauthenticated and protects instance status", async () => {
    const calls: Request[] = [];
    const client = await instanceClient(calls);
    await executeInstanceCommand(client, "setup-status", []);
    await executeInstanceCommand(client, "status", []);

    expect(calls.map((request) => new URL(request.url).pathname)).toEqual([
      "/v1/instance/setup/status",
      "/v1/instance",
    ]);
    expect(calls[0]?.headers.get("authorization")).toBeNull();
    expect(calls[1]?.headers.get("authorization")).toBe("Bearer owner-session");
  });

  it("configures every deployment mode while reading provider secrets only from the environment", async () => {
    const calls: Request[] = [];
    const client = await instanceClient(calls);
    const byoEnvironment = {
      FFDB_INSTANCE_STRIPE_SECRET_KEY: "sk_test_instance_private",
      FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET: "whsec_instance_private",
      FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY: "sk_test_connect_platform",
      FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET: "whsec_connect_platform",
    };

    const results = [
      await executeInstanceCommand(client, "setup", ["private", "owner_only"]),
      await executeInstanceCommand(client, "configure", ["team", "invitation_only"]),
      await executeInstanceCommand(client, "setup", ["byo", "authenticated"], byoEnvironment),
      await executeInstanceCommand(client, "configure", ["connect", "owner_only", "us", "owner@example.test", "https://portal.example.test/app", "https://portal.example.test/app?retry=1"], byoEnvironment),
    ];

    const bodies = await Promise.all(calls.map((request) => request.clone().json()));
    expect(bodies).toEqual([
      { deployment_mode: "private", organization_creation_policy: "owner_only" },
      { deployment_mode: "team", organization_creation_policy: "invitation_only" },
      { deployment_mode: "platform_byo", organization_creation_policy: "authenticated", secret_key: "sk_test_instance_private", webhook_secret: "whsec_instance_private" },
      { deployment_mode: "platform_connect", organization_creation_policy: "owner_only", secret_key: "sk_test_connect_platform", webhook_secret: "whsec_connect_platform", country: "US", email: "owner@example.test", return_url: "https://portal.example.test/app", refresh_url: "https://portal.example.test/app?retry=1" },
    ]);
    expect(JSON.stringify(results)).not.toContain("sk_test_instance_private");
    expect(JSON.stringify(results)).not.toContain("whsec_instance_private");
    expect(JSON.stringify(results)).not.toContain("sk_test_connect_platform");
    expect(JSON.stringify(results)).not.toContain("whsec_connect_platform");
    expect(() => instanceConfiguration(["byo", "owner_only"], {})).toThrow("FFDB_INSTANCE_STRIPE_SECRET_KEY is required");
  });

  it("operates Connect, global inventory, admins, exemptions, and plans with guarded destructive actions", async () => {
    const calls: Request[] = [];
    const client = await instanceClient(calls);
    const directory = await mkdtemp(join(tmpdir(), "@ffdb/cli-instance-"));
    temporaryDirectories.push(directory);
    const exemptionPath = join(directory, "exemption.json");
    const planPath = join(directory, "plan.json");
    await writeFile(exemptionPath, JSON.stringify({ reason: "Operator-owned organization" }));
    await writeFile(planPath, JSON.stringify({
      display_name: "Pro",
      billing_unit: "organization",
      base_price_cents: 4900,
      currency: "usd",
      project_limit: null,
      storage_bytes: 100_000_000_000,
      monthly_reads: 100_000_000,
      monthly_writes: 10_000_000,
      monthly_active_users: 100_000,
      overage_enabled: true,
      reads_at_limit: "overage",
      writes_at_limit: "overage",
      signups_at_limit: "overage",
      requires_payment_method_for_overage: true,
      active: true,
    }));

    await executeInstanceCommand(client, "connect", ["onboarding", "https://portal.example.test/app?connect=return", "https://portal.example.test/app?connect=refresh"]);
    await executeInstanceCommand(client, "connect", ["refresh"]);
    await executeInstanceCommand(client, "policy", ["set", "authenticated"]);
    await executeInstanceCommand(client, "admins", ["list"]);
    await executeInstanceCommand(client, "admins", ["grant", "user-2"]);
    await executeInstanceCommand(client, "admins", ["revoke", "user-2", "--yes"]);
    await executeInstanceCommand(client, "user-disable", ["user-2", "--yes"]);
    await executeInstanceCommand(client, "user-enable", ["user-2", "--yes"]);
    await executeInstanceCommand(client, "org-disable", ["org-1", "--yes"]);
    await executeInstanceCommand(client, "org-enable", ["org-1", "--yes"]);
    await executeInstanceCommand(client, "organizations", ["50", "100"]);
    await executeInstanceCommand(client, "users", ["25", "0"]);
    await executeInstanceCommand(client, "exemptions", ["list"]);
    await executeInstanceCommand(client, "exemptions", ["grant", "org-1", exemptionPath]);
    await executeInstanceCommand(client, "exemptions", ["revoke", "org-1", "--yes"]);
    await executeInstanceCommand(client, "plans", ["list"]);
    await executeInstanceCommand(client, "plans", ["put", "pro", planPath]);
    await executeInstanceCommand(client, "plans", ["retire", "pro", "--yes"]);

    expect(calls.map((request) => `${request.method} ${new URL(request.url).pathname}${new URL(request.url).search}`)).toEqual([
      "POST /v1/instance/billing/connect/onboarding",
      "POST /v1/instance/billing/refresh",
      "PATCH /v1/instance/organization-creation-policy",
      "GET /v1/instance/administrators",
      "POST /v1/instance/administrators",
      "DELETE /v1/instance/administrators/user-2",
      "PATCH /v1/instance/users/user-2",
      "PATCH /v1/instance/users/user-2",
      "PATCH /v1/instance/organizations/org-1",
      "PATCH /v1/instance/organizations/org-1",
      "GET /v1/instance/organizations?limit=50&offset=100",
      "GET /v1/instance/users?limit=25&offset=0",
      "GET /v1/instance/billing-exemptions",
      "PUT /v1/instance/billing-exemptions/org-1",
      "DELETE /v1/instance/billing-exemptions/org-1",
      "GET /v1/instance/plans",
      "PUT /v1/instance/plans/pro",
      "DELETE /v1/instance/plans/pro",
    ]);
    await expect(calls[6]?.clone().json()).resolves.toEqual({ disabled: true });
    await expect(calls[7]?.clone().json()).resolves.toEqual({ disabled: false });
    await expect(calls[8]?.clone().json()).resolves.toEqual({ disabled: true });
    await expect(calls[9]?.clone().json()).resolves.toEqual({ disabled: false });
    await expect(calls[13]?.clone().json()).resolves.toEqual({ reason: "Operator-owned organization" });
    await expect(calls[16]?.clone().json()).resolves.toMatchObject({ display_name: "Pro", base_price_cents: 4900 });
  });

  it("requires explicit confirmation for instance revocations and retirement", async () => {
    const client = await instanceClient([]);
    await expect(executeInstanceCommand(client, "admins", ["revoke", "user-2"])).rejects.toThrow("pass --yes");
    await expect(executeInstanceCommand(client, "user-disable", ["user-2"])).rejects.toThrow("pass --yes");
    await expect(executeInstanceCommand(client, "user-enable", ["user-2"])).rejects.toThrow("pass --yes");
    await expect(executeInstanceCommand(client, "org-disable", ["org-1"])).rejects.toThrow("pass --yes");
    await expect(executeInstanceCommand(client, "org-enable", ["org-1"])).rejects.toThrow("pass --yes");
    await expect(executeInstanceCommand(client, "exemptions", ["revoke", "org-1"])).rejects.toThrow("pass --yes");
    await expect(executeInstanceCommand(client, "plans", ["retire", "pro"])).rejects.toThrow("pass --yes");
  });
});

async function instanceClient(calls: Request[]): Promise<FFDBClient> {
  const sessions = new MemoryDeveloperSessionStore("cli-instance-test");
  await sessions.set({
    session_token: "owner-session",
    user_id: "owner-1",
    email: "owner@example.test",
    expires_at_ms: Date.now() + 60_000,
  });
  return new FFDBClient({
    baseUrl: "https://ffdb.example.test",
    developerSessionStore: sessions,
    fetch: async (input, init) => {
      const request = new Request(input, init);
      calls.push(request);
      if (request.url.endsWith("/v1/instance/setup/status")) {
        return Response.json({ bootstrap_available: false, setup_required: false, platform_byo_available: true, platform_connect_available: true });
      }
      if (request.url.endsWith("/v1/instance") && request.method === "POST") {
        return Response.json({ instance: instanceStatus(), onboarding: null });
      }
      return Response.json(instanceStatus());
    },
  });
}

function instanceStatus() {
  return {
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
  };
}

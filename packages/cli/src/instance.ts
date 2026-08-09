import { readFile } from "node:fs/promises";

import {
  type CompleteInstanceSetupRequest,
  FFDBClient,
  type GrantOrganizationBillingExemptionRequest,
  type OrganizationCreationPolicy,
  type PlatformBillingTier,
  type PutInstancePlanCatalogEntryRequest,
} from "@ffdb/client";

import { required } from "./args.js";
import { confirmDestructive } from "./safety.js";

export type InstanceCommandEnvironment = Readonly<Record<string, string | undefined>>;

export async function executeInstanceCommand(
  client: FFDBClient,
  action: string | undefined,
  args: readonly string[],
  environment: InstanceCommandEnvironment = process.env,
): Promise<unknown> {
  if (action === "setup-status") return client.instanceSetupStatus();
  if (action === "status") return client.instanceStatus();
  if (action === "setup" || action === "configure") {
    return client.configureInstance(instanceConfiguration(args, environment));
  }
  if (action === "policy" && args[0] === "set") {
    return client.updateOrganizationCreationPolicy(parseOrganizationPolicy(args[1]));
  }
  if (action === "connect" && args[0] === "onboarding") {
    return client.createInstanceConnectOnboarding({
      return_url: parseUrl(args[1], "return URL"),
      refresh_url: parseUrl(args[2], "refresh URL"),
    });
  }
  if (action === "connect" && args[0] === "refresh") return client.refreshInstanceBilling();
  if (action === "admins" && args[0] === "list") return client.instanceAdministrators();
  if (action === "admins" && args[0] === "grant") {
    return client.grantInstanceAdministrator(required(args[1], "user id"));
  }
  if (action === "admins" && args[0] === "revoke") {
    const userId = required(args[1], "user id");
    await confirmDestructive(`Revoke instance administrator ${userId}`, args.includes("--yes"));
    return client.revokeInstanceAdministrator(userId);
  }
  if (action === "user-disable" || action === "user-enable") {
    const userId = required(args[0], "user id");
    const disabled = action === "user-disable";
    await confirmDestructive(`${disabled ? "Disable" : "Enable"} instance user ${userId}`, args.includes("--yes"));
    return client.setInstanceUserDisabled(userId, disabled);
  }
  if (action === "org-disable" || action === "org-enable") {
    const organizationId = required(args[0], "organization id");
    const disabled = action === "org-disable";
    await confirmDestructive(`${disabled ? "Disable" : "Enable"} instance organization ${organizationId}`, args.includes("--yes"));
    return client.setInstanceOrganizationDisabled(organizationId, disabled);
  }
  if (action === "organizations") {
    return client.instanceOrganizations(parsePage(args));
  }
  if (action === "users") {
    return client.instanceUsers(parsePage(args));
  }
  if (action === "exemptions" && args[0] === "list") return client.billingExemptions();
  if (action === "exemptions" && args[0] === "grant") {
    const organizationId = required(args[1], "organization id");
    const input = await readJson<GrantOrganizationBillingExemptionRequest>(args[2]);
    return client.grantBillingExemption(organizationId, required(input.reason, "billing exemption reason"));
  }
  if (action === "exemptions" && args[0] === "revoke") {
    const organizationId = required(args[1], "organization id");
    await confirmDestructive(`Revoke billing exemption for organization ${organizationId}`, args.includes("--yes"));
    return client.revokeBillingExemption(organizationId);
  }
  if (action === "plans" && args[0] === "list") return client.instancePlans();
  if (action === "plans" && args[0] === "put") {
    const tier = parseTier(args[1]);
    return client.putInstancePlan(tier, await readJson<PutInstancePlanCatalogEntryRequest>(args[2]));
  }
  if (action === "plans" && args[0] === "retire") {
    const tier = parseTier(args[1]);
    await confirmDestructive(`Retire instance plan ${tier}`, args.includes("--yes"));
    return client.retireInstancePlan(tier);
  }
  throw new Error(`Unknown instance command: ${[action, ...args].filter(Boolean).join(" ")}`);
}

export function instanceConfiguration(
  args: readonly string[],
  environment: InstanceCommandEnvironment = process.env,
): CompleteInstanceSetupRequest {
  const mode = required(args[0], "deployment mode");
  const organization_creation_policy = parseOrganizationPolicy(args[1]);
  if (mode === "private" || mode === "team") {
    return { deployment_mode: mode, organization_creation_policy };
  }
  if (mode === "byo" || mode === "platform_byo") {
    return {
      deployment_mode: "platform_byo",
      organization_creation_policy,
      secret_key: required(environment.FFDB_INSTANCE_STRIPE_SECRET_KEY, "FFDB_INSTANCE_STRIPE_SECRET_KEY"),
      webhook_secret: required(environment.FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET, "FFDB_INSTANCE_STRIPE_WEBHOOK_SECRET"),
    };
  }
  if (mode === "connect" || mode === "platform_connect") {
    return {
      deployment_mode: "platform_connect",
      organization_creation_policy,
      secret_key: required(
        environment.FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY,
        "FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY",
      ),
      webhook_secret: required(
        environment.FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET,
        "FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET",
      ),
      country: required(args[2], "Stripe account country").toUpperCase(),
      email: required(args[3], "Stripe account email"),
      return_url: parseUrl(args[4], "return URL"),
      refresh_url: parseUrl(args[5], "refresh URL"),
    };
  }
  throw new Error("deployment mode must be private, team, byo, or connect");
}

export function parseOrganizationPolicy(value: string | undefined): OrganizationCreationPolicy {
  const policy = required(value, "organization creation policy");
  if (policy !== "owner_only" && policy !== "authenticated" && policy !== "invitation_only") {
    throw new Error("organization creation policy must be owner_only, authenticated, or invitation_only");
  }
  return policy;
}

function parseTier(value: string | undefined): PlatformBillingTier {
  const tier = required(value, "plan tier");
  if (tier !== "free" && tier !== "pay_as_you_go" && tier !== "pro") {
    throw new Error("plan tier must be free, pay_as_you_go, or pro");
  }
  return tier;
}

function parsePage(args: readonly string[]): { readonly limit?: number; readonly offset?: number } {
  const limit = parseOptionalInteger(args[0], "limit", 1);
  const offset = parseOptionalInteger(args[1], "offset", 0);
  return {
    ...(limit === undefined ? {} : { limit }),
    ...(offset === undefined ? {} : { offset }),
  };
}

function parseOptionalInteger(value: string | undefined, label: string, minimum: number): number | undefined {
  if (value === undefined) return undefined;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < minimum) throw new Error(`${label} must be an integer of at least ${minimum}`);
  return parsed;
}

function parseUrl(value: string | undefined, label: string): string {
  const input = required(value, label);
  let url: URL;
  try { url = new URL(input); }
  catch { throw new Error(`${label} must be an absolute HTTP(S) URL`); }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error(`${label} must be an absolute HTTP(S) URL`);
  }
  return url.href;
}

async function readJson<T>(path: string | undefined): Promise<T> {
  return JSON.parse(await readFile(required(path, "JSON file path"), "utf8")) as T;
}

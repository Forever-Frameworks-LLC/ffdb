import { BrowserDeveloperSessionStore, BrowserSessionStore, FFDBClient } from "@ffdb/client";

const PORTAL_PROJECT_CREDENTIAL_TTL_MS = 12 * 60 * 60 * 1_000;
export const PORTAL_PROJECT_CREDENTIAL_REFRESH_LEAD_MS = 5 * 60 * 1_000;
const PORTAL_PROJECT_SCOPES = [
  "database_query",
  "database_migrate",
  "database_schema",
  "auth_manage",
  "storage_manage",
  "email_manage",
  "commerce_manage",
  "keys_rotate",
  "backups_manage",
  "logs_read",
] as const;

export interface PortalConfiguration {
  readonly apiUrl: string;
  readonly instanceName?: string | undefined;
  readonly organizationId?: string | undefined;
  readonly projectId: string;
  readonly developerKey: string | undefined;
  readonly developerKeyExpiresAtMs?: number | null | undefined;
  readonly developerKeyManaged?: boolean | undefined;
  readonly projectName: string;
  readonly organizationName: string;
}

export function portalConfiguration(environment: ImportMetaEnv = import.meta.env): PortalConfiguration {
  const apiUrl = environment.VITE_FFDB_API_URL
    ?? globalThis.localStorage?.getItem("ffdb.portal.active-instance-url")
    ?? globalThis.location?.origin
    ?? "http://127.0.0.1:8080";
  const namespace = portalInstanceNamespace(apiUrl);
  const projectId = globalThis.sessionStorage?.getItem(`${namespace}.active-project`)
    ?? globalThis.sessionStorage?.getItem("ffdb.portal.active-project")
    ?? environment.VITE_FFDB_PROJECT_ID
    ?? "";
  const storedKey = projectId === "" ? undefined : portalProjectKey(apiUrl, projectId);
  const storedKeyMetadata = projectId === "" ? undefined : portalProjectKeyMetadata(apiUrl, projectId);
  const hostname = globalThis.location?.hostname ?? "";
  const explicitLocalDevelopment = environment.VITE_FFDB_DEV_MODE === "true"
    && ["localhost", "127.0.0.1", "::1"].includes(hostname);
  const developerKey = storedKey
    ?? (explicitLocalDevelopment ? environment.VITE_FFDB_DEVELOPER_KEY : undefined);
  const projectName = globalThis.sessionStorage?.getItem(`${namespace}.active-project-name`)
    ?? environment.VITE_FFDB_PROJECT_NAME
    ?? (projectId || "Choose a project");
  const organizationId = globalThis.sessionStorage?.getItem(`${namespace}.active-organization-id`)
    ?? undefined;
  const organizationName = globalThis.sessionStorage?.getItem(`${namespace}.active-organization-name`)
    ?? globalThis.sessionStorage?.getItem("ffdb.portal.active-organization")
    ?? environment.VITE_FFDB_ORGANIZATION_NAME
    ?? "Choose an organization";
  const configuredInstanceName = globalThis.localStorage?.getItem(`${namespace}.name`)
    ?? environment.VITE_FFDB_INSTANCE_NAME;
  const instanceName = configuredInstanceName
    ?? (new URL(apiUrl).hostname === "127.0.0.1" || new URL(apiUrl).hostname === "localhost" ? "Local development" : new URL(apiUrl).host);
  persistPortalInstance({ apiUrl, instanceName });
  return {
    apiUrl,
    instanceName,
    organizationId,
    projectId,
    developerKey,
    ...(storedKeyMetadata === undefined ? {} : {
      developerKeyExpiresAtMs: storedKeyMetadata.expiresAtMs,
      developerKeyManaged: storedKeyMetadata.managed,
    }),
    projectName,
    organizationName,
  };
}

export function createPortalClient(configuration: PortalConfiguration): FFDBClient {
  return new FFDBClient({
    baseUrl: configuration.apiUrl,
    projectId: configuration.projectId,
    ...(configuration.developerKey === undefined ? {} : { developerKey: configuration.developerKey }),
    // Browser tokens remain in sessionStorage: any XSS can still access them,
    // so deployments must enforce CSP and avoid untrusted script execution.
    sessionStore: new BrowserSessionStore(globalThis.sessionStorage, `${portalInstanceNamespace(configuration.apiUrl)}.${configuration.projectId || "platform"}`),
    developerSessionStore: new BrowserDeveloperSessionStore(globalThis.sessionStorage, `${portalInstanceNamespace(configuration.apiUrl)}.developer`),
  });
}

/** Exchange the signed-in platform session for a short-lived, project-scoped
 * credential used only by this browser tab. Project API keys remain appropriate
 * for applications and automation; the portal must not require users to copy a
 * permanent secret onto every device. */
export interface IssuedPortalProjectCredential {
  readonly secret: string;
  readonly expiresAtMs: number;
  readonly managed: true;
}

export async function issuePortalProjectCredential(client: FFDBClient): Promise<IssuedPortalProjectCredential> {
  const session = await client.developerSession();
  if (session === null) throw new Error("Sign in again to open this project.");
  const now = Date.now();
  if (session.expires_at_ms <= now + PORTAL_PROJECT_CREDENTIAL_REFRESH_LEAD_MS) {
    throw new Error("Your account session is about to expire. Sign in again to keep this project open.");
  }
  const expiresAt = Math.min(session.expires_at_ms, now + PORTAL_PROJECT_CREDENTIAL_TTL_MS);
  const credential = await client.createApiKey({
    name: "portal-session",
    scopes: PORTAL_PROJECT_SCOPES,
    expires_at_ms: expiresAt,
  });
  return {
    secret: credential.secret,
    expiresAtMs: credential.expires_at_ms ?? expiresAt,
    managed: true,
  };
}

export function persistPortalProject(projectId: string, developerKey?: string, organizationName?: string, organizationId?: string, projectName?: string, apiUrl?: string): void {
  const namespace = portalInstanceNamespace(apiUrl ?? globalThis.localStorage?.getItem("ffdb.portal.active-instance-url") ?? globalThis.location.origin);
  globalThis.sessionStorage.setItem(`${namespace}.active-project`, projectId);
  if (organizationName !== undefined) {
    globalThis.sessionStorage.setItem(`${namespace}.active-organization-name`, organizationName);
  }
  if (organizationId !== undefined) {
    globalThis.sessionStorage.setItem(`${namespace}.active-organization-id`, organizationId);
  }
  if (projectName !== undefined) {
    globalThis.sessionStorage.setItem(`${namespace}.active-project-name`, projectName);
  }
  if (developerKey !== undefined) {
    globalThis.sessionStorage.setItem(`${namespace}.project-key.${projectId}`, developerKey);
  }
}

export function portalProjectKey(apiUrl: string, projectId: string): string | undefined {
  if (projectId === "") return undefined;
  const namespace = portalInstanceNamespace(apiUrl);
  return globalThis.sessionStorage?.getItem(`${namespace}.project-key.${projectId}`)
    ?? globalThis.sessionStorage?.getItem(`ffdb.portal.project-key.${projectId}`)
    ?? undefined;
}

export interface PortalProjectKeyMetadata {
  readonly expiresAtMs: number | null;
  readonly managed: boolean;
}

export function persistPortalProjectKeyMetadata(
  apiUrl: string,
  projectId: string,
  metadata: PortalProjectKeyMetadata,
): void {
  if (projectId === "") return;
  const namespace = portalInstanceNamespace(apiUrl);
  globalThis.sessionStorage?.setItem(
    `${namespace}.project-key-metadata.${projectId}`,
    JSON.stringify({ expires_at_ms: metadata.expiresAtMs, managed: metadata.managed }),
  );
}

export function portalProjectKeyMetadata(apiUrl: string, projectId: string): PortalProjectKeyMetadata | undefined {
  if (projectId === "") return undefined;
  const namespace = portalInstanceNamespace(apiUrl);
  const stored = globalThis.sessionStorage?.getItem(`${namespace}.project-key-metadata.${projectId}`);
  if (stored === null || stored === undefined) return undefined;
  try {
    const value = JSON.parse(stored) as { readonly expires_at_ms?: unknown; readonly managed?: unknown };
    if (typeof value.managed !== "boolean") return undefined;
    if (value.expires_at_ms !== null && typeof value.expires_at_ms !== "number") return undefined;
    return { expiresAtMs: value.expires_at_ms, managed: value.managed };
  } catch {
    globalThis.sessionStorage?.removeItem(`${namespace}.project-key-metadata.${projectId}`);
    return undefined;
  }
}

/** Remove only the browser-held credential for one project. Server-side keys are
 * revoked separately through the API so those two security actions cannot be
 * confused in the UI. */
export function clearPortalProjectKey(apiUrl: string, projectId: string): void {
  if (projectId === "") return;
  const namespace = portalInstanceNamespace(apiUrl);
  globalThis.sessionStorage?.removeItem(`${namespace}.project-key.${projectId}`);
  globalThis.sessionStorage?.removeItem(`${namespace}.project-key-metadata.${projectId}`);
  // Remove the pre-multi-instance compatibility entry as well, if present.
  globalThis.sessionStorage?.removeItem(`ffdb.portal.project-key.${projectId}`);
}

export interface PortalInstanceRecord {
  readonly apiUrl: string;
  readonly instanceName: string;
}

export function portalInstances(): readonly PortalInstanceRecord[] {
  try {
    const value = JSON.parse(globalThis.localStorage?.getItem("ffdb.portal.instances") ?? "[]") as unknown;
    if (!Array.isArray(value)) return [];
    return value.filter((item): item is PortalInstanceRecord => typeof item === "object" && item !== null
      && typeof (item as { apiUrl?: unknown }).apiUrl === "string"
      && typeof (item as { instanceName?: unknown }).instanceName === "string");
  } catch {
    return [];
  }
}

export function persistPortalInstance(instance: PortalInstanceRecord): void {
  const normalized = { apiUrl: instance.apiUrl.replace(/\/$/u, ""), instanceName: instance.instanceName.trim() };
  const next = [normalized, ...portalInstances().filter((item) => item.apiUrl !== normalized.apiUrl)];
  globalThis.localStorage?.setItem("ffdb.portal.instances", JSON.stringify(next));
  globalThis.localStorage?.setItem(`${portalInstanceNamespace(normalized.apiUrl)}.name`, normalized.instanceName);
}

/** Forget a saved instance and all of its browser-local sessions and project
 * credentials. This does not delete or mutate the remote FFDB installation. */
export function forgetPortalInstance(apiUrl: string): void {
  const normalizedUrl = apiUrl.replace(/\/$/u, "");
  const next = portalInstances().filter((item) => item.apiUrl !== normalizedUrl);
  globalThis.localStorage?.setItem("ffdb.portal.instances", JSON.stringify(next));
  const namespace = portalInstanceNamespace(normalizedUrl);
  globalThis.localStorage?.removeItem(`${namespace}.name`);
  for (let index = (globalThis.sessionStorage?.length ?? 0) - 1; index >= 0; index -= 1) {
    const key = globalThis.sessionStorage?.key(index);
    if (key?.startsWith(`${namespace}.`) === true) globalThis.sessionStorage?.removeItem(key);
  }
}

export function selectPortalInstance(instance: PortalInstanceRecord): PortalConfiguration {
  persistPortalInstance(instance);
  globalThis.localStorage?.setItem("ffdb.portal.active-instance-url", instance.apiUrl);
  return portalConfiguration({ ...import.meta.env, VITE_FFDB_API_URL: instance.apiUrl, VITE_FFDB_INSTANCE_NAME: instance.instanceName });
}

export function portalInstanceNamespace(apiUrl: string): string {
  return `ffdb.portal.instance.${encodeURIComponent(apiUrl.replace(/\/$/u, ""))}`;
}

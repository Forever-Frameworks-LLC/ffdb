import { chmod, mkdir, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

export interface CliCredentials {
  readonly baseUrl: string;
  readonly projectId?: string;
  readonly developerKey?: string;
  readonly developerSessionToken?: string;
  readonly developerEmail?: string;
  readonly developerUserId?: string;
  readonly developerSessionExpiresAtMs?: number;
}

export interface CredentialStore {
  load(): Promise<CliCredentials | null>;
  save(credentials: CliCredentials): Promise<void>;
  clear(): Promise<void>;
}

export function defaultCredentialPath(): string {
  return process.env.FFDB_CONFIG ?? join(homedir(), ".config", "ffdb", "credentials.json");
}

export class FileCredentialStore implements CredentialStore {
  constructor(readonly path = defaultCredentialPath()) {}

  async load(): Promise<CliCredentials | null> {
    try {
      const value = JSON.parse(await readFile(this.path, "utf8")) as Partial<CliCredentials>;
      if (typeof value.baseUrl !== "string") throw new Error("Credential file is invalid");
      return {
        baseUrl: value.baseUrl,
        ...(typeof value.projectId === "string" ? { projectId: value.projectId } : {}),
        ...(typeof value.developerKey === "string" ? { developerKey: value.developerKey } : {}),
        ...(typeof value.developerSessionToken === "string" ? { developerSessionToken: value.developerSessionToken } : {}),
        ...(typeof value.developerEmail === "string" ? { developerEmail: value.developerEmail } : {}),
        ...(typeof value.developerUserId === "string" ? { developerUserId: value.developerUserId } : {}),
        ...(typeof value.developerSessionExpiresAtMs === "number" ? { developerSessionExpiresAtMs: value.developerSessionExpiresAtMs } : {}),
      };
    } catch (cause) {
      if (isMissing(cause)) return null;
      throw cause;
    }
  }

  async save(credentials: CliCredentials): Promise<void> {
    const directory = dirname(this.path);
    await mkdir(directory, { recursive: true, mode: 0o700 });
    const temporary = `${this.path}.${Date.now()}.tmp`;
    await writeFile(temporary, `${JSON.stringify(credentials, null, 2)}\n`, { encoding: "utf8", mode: 0o600, flag: "wx" });
    await chmod(temporary, 0o600);
    try { await rename(temporary, this.path); }
    catch (cause) { await unlink(temporary).catch(() => undefined); throw cause; }
    await chmod(this.path, 0o600);
  }

  async clear(): Promise<void> {
    try { await unlink(this.path); }
    catch (cause) { if (!isMissing(cause)) throw cause; }
  }
}

function isMissing(cause: unknown): boolean {
  return cause instanceof Error && "code" in cause && cause.code === "ENOENT";
}

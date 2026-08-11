import type { AuthTokenPair } from "@ffdb/client";
import { FFDBClient } from "@ffdb/client";
import {
  NativeSQLiteReplica,
  ReactNativeSessionStore,
  type AsyncKeyValueStorage,
  type NativeSQLiteDriver,
  type SQLitePrimitive,
  type SQLiteResult,
} from "@ffdb/react-native";
import { OfflineSyncClient } from "@ffdb/sync-client";
import * as SecureStore from "expo-secure-store";
import * as SQLite from "expo-sqlite";
import { Platform } from "react-native";

const apiUrl = process.env.EXPO_PUBLIC_FFDB_API_URL?.trim() ?? "";
const projectId = process.env.EXPO_PUBLIC_FFDB_PROJECT_ID?.trim() ?? "";

export const nativeAuthRedirect = "ffdb-field-notes://auth/callback";
export const ffdbProjectId = projectId || "unconfigured";
export const configurationError = apiUrl === "" || projectId === ""
  ? "EXPO_PUBLIC_FFDB_API_URL and EXPO_PUBLIC_FFDB_PROJECT_ID are required."
  : null;

const volatileWebStorage = new Map<string, string>();
const secureStorage: AsyncKeyValueStorage = Platform.OS === "web"
  ? {
      getItem: async (key) => volatileWebStorage.get(key) ?? null,
      setItem: async (key, value) => { volatileWebStorage.set(key, value); },
      removeItem: async (key) => { volatileWebStorage.delete(key); },
    }
  : {
      getItem: (key) => SecureStore.getItemAsync(key),
      setItem: (key, value) => SecureStore.setItemAsync(key, value),
      removeItem: (key) => SecureStore.deleteItemAsync(key),
    };

export const ffdb = configurationError === null
  ? new FFDBClient({
      baseUrl: apiUrl,
      projectId,
      sessionStore: new ReactNativeSessionStore(
        secureStorage,
        `ffdb.field-notes.${projectId}`,
      ),
    })
  : null;

const syncClients = new Map<string, Promise<OfflineSyncClient>>();

export function nativeSyncClient(session: AuthTokenPair): Promise<OfflineSyncClient> {
  const key = `${projectId}:${session.user.id}`;
  const existing = syncClients.get(key);
  if (existing !== undefined) return existing;
  if (ffdb === null) return Promise.reject(new Error(configurationError ?? "FFDB is not configured"));

  const creating = SQLite.openDatabaseAsync(`ffdb-field-notes-v2-${session.user.id}.sqlite3`)
    .then(async (database) => {
      const replica = new NativeSQLiteReplica(new ExpoSQLiteDriver(database));
      await replica.initialize();
      return new OfflineSyncClient(ffdb, replica);
    })
    .catch((cause: unknown) => {
      syncClients.delete(key);
      throw cause;
    });
  syncClients.set(key, creating);
  return creating;
}

type SQLiteExecutor = Pick<SQLite.SQLiteDatabase, "prepareAsync">;

class ExpoSQLiteDriver implements NativeSQLiteDriver {
  constructor(
    private readonly executor: SQLiteExecutor,
    private readonly root: SQLite.SQLiteDatabase | null = executor instanceof SQLite.SQLiteDatabase
      ? executor
      : null,
  ) {}

  async execute<Row extends Readonly<Record<string, SQLitePrimitive>> = Readonly<Record<string, SQLitePrimitive>>>(
    sql: string,
    parameters: readonly SQLitePrimitive[] = [],
  ): Promise<SQLiteResult<Row>> {
    const statement = await this.executor.prepareAsync(sql);
    try {
      const result = await statement.executeAsync<Row>([...parameters]);
      return { rows: await result.getAllAsync(), changes: result.changes };
    } finally {
      await statement.finalizeAsync();
    }
  }

  async transaction<T>(work: (driver: NativeSQLiteDriver) => Promise<T>): Promise<T> {
    if (this.root === null) return work(this);
    let output: T | undefined;
    if (Platform.OS === "web") {
      await this.root.withTransactionAsync(async () => {
        output = await work(new ExpoSQLiteDriver(this.root!));
      });
    } else {
      await this.root.withExclusiveTransactionAsync(async (transaction) => {
        output = await work(new ExpoSQLiteDriver(transaction));
      });
    }
    return output as T;
  }
}

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  useSyncExternalStore,
  type PropsWithChildren,
} from "react";

import type {
  AuthTokenPair,
  FFDBClient,
  QueryRequest,
  QueryResult,
  RequestOptions,
  SessionSummary,
} from "@ffdb/client";
import type { OfflineSyncClient, SyncState } from "@ffdb/sync-client";

const FFDBContext = createContext<FFDBClient | null>(null);

export function FFDBProvider({ client, children }: PropsWithChildren<{ readonly client: FFDBClient }>) {
  return <FFDBContext.Provider value={client}>{children}</FFDBContext.Provider>;
}

export function useFFDB(): FFDBClient {
  const client = useContext(FFDBContext);
  if (client === null) throw new Error("useFFDB must be used within FFDBProvider");
  return client;
}

export interface AuthState {
  readonly status: "loading" | "authenticated" | "anonymous" | "error";
  readonly session: AuthTokenPair | null;
  readonly error: Error | null;
}

interface AuthContextValue extends AuthState {
  signIn(email: string, password: string, options?: RequestOptions): Promise<AuthTokenPair>;
  signOut(options?: RequestOptions): Promise<void>;
  refresh(signal?: AbortSignal): Promise<AuthTokenPair>;
}

const AuthContext = createContext<AuthContextValue | null>(null);

export function AuthProvider({ children }: PropsWithChildren) {
  const client = useFFDB();
  const [state, setState] = useState<AuthState>({ status: "loading", session: null, error: null });

  useEffect(() => {
    let active = true;
    void client.auth
      .session()
      .then((session) => {
        if (active) setState({ status: session === null ? "anonymous" : "authenticated", session, error: null });
      })
      .catch((cause: unknown) => {
        if (active) setState({ status: "error", session: null, error: toError(cause) });
      });
    return () => {
      active = false;
    };
  }, [client]);

  const signIn = useCallback(
    async (email: string, password: string, options: RequestOptions = {}) => {
      try {
        const session = await client.auth.signIn(email, password, options);
        setState({ status: "authenticated", session, error: null });
        return session;
      } catch (cause) {
        const error = toError(cause);
        setState({ status: "error", session: null, error });
        throw error;
      }
    },
    [client],
  );

  const signOut = useCallback(
    async (options: RequestOptions = {}) => {
      await client.auth.signOut(options);
      setState({ status: "anonymous", session: null, error: null });
    },
    [client],
  );

  const refresh = useCallback(
    async (signal?: AbortSignal) => {
      const session = await client.auth.refresh(signal);
      setState({ status: "authenticated", session, error: null });
      return session;
    },
    [client],
  );

  const value = useMemo(() => ({ ...state, signIn, signOut, refresh }), [state, signIn, signOut, refresh]);
  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const auth = useContext(AuthContext);
  if (auth === null) throw new Error("useAuth must be used within AuthProvider");
  return auth;
}

export interface QueryState<Row extends readonly unknown[]> {
  readonly status: "idle" | "loading" | "success" | "error";
  readonly data: QueryResult<Row & readonly (null | number | string | { readonly $blob: string })[]> | null;
  readonly error: Error | null;
  refetch(): Promise<void>;
}

export function useQuery<
  Row extends readonly (null | number | string | { readonly $blob: string })[] = readonly (
    | null
    | number
    | string
    | { readonly $blob: string }
  )[],
>(request: QueryRequest | null, dependencies: readonly unknown[] = []): QueryState<Row> {
  const client = useFFDB();
  const [revision, setRevision] = useState(0);
  const [state, setState] = useState<Omit<QueryState<Row>, "refetch">>({
    status: "idle",
    data: null,
    error: null,
  });
  const refetch = useCallback(async () => {
    setRevision((value) => value + 1);
  }, []);

  useEffect(() => {
    if (request === null) {
      setState({ status: "idle", data: null, error: null });
      return;
    }
    const controller = new AbortController();
    setState((current) => ({ ...current, status: "loading", error: null }));
    void client
      .query<Row>(request, { signal: controller.signal })
      .then((data) => setState({ status: "success", data, error: null }))
      .catch((cause: unknown) => {
        if (!controller.signal.aborted) setState({ status: "error", data: null, error: toError(cause) });
      });
    return () => controller.abort();
    // The caller owns dependency stability; request content is intentionally keyed by the dependency list.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, revision, ...dependencies]);

  return { ...state, refetch };
}

export function useSync(syncClient: OfflineSyncClient): SyncState & { sync(signal?: AbortSignal): Promise<void> } {
  const state = useSyncExternalStore(
    (listener) => syncClient.subscribe(listener),
    () => syncClient.state,
    () => syncClient.state,
  );
  return useMemo(() => ({ ...state, sync: (signal?: AbortSignal) => syncClient.sync(signal) }), [state, syncClient]);
}

export function useSessions(): QueryLikeState<readonly SessionSummary[]> {
  const client = useFFDB();
  return useAsyncValue((signal) => client.auth.sessions({ signal }), [client]);
}

export interface StorageUploadState {
  readonly status: "idle" | "uploading" | "success" | "error";
  readonly error: Error | null;
  upload(
    bucket: string,
    key: string,
    body: BodyInit,
    metadata: { readonly sizeBytes: number; readonly contentType: string; readonly checksumSha256?: string },
    options?: RequestOptions,
  ): Promise<void>;
}

export function useStorageUpload(): StorageUploadState {
  const client = useFFDB();
  const [state, setState] = useState<Omit<StorageUploadState, "upload">>({ status: "idle", error: null });
  const upload = useCallback<StorageUploadState["upload"]>(
    async (bucket, key, body, metadata, options = {}) => {
      setState({ status: "uploading", error: null });
      try {
        await client.storage.upload(bucket, key, body, metadata, options);
        setState({ status: "success", error: null });
      } catch (cause) {
        const error = toError(cause);
        setState({ status: "error", error });
        throw error;
      }
    },
    [client],
  );
  return { ...state, upload };
}

interface QueryLikeState<T> {
  readonly status: "loading" | "success" | "error";
  readonly data: T | null;
  readonly error: Error | null;
  refetch(): void;
}

function useAsyncValue<T>(loader: (signal: AbortSignal) => Promise<T>, dependencies: readonly unknown[]): QueryLikeState<T> {
  const [revision, setRevision] = useState(0);
  const [state, setState] = useState<Omit<QueryLikeState<T>, "refetch">>({
    status: "loading",
    data: null,
    error: null,
  });
  useEffect(() => {
    const controller = new AbortController();
    setState((current) => ({ ...current, status: "loading", error: null }));
    void loader(controller.signal)
      .then((data) => setState({ status: "success", data, error: null }))
      .catch((cause: unknown) => {
        if (!controller.signal.aborted) setState({ status: "error", data: null, error: toError(cause) });
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [revision, ...dependencies]);
  return { ...state, refetch: () => setRevision((value) => value + 1) };
}

export function optimisticList<T extends { readonly id: string }>(
  current: readonly T[],
  optimistic: T,
): { readonly next: readonly T[]; rollback(): readonly T[] } {
  const previous = current;
  const existing = current.findIndex((value) => value.id === optimistic.id);
  const next = existing < 0 ? [...current, optimistic] : current.map((value) => (value.id === optimistic.id ? optimistic : value));
  return { next, rollback: () => previous };
}

function toError(cause: unknown): Error {
  return cause instanceof Error ? cause : new Error("Unknown FFDB error");
}

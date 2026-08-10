import { BrowserSessionStore, FFDBClient } from "@ffdb/client";

const projectId = import.meta.env.VITE_FFDB_PROJECT_ID?.trim();

export const configurationError = projectId
  ? null
  : "VITE_FFDB_PROJECT_ID is missing. Copy .env.example to .env.local and add your project ID.";

export const ffdb = projectId
  ? new FFDBClient({
      baseUrl: import.meta.env.VITE_FFDB_API_URL?.trim() || globalThis.location.origin,
      projectId,
      sessionStore: new BrowserSessionStore(
        globalThis.sessionStorage,
        `ffdb.field-notes.${projectId}`,
      ),
    })
  : null;

export const ffdbProjectId = projectId ?? "unconfigured";

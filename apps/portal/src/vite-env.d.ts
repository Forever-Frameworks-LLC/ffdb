/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_FFDB_API_URL?: string;
  readonly VITE_FFDB_PROJECT_ID?: string;
  readonly VITE_FFDB_DEVELOPER_KEY?: string;
  readonly VITE_FFDB_PROJECT_NAME?: string;
  readonly VITE_FFDB_ORGANIZATION_NAME?: string;
}

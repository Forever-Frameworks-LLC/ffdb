import { FFDBClient, type QueryResult } from "@ffdb/client";

/**
 * Compile-time counterpart to the public landing examples.
 * This file is included by the app TypeScript project but is not imported by the bundle.
 */
export function createLandingExampleClient(): FFDBClient {
  return new FFDBClient({
    baseUrl: "https://data.example.com",
    projectId: "your-project-id",
  });
}

export async function runLandingAuthAndQuery(
  ffdb: FFDBClient,
  email: string,
  password: string,
): Promise<QueryResult> {
  await ffdb.auth.signIn(email, password);
  return ffdb.query({
    sql: "select id, title from documents order by title",
    options: { max_rows: 100 },
  });
}

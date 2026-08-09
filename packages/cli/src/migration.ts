import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { basename } from "node:path";

import type { MigrationSpec } from "@ffdb/client";

const UP_MARKER = /^\s*--\s*migrate:up\s*$/im;
const DOWN_MARKER = /^\s*--\s*migrate:down\s*$/im;

export function parseMigration(source: string, filename: string, createdAtMs: number): MigrationSpec {
  const up = UP_MARKER.exec(source);
  const down = DOWN_MARKER.exec(source);
  if (up === null || down === null || up.index >= down.index) {
    throw new Error("Migration must contain -- migrate:up followed by -- migrate:down");
  }
  const id = basename(filename, ".sql").split("_")[0] ?? "";
  const name = basename(filename, ".sql").slice(id.length + 1).replaceAll("_", " ");
  const upSql = source.slice(up.index + up[0].length, down.index).trim();
  const downSql = source.slice(down.index + down[0].length).trim();
  if (!id || !name || !upSql || !downSql) throw new Error("Migration id, name, up SQL, and down SQL are required");
  const checksum = createHash("sha256")
    .update(id)
    .update("\0")
    .update(name)
    .update("\0")
    .update(upSql)
    .update("\0")
    .update(downSql)
    .digest("hex");
  return { id, name, up_sql: upSql, down_sql: downSql, checksum, created_at_ms: createdAtMs };
}

export async function loadMigration(path: string, createdAtMs = Date.now()): Promise<MigrationSpec> {
  return parseMigration(await readFile(path, "utf8"), path, createdAtMs);
}

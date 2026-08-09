import { createInterface } from "node:readline/promises";
import type { Readable, Writable } from "node:stream";

import type { MigrationSpec } from "@ffdb/client";

export interface ConfirmationIO {
  readonly input: Readable & { readonly isTTY?: boolean };
  readonly output: Writable & { readonly isTTY?: boolean };
}

export async function confirmDestructive(
  message: string,
  yes: boolean,
  io: ConfirmationIO = { input: process.stdin, output: process.stdout },
): Promise<void> {
  if (yes) return;
  if (io.input.isTTY !== true || io.output.isTTY !== true) {
    throw new Error(`${message}; pass --yes to confirm in non-interactive mode`);
  }
  const prompt = createInterface({ input: io.input, output: io.output });
  try {
    const answer = await prompt.question(`${message}\nType "yes" to continue: `);
    if (answer.trim().toLowerCase() !== "yes") throw new Error("Operation cancelled");
  } finally {
    prompt.close();
  }
}

export function migrationIdempotencyKey(migration: MigrationSpec): string {
  return `migration:${migration.id}:${migration.checksum}`;
}

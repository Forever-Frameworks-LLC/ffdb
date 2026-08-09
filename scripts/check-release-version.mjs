import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+(?:[-.][0-9A-Za-z][0-9A-Za-z.-]*)?$/.test(version)) {
  throw new Error("usage: node scripts/check-release-version.mjs VERSION");
}

const jsonFiles = ["package.json"];
for (const parent of ["apps", "packages"]) {
  for (const entry of readdirSync(parent, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      const path = join(parent, entry.name, "package.json");
      try {
        readFileSync(path);
        jsonFiles.push(path);
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
      }
    }
  }
}
for (const path of jsonFiles) {
  const manifest = JSON.parse(readFileSync(path, "utf8"));
  if (manifest.version !== version) {
    throw new Error(`${path} has version ${manifest.version}; expected ${version}`);
  }
}

const workspace = readFileSync("Cargo.toml", "utf8");
const workspaceVersion = workspace.match(/\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/)?.[1];
if (workspaceVersion !== version) {
  throw new Error(`Cargo workspace version is ${workspaceVersion}; expected ${version}`);
}
const worker = readFileSync("apps/database-worker/Cargo.toml", "utf8");
const workerVersion = worker.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (workerVersion !== version) {
  throw new Error(`database-worker version is ${workerVersion}; expected ${version}`);
}

console.log(`Validated release version ${version} across ${jsonFiles.length} npm manifests and Cargo.`);

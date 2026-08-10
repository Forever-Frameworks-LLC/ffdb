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

const releaseFacingTextFiles = [
  "README.md",
  "docs/operations/self-hosting.md",
  "docs/API/sdk.md",
  "apps/docs/src/content.ts",
  "apps/docs/src/content.test.ts",
  "apps/landing/src/App.tsx",
  "packages/client/README.md",
  "packages/sync-client/README.md",
  "packages/react/README.md",
  "packages/react-native/README.md",
  "packages/email-components/README.md",
  "packages/cli/README.md",
];
const releasePinPatterns = [
  /@ffdb\/[a-z-]+@(\d+\.\d+\.\d+)/g,
  /ffdb-(?:compose-bundle|native-linux-(?:amd64|arm64)|client|sync-client|react|react-native|email-components|cli)-(\d+\.\d+\.\d+)/g,
  /ghcr\.io\/forever-frameworks-llc\/ffdb-(?:runtime|gateway):(\d+\.\d+\.\d+)/g,
  /\b(?:FFDB_)?VERSION=(\d+\.\d+\.\d+)/g,
  /releases\/(?:download\/)?v(\d+\.\d+\.\d+)/g,
  /--version\s+(\d+\.\d+\.\d+)/g,
  /supported\s+(\d+\.\d+\.\d+)\s+release/g,
];
for (const path of releaseFacingTextFiles) {
  const source = readFileSync(path, "utf8");
  for (const pattern of releasePinPatterns) {
    for (const match of source.matchAll(pattern)) {
      if (match[1] !== version) {
        throw new Error(`${path} has release pin ${match[1]}; expected ${version}`);
      }
    }
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

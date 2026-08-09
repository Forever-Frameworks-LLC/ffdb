import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

const version = process.argv[2];
const archiveDirectory = process.argv[3];
if (!version || !/^\d+\.\d+\.\d+(?:[-.][0-9A-Za-z][0-9A-Za-z.-]*)?$/.test(version)) {
  throw new Error("usage: node scripts/check-sdk-package-contract.mjs VERSION [ARCHIVE_DIRECTORY]");
}

const repositoryUrl = "git+https://github.com/Forever-Frameworks-LLC/ffdb.git";
const bugsUrl = "https://github.com/Forever-Frameworks-LLC/ffdb/issues";
const packages = [
  { directory: "client", name: "@ffdb/client", exports: ["."] },
  { directory: "sync-client", name: "@ffdb/sync-client", exports: [".", "./browser", "./node"], dependencies: ["@ffdb/client"] },
  { directory: "react", name: "@ffdb/react", exports: ["."], dependencies: ["@ffdb/client", "@ffdb/sync-client"] },
  { directory: "react-native", name: "@ffdb/react-native", exports: ["."], dependencies: ["@ffdb/client", "@ffdb/sync-client"] },
  { directory: "email-components", name: "@ffdb/email-components", exports: ["."] },
  { directory: "cli", name: "@ffdb/cli", exports: ["."], dependencies: ["@ffdb/client"], bin: "dist/main.js", templates: true },
];

function check(condition, message) {
  if (!condition) throw new Error(message);
}

function exportTargets(value, targets = []) {
  if (typeof value === "string") targets.push(value);
  else if (value && typeof value === "object") {
    for (const nested of Object.values(value)) exportTargets(nested, targets);
  }
  return targets;
}

function validateManifest(manifest, expected, location, packed) {
  check(manifest.name === expected.name, `${location} has package name ${manifest.name}; expected ${expected.name}`);
  check(manifest.version === version, `${location} has version ${manifest.version}; expected ${version}`);
  check(manifest.private !== true, `${location} is private`);
  check(manifest.license === "Apache-2.0", `${location} must declare Apache-2.0`);
  check(manifest.author === "Forever Frameworks <admin@forever-frameworks.com>", `${location} has the wrong author metadata`);
  check(manifest.repository?.type === "git", `${location} repository type must be git`);
  check(manifest.repository?.url === repositoryUrl, `${location} has the wrong repository URL`);
  check(manifest.repository?.directory === `packages/${expected.directory}`, `${location} has the wrong repository directory`);
  check(manifest.bugs?.url === bugsUrl, `${location} has the wrong bugs URL`);
  check(typeof manifest.homepage === "string" && manifest.homepage.startsWith("https://ffdb.forever-frameworks.com/"), `${location} has an invalid homepage`);
  check(manifest.type === "module", `${location} must be ESM`);
  check(manifest.engines?.node === ">=24", `${location} must require the release Node baseline`);
  check(manifest.publishConfig?.access === "public", `${location} must publish publicly`);
  check(manifest.publishConfig?.registry === "https://registry.npmjs.org/", `${location} must use the public npm registry`);
  check(manifest.publishConfig?.provenance === true, `${location} must request provenance`);
  check(Array.isArray(manifest.files) && manifest.files.includes("dist") && manifest.files.includes("README.md"), `${location} must publish dist and README.md`);
  check(manifest.main === "./dist/index.js", `${location} has the wrong ESM entry point`);
  check(manifest.types === "./dist/index.d.ts", `${location} has the wrong declarations entry point`);
  for (const subpath of expected.exports) check(manifest.exports?.[subpath], `${location} is missing export ${subpath}`);
  check(manifest.exports?.["./package.json"] === "./package.json", `${location} must export package.json`);
  if (expected.bin) check(manifest.bin?.ffdb === expected.bin, `${location} has the wrong ffdb binary target`);
  if (expected.templates) check(manifest.files.includes("templates"), `${location} must publish CLI templates`);
  for (const dependency of expected.dependencies ?? []) {
    const selector = manifest.dependencies?.[dependency];
    check(selector === (packed ? version : "workspace:*"), `${location} has invalid ${dependency} selector ${selector}`);
  }
  if (packed) check(!JSON.stringify(manifest).includes("workspace:"), `${location} contains an unresolved workspace selector`);
}

for (const expected of packages) {
  const sourcePath = join("packages", expected.directory, "package.json");
  const source = JSON.parse(readFileSync(sourcePath, "utf8"));
  validateManifest(source, expected, sourcePath, false);
  const readme = readFileSync(join("packages", expected.directory, "README.md"), "utf8");
  check(readme.includes(expected.name), `packages/${expected.directory}/README.md does not use ${expected.name}`);

  if (!archiveDirectory) continue;
  const archive = join(archiveDirectory, `ffdb-${expected.directory}-${version}.tgz`);
  check(existsSync(archive), `missing package archive ${archive}`);
  const manifest = JSON.parse(execFileSync("tar", ["-xOf", archive, "package/package.json"], { encoding: "utf8" }));
  validateManifest(manifest, expected, archive, true);
  const packedReadme = execFileSync("tar", ["-xOf", archive, "package/README.md"], { encoding: "utf8" });
  check(packedReadme === readme, `${archive} contains a stale README.md`);
  const files = execFileSync("tar", ["-tzf", archive], { encoding: "utf8" }).trim().split("\n");
  check(files.includes("package/README.md"), `${archive} is missing README.md`);
  check(!files.some((file) => file.includes(".test.")), `${archive} contains compiled test files`);
  check(!files.some((file) => file.startsWith("package/src/")), `${archive} contains TypeScript source`);
  for (const target of exportTargets(manifest.exports)) {
    check(target.startsWith("./"), `${archive} contains a non-relative export target`);
    check(files.includes(`package/${target.slice(2)}`), `${archive} is missing exported file ${target}`);
  }
  if (expected.bin) check(files.includes(`package/${expected.bin}`), `${archive} is missing ${expected.bin}`);
  if (expected.templates) check(files.some((file) => file.startsWith("package/templates/")), `${archive} is missing CLI templates`);
}

console.log(`Validated ${packages.length} publishable SDK package contracts at ${version}${archiveDirectory ? " with packed archives" : ""}.`);

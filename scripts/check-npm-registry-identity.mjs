const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+(?:[-.][0-9A-Za-z][0-9A-Za-z.-]*)?$/.test(version)) {
  throw new Error("usage: node scripts/check-npm-registry-identity.mjs VERSION");
}

const packages = [
  "@ffdb/client",
  "@ffdb/cli",
  "@ffdb/sync-client",
  "@ffdb/react",
  "@ffdb/react-native",
  "@ffdb/email-components",
];

function check(condition, message) {
  if (!condition) throw new Error(message);
}

function registryPath(name) {
  return name.replace("/", "%2f");
}

function semverParts(value) {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(value);
  check(match, `cannot compare non-SemVer npm version ${value}`);
  return {
    core: match.slice(1, 4).map(Number),
    prerelease: match[4]?.split(".") ?? [],
  };
}

function compareSemver(left, right) {
  const a = semverParts(left);
  const b = semverParts(right);
  for (let index = 0; index < 3; index += 1) {
    if (a.core[index] !== b.core[index]) return a.core[index] < b.core[index] ? -1 : 1;
  }
  if (a.prerelease.length === 0 || b.prerelease.length === 0) {
    return a.prerelease.length === b.prerelease.length ? 0 : a.prerelease.length === 0 ? 1 : -1;
  }
  const length = Math.max(a.prerelease.length, b.prerelease.length);
  for (let index = 0; index < length; index += 1) {
    const leftPart = a.prerelease[index];
    const rightPart = b.prerelease[index];
    if (leftPart === undefined || rightPart === undefined) return leftPart === undefined ? -1 : 1;
    if (leftPart === rightPart) continue;
    const leftNumeric = /^\d+$/.test(leftPart);
    const rightNumeric = /^\d+$/.test(rightPart);
    if (leftNumeric && rightNumeric) return Number(leftPart) < Number(rightPart) ? -1 : 1;
    if (leftNumeric !== rightNumeric) return leftNumeric ? -1 : 1;
    return leftPart < rightPart ? -1 : 1;
  }
  return 0;
}

for (const name of packages) {
  const response = await fetch(`https://registry.npmjs.org/${registryPath(name)}`, {
    headers: { accept: "application/json" },
    signal: AbortSignal.timeout(15_000),
  });
  if (response.status === 404) {
    console.log(`${name}: unclaimed; ${version}=available`);
    continue;
  }
  check(response.ok, `${name} registry query failed with HTTP ${response.status}`);
  const metadata = await response.json();
  const versions = Object.keys(metadata.versions ?? {});
  const latest = metadata["dist-tags"]?.latest;
  check(metadata.name === name, `${name} registry identity mismatch`);
  check(!versions.includes(version), `${name}@${version} is already published`);
  const atOrAboveCandidate = versions.filter((published) => compareSemver(published, version) >= 0);
  check(atOrAboveCandidate.length === 0, `${name} has published version(s) at or above ${version}: ${atOrAboveCandidate.join(", ")}`);
  console.log(`${name}: latest=${latest}; versions=${versions.length}; ${version}=available`);
}

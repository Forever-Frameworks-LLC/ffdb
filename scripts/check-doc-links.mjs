import { access, readdir, readFile } from "node:fs/promises";
import { dirname, extname, isAbsolute, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const documents = [
  join(repository, "README.md"),
  join(repository, "CONTRIBUTING.md"),
  join(repository, "SECURITY.md"),
  ...(await markdownFiles(join(repository, "docs"))),
];
const failures = [];

for (const document of documents) {
  const source = await readFile(document, "utf8");
  const links = source.matchAll(/!?\[[^\]]*\]\(([^)]+)\)/g);
  for (const match of links) {
    const raw = match[1]?.trim().replace(/^<|>$/g, "").split(/\s+"/u)[0];
    if (raw === undefined || raw === "" || raw.startsWith("#")) continue;
    if (/^(?:https?:|mailto:)/iu.test(raw)) continue;
    if (/^[a-z][a-z0-9+.-]*:/iu.test(raw)) {
      failures.push(`${relative(document)}: unsupported link scheme ${raw}`);
      continue;
    }
    const path = decodeURIComponent(raw.split("#", 1)[0] ?? "");
    if (path === "") continue;
    const target = isAbsolute(path)
      ? resolve(repository, `.${normalize(path)}`)
      : resolve(dirname(document), path);
    if (target !== repository && !target.startsWith(`${repository}/`)) {
      failures.push(`${relative(document)}: link escapes repository: ${raw}`);
      continue;
    }
    try {
      await access(target);
    } catch {
      failures.push(`${relative(document)}: missing target ${raw}`);
    }
  }
}

const docsContent = await readFile(join(repository, "apps/docs/src/content.ts"), "utf8");
const docsRoutes = new Set(
  [...docsContent.matchAll(/^ {4}path: "([^"]+)",$/gmu)].map((match) => match[1]),
);
const landingApp = join(repository, "apps/landing/src/App.tsx");
const landingSource = await readFile(landingApp, "utf8");
const landingDocsLinks = new Set(
  [...landingSource.matchAll(/["'](\/docs(?:\/[^"'#?\s]*)?)/gu)].map((match) => match[1]),
);
for (const href of landingDocsLinks) {
  const route = href === "/docs" || href === "/docs/" ? "/" : href.slice("/docs".length);
  if (!docsRoutes.has(route)) {
    failures.push(`${relative(landingApp)}: missing docs application route ${href}`);
  }
}

if (failures.length > 0) {
  process.stderr.write(`${failures.join("\n")}\n`);
  process.exitCode = 1;
} else {
  process.stdout.write(
    `Validated local links in ${documents.length} Markdown files and ${landingDocsLinks.size} landing docs routes.\n`,
  );
}

async function markdownFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return markdownFiles(path);
    return entry.isFile() && extname(entry.name) === ".md" ? [path] : [];
  }));
  return nested.flat().sort();
}

function relative(path) {
  return path.slice(repository.length + 1);
}

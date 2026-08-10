import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import ts from "../packages/client/node_modules/typescript/lib/typescript.js";

const root = resolve(import.meta.dirname, "..");
const check = process.argv.includes("--check");

const read = (path) => readFile(resolve(root, path), "utf8");
const compact = (value) => value.replace(/\s+/gu, " ").replace(/\s+([,;:)])/gu, "$1").trim();
const exported = (node) => node.modifiers?.some((modifier) => modifier.kind === ts.SyntaxKind.ExportKeyword) === true;
const publiclyVisible = (node) => node.modifiers?.every((modifier) => ![
  ts.SyntaxKind.PrivateKeyword,
  ts.SyntaxKind.ProtectedKeyword,
].includes(modifier.kind)) !== false && node.name?.kind !== ts.SyntaxKind.PrivateIdentifier;

function declarationHead(node, sourceFile) {
  const end = node.body?.getStart(sourceFile) ?? node.end;
  return compact(sourceFile.text.slice(node.getStart(sourceFile), end).replace(/\s*=>\s*$/u, ""));
}

function collectTypeScriptReference(source, fileName) {
  const sourceFile = ts.createSourceFile(fileName, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const classes = [];
  const declarations = [];
  const functions = [];
  for (const node of sourceFile.statements) {
    if (!exported(node)) continue;
    if (ts.isClassDeclaration(node) && node.name !== undefined) {
      const members = node.members
        .filter(publiclyVisible)
        .filter((member) => ts.isConstructorDeclaration(member)
          || ts.isMethodDeclaration(member)
          || ts.isGetAccessorDeclaration(member)
          || ts.isSetAccessorDeclaration(member)
          || ts.isPropertyDeclaration(member))
        .map((member) => declarationHead(member, sourceFile));
      classes.push({ name: node.name.text, members });
    } else if ((ts.isInterfaceDeclaration(node) || ts.isTypeAliasDeclaration(node)) && node.name !== undefined) {
      declarations.push({ name: node.name.text, declaration: compact(node.getText(sourceFile)) });
    } else if (ts.isFunctionDeclaration(node) && node.name !== undefined) {
      functions.push({ name: node.name.text, signature: declarationHead(node, sourceFile) });
    }
  }
  return { classes, declarations, functions };
}

function collectNamedExports(source, fileName) {
  const sourceFile = ts.createSourceFile(fileName, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const names = new Set();
  for (const node of sourceFile.statements) {
    if (!ts.isExportDeclaration(node) || node.exportClause === undefined || !ts.isNamedExports(node.exportClause)) continue;
    for (const element of node.exportClause.elements) names.add(element.name.text);
  }
  return names;
}

function splitChunks(values, size) {
  const chunks = [];
  for (let index = 0; index < values.length; index += size) chunks.push(values.slice(index, index + size));
  return chunks;
}

function schemaLabel(schema) {
  if (schema === undefined) return "unspecified";
  if (schema.$ref !== undefined) return schema.$ref.split("/").at(-1);
  if (schema.enum !== undefined) return schema.enum.map((value) => JSON.stringify(value)).join(" | ");
  if (schema.type === "array") return `array<${schemaLabel(schema.items)}>`;
  if (Array.isArray(schema.type)) return schema.type.join(" | ");
  if (schema.type === "object") {
    const required = schema.required ?? [];
    const fields = Object.entries(schema.properties ?? {}).map(([name, value]) => `${name}${required.includes(name) ? "" : "?"}: ${schemaLabel(value)}`);
    return fields.length === 0 ? "object" : `{ ${fields.join("; ")} }`;
  }
  return schema.type ?? schema.format ?? "JSON";
}

function dereference(value, components, kind) {
  if (value?.$ref === undefined) return value;
  return components?.[kind]?.[value.$ref.split("/").at(-1)] ?? value;
}

function bodyLabel(body, components) {
  if (body === undefined) return "none";
  const resolved = dereference(body, components, "requestBodies");
  const schema = resolved?.content?.["application/json"]?.schema;
  return `${resolved?.required === true ? "required" : "optional"} ${schemaLabel(schema)} JSON`;
}

function responseLabel(response, components) {
  const resolved = dereference(response, components, "responses");
  const content = resolved?.content ?? {};
  const media = content["application/json"] ?? content["text/plain"];
  return media?.schema === undefined ? (resolved?.description ?? "empty") : schemaLabel(media.schema);
}

function collectOpenApiReference(contract) {
  const groups = new Map(contract.tags.map((tag) => [tag.name, []]));
  const methods = new Set(["get", "post", "put", "patch", "delete"]);
  for (const [path, pathItem] of Object.entries(contract.paths)) {
    for (const [method, operation] of Object.entries(pathItem)) {
      if (!methods.has(method)) continue;
      const parameters = [...(pathItem.parameters ?? []), ...(operation.parameters ?? [])]
        .map((parameter) => dereference(parameter, contract.components, "parameters"))
        .map((parameter) => `${parameter.name} (${parameter.in}, ${parameter.required === true ? "required" : "optional"}, ${schemaLabel(parameter.schema)})`);
      const security = operation.security ?? contract.security ?? [];
      const auth = security.length === 0 ? "public" : security.flatMap((entry) => Object.keys(entry)).join(" or ");
      const responses = Object.entries(operation.responses ?? {}).map(([status, response]) => `${status}: ${responseLabel(response, contract.components)}`);
      const errors = Object.keys(operation.responses ?? {}).filter((status) => Number(status) >= 400).join(", ") || "none declared";
      const entry = `${method.toUpperCase()} ${path} — ${operation.operationId}; auth: ${auth}; arguments: ${parameters.join("; ") || "none"}; body: ${bodyLabel(operation.requestBody, contract.components)}; returns: ${responses.join("; ")}; errors: ${errors}${operation["x-ffdb-idempotency"] === undefined ? "" : "; Idempotency-Key required"}`;
      const tag = operation.tags?.[0] ?? "Other";
      if (!groups.has(tag)) groups.set(tag, []);
      groups.get(tag).push(entry);
    }
  }
  return [...groups.entries()].filter(([, operations]) => operations.length > 0).map(([tag, operations]) => ({ tag, operations }));
}

function collectCliCommands(source) {
  const match = source.match(/function referenceHelp\(\): string \{\s*return `([\s\S]*?)`;\s*\}/u);
  if (match === null) throw new Error("CLI reference help template was not found");
  const groups = [];
  let current;
  for (const rawLine of match[1].split("\n")) {
    const line = rawLine.trim();
    if (line === "") continue;
    if (line.endsWith(":")) {
      current = { name: line.slice(0, -1), commands: [] };
      groups.push(current);
    } else {
      if (current === undefined) {
        current = { name: "Usage", commands: [] };
        groups.push(current);
      }
      current.commands.push(line);
    }
  }
  return groups;
}

function markdownClient(reference) {
  const lines = ["# @ffdb/client API reference", "", "Generated from the exported TypeScript declarations. All Promise-returning network methods can reject with FFDBError, AbortError, or a fetch/runtime error.", ""];
  for (const item of reference.classes) lines.push(`## ${item.name}`, "", ...item.members.map((member) => `- \`${member}\``), "");
  if (reference.functions.length > 0) lines.push("## Functions", "", ...reference.functions.map((item) => `- \`${item.signature}\``), "");
  lines.push("## Exported interfaces and types", "", ...reference.declarations.map((item) => `- \`${item.declaration}\``), "");
  return lines.join("\n");
}

function markdownCli(groups, moduleReference, environment) {
  const lines = ["# @ffdb/cli reference", "", "Generated from the shipped CLI help and exported module declarations. Use --json for structured output; invalid arguments, missing credentials, API failures, and declined destructive confirmations exit non-zero.", "", `Environment variables: ${environment.map((name) => `\`${name}\``).join(", ")}.`, ""];
  for (const group of groups) lines.push(`## ${group.name}`, "", ...group.commands.map((command) => `- \`${command}\``), "");
  lines.push("## Programmatic module exports", "");
  for (const item of moduleReference.classes) lines.push(`### ${item.name}`, "", ...item.members.map((member) => `- \`${member}\``), "");
  for (const item of moduleReference.functions) lines.push(`- \`${item.signature}\``);
  for (const item of moduleReference.declarations) lines.push(`- \`${item.declaration}\``);
  lines.push("");
  return lines.join("\n");
}

function markdownHttp(groups) {
  const lines = ["# FFDB HTTP API reference", "", "Generated from docs/API/openapi.json. The deployed /openapi.json document remains authoritative for machine-readable schemas.", ""];
  for (const group of groups) lines.push(`## ${group.tag}`, "", ...group.operations.map((operation) => `- ${operation}`), "");
  return lines.join("\n");
}

const clientSource = await read("packages/client/src/client.ts");
const clientTypesSource = await read("packages/client/src/types.ts");
const idSource = await read("packages/client/src/id.ts");
const clientBase = collectTypeScriptReference(clientSource, "client.ts");
const clientTypes = collectTypeScriptReference(clientTypesSource, "types.ts");
const clientId = collectTypeScriptReference(idSource, "id.ts");
const clientReference = {
  classes: clientBase.classes,
  functions: [...clientBase.functions, ...clientId.functions],
  declarations: [...clientBase.declarations, ...clientTypes.declarations, ...clientId.declarations].sort((left, right) => left.name.localeCompare(right.name)),
};

const cliFiles = ["args.ts", "billing.ts", "config.ts", "init.ts", "instance.ts", "migration.ts", "safety.ts", "typegen.ts"];
const cliSources = await Promise.all(cliFiles.map(async (name) => ({ name, source: await read(`packages/cli/src/${name}`) })));
const cliParts = cliSources.map(({ name, source }) => collectTypeScriptReference(source, name));
const cliIndex = await read("packages/cli/src/index.ts");
const cliExportNames = collectNamedExports(cliIndex, "index.ts");
const cliReference = {
  classes: cliParts.flatMap((part) => part.classes).filter((item) => cliExportNames.has(item.name)),
  functions: cliParts.flatMap((part) => part.functions).filter((item) => cliExportNames.has(item.name)),
  declarations: cliParts.flatMap((part) => part.declarations).filter((item) => cliExportNames.has(item.name)).sort((left, right) => left.name.localeCompare(right.name)),
};
const cliMain = await read("packages/cli/src/main.ts");
const cliCommands = collectCliCommands(cliMain);
const cliEnvironment = [...new Set([cliMain, cliIndex, ...cliSources.map(({ source }) => source)]
  .flatMap((source) => [...source.matchAll(/\bFFDB_[A-Z0-9_]+\b/gu)].map((match) => match[0])))].sort();
const openApi = JSON.parse(await read("docs/API/openapi.json"));
const httpOperations = collectOpenApiReference(openApi);

const clientClassSections = clientReference.classes.map((item) => ({
  heading: `${item.name} class`,
  paragraphs: [`Public constructor, properties, and methods exported by @ffdb/client. Signatures show required parameters, optional/defaulted parameters, and return types exactly as declared.`],
  bullets: item.members,
}));
const clientTypeSections = splitChunks(clientReference.declarations, 25).map((items, index) => ({
  heading: `Exported interfaces and types ${index + 1}`,
  paragraphs: ["Readonly markers, optional properties, unions, and generic defaults are preserved from the public declaration."],
  bullets: items.map((item) => item.declaration),
}));
const cliCommandSections = cliCommands.map((item) => ({ heading: `CLI: ${item.name}`, bullets: item.commands }));
const cliModuleSections = [
  { heading: "CLI module functions", bullets: cliReference.functions.map((item) => item.signature) },
  { heading: "CLI module interfaces and types", bullets: cliReference.declarations.map((item) => item.declaration) },
  ...cliReference.classes.map((item) => ({ heading: `CLI module: ${item.name}`, bullets: item.members })),
];
const httpOperationSections = httpOperations.map((item) => ({ heading: `HTTP: ${item.tag}`, bullets: item.operations }));

const generated = `// Generated by scripts/generate-public-doc-reference.mjs. Do not edit by hand.\n\nexport const clientClassSections = ${JSON.stringify(clientClassSections, null, 2)} as const;\n\nexport const clientTypeSections = ${JSON.stringify(clientTypeSections, null, 2)} as const;\n\nexport const clientFunctionSignatures = ${JSON.stringify(clientReference.functions.map((item) => item.signature), null, 2)} as const;\n\nexport const cliCommandSections = ${JSON.stringify(cliCommandSections, null, 2)} as const;\n\nexport const cliModuleSections = ${JSON.stringify(cliModuleSections, null, 2)} as const;\n\nexport const cliEnvironment = ${JSON.stringify(cliEnvironment, null, 2)} as const;\n\nexport const httpOperationSections = ${JSON.stringify(httpOperationSections, null, 2)} as const;\n`;

const outputs = new Map([
  ["apps/docs/src/generated-reference.ts", generated],
  ["apps/docs/public/reference/client.md", markdownClient(clientReference)],
  ["apps/docs/public/reference/cli.md", markdownCli(cliCommands, cliReference, cliEnvironment)],
  ["apps/docs/public/reference/http-api.md", markdownHttp(httpOperations)],
  ["apps/docs/public/openapi.json", `${JSON.stringify(openApi, null, 2)}\n`],
]);

for (const [path, contents] of outputs) {
  const target = resolve(root, path);
  if (check) {
    const current = await readFile(target, "utf8").catch(() => "");
    if (current !== contents) throw new Error(`${path} is stale; run node scripts/generate-public-doc-reference.mjs`);
  } else {
    await mkdir(resolve(target, ".."), { recursive: true });
    await writeFile(target, contents);
  }
}

console.log(`${check ? "Validated" : "Generated"} ${clientReference.classes.length} client classes, ${clientReference.declarations.length} client types, ${cliCommands.reduce((sum, group) => sum + group.commands.length, 0)} CLI help lines, and ${httpOperations.reduce((sum, group) => sum + group.operations.length, 0)} OpenAPI operations.`);

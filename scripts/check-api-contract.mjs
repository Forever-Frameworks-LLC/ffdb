import { readFile } from "node:fs/promises";

const [routerSource, contractSource] = await Promise.all([
  readFile(new URL("../apps/api/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../docs/API/openapi.json", import.meta.url), "utf8"),
]);

const contract = JSON.parse(contractSource);
if (contract.openapi !== "3.1.0" || typeof contract.paths !== "object") {
  throw new Error("docs/API/openapi.json is not an OpenAPI 3.1 contract");
}

// The contract keeps one path item per four-space-indented line. JSON.parse
// silently keeps the last duplicate object key, so reject duplicates from the
// source before route comparison can accidentally bless a discarded contract.
const declaredPathKeys = [...contractSource.matchAll(/^ {4}"(\/[^"\n]+)":/gmu)]
  .map((match) => match[1]);
const duplicatePathKeys = [...new Set(declaredPathKeys.filter(
  (path, index) => declaredPathKeys.indexOf(path) !== index,
))].sort();
if (duplicatePathKeys.length > 0) {
  throw new Error(`OpenAPI duplicate path keys: ${duplicatePathKeys.join(", ")}`);
}

const routerPaths = new Set(
  [...routerSource.matchAll(/\.route\(\s*"([^"]+)"/g)].map((match) => match[1]),
);
const contractPaths = new Set(Object.keys(contract.paths));
const missing = [...routerPaths].filter((path) => !contractPaths.has(path)).sort();
const stale = [...contractPaths].filter((path) => !routerPaths.has(path)).sort();
if (missing.length > 0 || stale.length > 0) {
  throw new Error(
    `OpenAPI/router drift; missing=[${missing.join(", ")}], stale=[${stale.join(", ")}]`,
  );
}

const operationIds = new Set();
for (const [path, item] of Object.entries(contract.paths)) {
  const placeholders = [...path.matchAll(/\{([^}]+)\}/g)].map((match) => match[1]);
  const parameters = Array.isArray(item.parameters) ? item.parameters : [];
  const pathParameterNames = parameters
    .map((parameter) => {
      const name = parameter.$ref?.split("/").at(-1);
      return name === undefined ? parameter : contract.components?.parameters?.[name];
    })
    .filter((parameter) => parameter?.in === "path" && parameter.required === true)
    .map((parameter) => parameter.name);
  for (const placeholder of placeholders) {
    if (!pathParameterNames.includes(placeholder)) {
      throw new Error(`${path} does not declare required path parameter ${placeholder}`);
    }
  }
  for (const method of ["get", "post", "put", "patch", "delete"]) {
    const operation = item[method];
    if (operation === undefined) continue;
    if (typeof operation.operationId !== "string" || operationIds.has(operation.operationId)) {
      throw new Error(`${method.toUpperCase()} ${path} has a missing or duplicate operationId`);
    }
    operationIds.add(operation.operationId);
    if (typeof operation.responses !== "object" || Object.keys(operation.responses).length === 0) {
      throw new Error(`${method.toUpperCase()} ${path} has no responses`);
    }
    if (operation["x-ffdb-idempotency"] === "required") {
      const allParameters = [...parameters, ...(operation.parameters ?? [])];
      if (!allParameters.some((parameter) => parameter.$ref === "#/components/parameters/IdempotencyKey")) {
        throw new Error(`${method.toUpperCase()} ${path} requires an undocumented Idempotency-Key`);
      }
    }
  }
}

const syncCursor = contract.paths["/v1/projects/{project_id}/sync"]?.get?.parameters
  ?.find((parameter) => parameter.name === "cursor" && parameter.in === "query");
if (syncCursor === undefined || syncCursor.required === true) {
  throw new Error("GET project sync must document cursor as optional for the initial pull");
}

console.log(`Validated ${contractPaths.size} OpenAPI paths against the Axum router.`);

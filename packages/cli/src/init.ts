import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, parse, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export type ProjectTemplate = "browser" | "node" | "react";

export interface ScaffoldResult {
  readonly directory: string;
  readonly files: readonly string[];
  readonly dependencies: readonly string[];
}

interface TemplateFile {
  readonly source: string;
  readonly destination: string;
}

const TEMPLATE_FILES: Readonly<Record<ProjectTemplate, readonly TemplateFile[]>> = {
  browser: [
    { source: "ffdb.ts.txt", destination: "src/ffdb.ts" },
    { source: "env.example.txt", destination: ".env.example" },
  ],
  node: [
    { source: "ffdb.ts.txt", destination: "src/ffdb.ts" },
    { source: "env.example.txt", destination: ".env.example" },
  ],
  react: [
    { source: "ffdb.ts.txt", destination: "src/ffdb.ts" },
    { source: "providers.tsx.txt", destination: "src/FFDBProviders.tsx" },
    { source: "env.example.txt", destination: ".env.example" },
  ],
};

const TEMPLATE_DEPENDENCIES: Readonly<Record<ProjectTemplate, readonly string[]>> = {
  browser: ["@ffdb/client"],
  node: ["@ffdb/client"],
  react: ["@ffdb/client", "@ffdb/react"],
};

export function parseProjectTemplate(value: string | undefined): ProjectTemplate {
  const template = value ?? "browser";
  if (template !== "browser" && template !== "react" && template !== "node") {
    throw new Error(`Unknown project template: ${template}. Expected browser, react, or node.`);
  }
  return template;
}

export async function scaffoldProject(
  targetDirectory: string,
  template: ProjectTemplate,
  options: { readonly templateRoot?: string } = {},
): Promise<ScaffoldResult> {
  const directory = resolve(targetDirectory);
  if (directory === parse(directory).root) throw new Error("Refusing to scaffold into a filesystem root");
  const templateRoot = options.templateRoot ?? resolve(dirname(fileURLToPath(import.meta.url)), "..", "templates");
  const files = TEMPLATE_FILES[template];
  const destinations = files.map((file) => join(directory, file.destination));

  for (const destination of destinations) {
    try {
      await access(destination);
      throw new Error(`Refusing to overwrite existing file: ${destination}`);
    } catch (cause) {
      if (cause instanceof Error && "code" in cause && cause.code === "ENOENT") continue;
      throw cause;
    }
  }

  const contents = await Promise.all(files.map((file) => readFile(join(templateRoot, template, file.source), "utf8")));
  for (let index = 0; index < files.length; index += 1) {
    const destination = destinations[index];
    const content = contents[index];
    if (destination === undefined || content === undefined) throw new Error("Invalid scaffold template definition");
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(destination, content, { encoding: "utf8", flag: "wx" });
  }

  return { directory, files: destinations, dependencies: TEMPLATE_DEPENDENCIES[template] };
}

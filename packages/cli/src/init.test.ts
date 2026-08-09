import { mkdtemp, readFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

import { describe, expect, it } from "vitest";

import { parseProjectTemplate, scaffoldProject } from "./init.js";

describe("scaffoldProject", () => {
  it("creates a browser starter without privileged credentials", async () => {
    const parent = await mkdtemp(join(tmpdir(), "ffdb-init-"));
    const target = join(parent, "browser-app");
    const result = await scaffoldProject(target, "browser");
    const source = await readFile(join(target, "src/ffdb.ts"), "utf8");
    const environment = await readFile(join(target, ".env.example"), "utf8");

    expect(result.dependencies).toEqual(["@ffdb/client"]);
    expect(source).toContain("BrowserSessionStore");
    expect(source).not.toContain("developerKey");
    expect(environment).not.toContain("FFDB_DEVELOPER_KEY");
  });

  it("creates the React provider and refuses to overwrite it", async () => {
    const parent = await mkdtemp(join(tmpdir(), "ffdb-init-"));
    const target = join(parent, "react-app");
    const result = await scaffoldProject(target, "react");

    expect(result.dependencies).toEqual(["@ffdb/client", "@ffdb/react"]);
    await expect(readFile(join(target, "src/FFDBProviders.tsx"), "utf8")).resolves.toContain("FFDBProvider");
    await expect(scaffoldProject(target, "react")).rejects.toThrow("Refusing to overwrite existing file");
  });

  it("validates template names and defaults to browser", () => {
    expect(parseProjectTemplate(undefined)).toBe("browser");
    expect(() => parseProjectTemplate("legacy")).toThrow("Unknown project template");
  });
});

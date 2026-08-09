import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vitest/config";

import { pages } from "./src/content";
import { renderDocRouteHtml, renderDocsSitemap } from "./src/seo";

function staticDocumentationRoutes(): Plugin {
  return {
    name: "ffdb-static-documentation-routes",
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        if (new URL(request.url ?? "/", "http://ffdb.local").pathname !== "/sitemap.xml") {
          next();
          return;
        }
        response.statusCode = 200;
        response.setHeader("Content-Type", "application/xml; charset=utf-8");
        response.end(renderDocsSitemap(pages));
      });
    },
    async closeBundle() {
      const output = fileURLToPath(new URL("./dist/", import.meta.url));
      const shell = await readFile(`${output}index.html`, "utf8");
      await Promise.all(pages.map(async (page) => {
        const directory = page.path === "/" ? output : `${output}${page.path}/`;
        await mkdir(directory, { recursive: true });
        await writeFile(`${directory}index.html`, renderDocRouteHtml(shell, page), "utf8");
      }));
      await writeFile(`${output}sitemap.xml`, renderDocsSitemap(pages), "utf8");
    },
  };
}

export default defineConfig({
  plugins: [react(), staticDocumentationRoutes()],
  server: { port: 5175, strictPort: true },
  preview: { port: 4175, strictPort: true },
  test: { environment: "node" },
});

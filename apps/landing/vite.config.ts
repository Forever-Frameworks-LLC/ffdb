import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vitest/config";

import { legalPageForPath, legalPages, renderLegalPage } from "./src/legal";

const applicationRoot = fileURLToPath(new URL(".", import.meta.url));

function publicLegalPages(): Plugin {
  const serveLegalPage = (requestUrl: string | undefined): string | undefined => {
    if (requestUrl === undefined) return undefined;
    const pathname = new URL(requestUrl, "http://ffdb.local").pathname;
    const page = legalPageForPath(pathname);
    return page === undefined ? undefined : renderLegalPage(page);
  };

  const middleware = (server: { middlewares: { use: (handler: (request: { url?: string }, response: { statusCode: number; setHeader: (name: string, value: string) => void; end: (body: string) => void }, next: () => void) => void) => void } }) => {
    server.middlewares.use((request, response, next) => {
      const html = serveLegalPage(request.url);
      if (html === undefined) {
        next();
        return;
      }
      response.statusCode = 200;
      response.setHeader("Content-Type", "text/html; charset=utf-8");
      response.end(html);
    });
  };

  return {
    name: "ffdb-public-legal-pages",
    configureServer: middleware,
    configurePreviewServer: middleware,
    async closeBundle() {
      const output = fileURLToPath(new URL("./dist/", import.meta.url));
      await Promise.all(legalPages.map(async (page) => {
        const directory = `${output}${page.path}`;
        await mkdir(directory, { recursive: true });
        await writeFile(`${directory}/index.html`, renderLegalPage(page), "utf8");
      }));
      await copyFile(`${applicationRoot}design/render-desktop-refresh.png`, `${output}social-card.jpg`);
    },
  };
}

export default defineConfig({
  plugins: [react(), publicLegalPages()],
  server: { port: 5174, strictPort: true },
  preview: { port: 4174, strictPort: true },
  test: { environment: "node" },
});

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { pages } from "./content";
import { canonicalDocUrl, metadataForDoc, publicOrigin, renderDocRouteHtml, renderDocsSitemap } from "./seo";

describe("documentation discovery and static routes", () => {
  it("creates route-specific canonical and social metadata", () => {
    const page = pages.find((candidate) => candidate.path === "/install/docker");
    expect(page).toBeDefined();
    if (page === undefined) return;
    const metadata = metadataForDoc(page);
    expect(metadata.title).toBe("Install with Docker Compose — FFDB Docs");
    expect(metadata.canonical).toBe(`${publicOrigin}/docs/install/docker/`);
    expect(JSON.parse(metadata.structuredData)).toMatchObject({
      "@type": "TechArticle",
      headline: "Install with Docker Compose",
      publisher: { name: "Forever Frameworks LLC" },
    });
  });

  it("emits readable static HTML at every documentation route", () => {
    const shell = readFileSync(new URL("../index.html", import.meta.url), "utf8");
    for (const page of pages) {
      const html = renderDocRouteHtml(shell, page);
      expect(html).toContain(`<link rel="canonical" href="${canonicalDocUrl(page.path)}"`);
      expect(html).toContain(`<h1>${page.title}</h1>`);
      expect(html).toContain(page.description);
      expect(html).toContain('type="application/ld+json"');
      expect(html).toContain('property="og:title"');
      expect(html).toContain('name="twitter:card"');
      expect(html).toContain('href="/terms/"');
      expect(html).toContain('href="/privacy/"');
      expect(html).toContain('href="/security/"');
    }
  });

  it("lists every documentation route in the docs sitemap", () => {
    const sitemap = renderDocsSitemap(pages);
    expect(sitemap.match(/<url>/gu)).toHaveLength(pages.length);
    for (const page of pages) expect(sitemap).toContain(`<loc>${canonicalDocUrl(page.path)}</loc>`);
  });

  it("links legal pages from the interactive footer and advertises both sitemaps", () => {
    const application = readFileSync(new URL("./DocsApp.tsx", import.meta.url), "utf8");
    const robots = readFileSync(new URL("../public/robots.txt", import.meta.url), "utf8");
    for (const path of ["/terms/", "/privacy/", "/security/"]) expect(application).toContain(`href="${path}"`);
    expect(robots).toContain(`Sitemap: ${publicOrigin}/sitemap.xml`);
    expect(robots).toContain(`Sitemap: ${publicOrigin}/docs/sitemap.xml`);
  });
});

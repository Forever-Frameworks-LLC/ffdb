import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { contactEmail, legalPages, publicOrigin, renderLegalPage, repositoryUrl } from "./legal";

describe("public legal and discovery surfaces", () => {
  it("publishes distinct, comprehensive end-user legal pages", () => {
    expect(legalPages.map((page) => page.path)).toEqual(["/terms", "/privacy", "/security"]);
    for (const page of legalPages) {
      expect(page.sections.length).toBeGreaterThanOrEqual(10);
      const html = renderLegalPage(page);
      expect(html).toContain(`<link rel="canonical" href="${publicOrigin}${page.path}/"`);
      expect(html).toContain('name="robots" content="index,follow,max-image-preview:large"');
      expect(html).toContain('property="og:image"');
      expect(html).toContain('name="twitter:card" content="summary_large_image"');
      expect(html).toContain('type="application/ld+json"');
      expect(html).toContain(contactEmail);
      expect(html).not.toMatch(/TODO|TBD|example\.com/iu);
    }
  });

  it("states the self-hosted data boundary and avoids invented certifications", () => {
    const privacy = JSON.stringify(legalPages.find((page) => page.path === "/privacy"));
    const security = JSON.stringify(legalPages.find((page) => page.path === "/security"));
    const terms = JSON.stringify(legalPages.find((page) => page.path === "/terms"));
    expect(privacy).toContain("Installing FFDB does not automatically send");
    expect(privacy).toContain("independently operated FFDB instance");
    expect(security).toContain("does not claim SOC 2");
    expect(security).toContain(`${repositoryUrl}/blob/main/SECURITY.md`);
    expect(terms).toContain("Apache License 2.0");
    expect(terms).toContain("does not itself create a payment obligation");
  });

  it("links every legal route from the landing footer", () => {
    const application = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    for (const page of legalPages) expect(application).toContain(`"${page.path}/"`);
  });

  it("provides canonical social metadata, robots directives, and sitemaps", () => {
    const index = readFileSync(new URL("../index.html", import.meta.url), "utf8");
    const robots = readFileSync(new URL("../public/robots.txt", import.meta.url), "utf8");
    const sitemap = readFileSync(new URL("../public/sitemap.xml", import.meta.url), "utf8");
    expect(index).toContain(`<link rel="canonical" href="${publicOrigin}/"`);
    expect(index).toContain('property="og:title"');
    expect(index).toContain('name="twitter:card"');
    expect(index).toContain('type="application/ld+json"');
    expect(robots).toContain(`Sitemap: ${publicOrigin}/sitemap.xml`);
    expect(robots).toContain(`Sitemap: ${publicOrigin}/docs/sitemap.xml`);
    expect(robots).toContain("Allow: /docs/");
    expect(robots).toContain("Disallow: /app/");
    for (const page of legalPages) expect(sitemap).toContain(`<loc>${publicOrigin}${page.path}/</loc>`);
  });
});

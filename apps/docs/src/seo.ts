import type { DocPage, DocSection } from "./content";

export const publicOrigin = "https://ffdb.forever-frameworks.com";
export const docsBaseUrl = `${publicOrigin}/docs`;
export const socialImageUrl = `${publicOrigin}/social-card.jpg`;

export interface DocMetadata {
  readonly title: string;
  readonly description: string;
  readonly canonical: string;
  readonly structuredData: string;
}

export function canonicalDocUrl(path: string): string {
  return path === "/" ? `${docsBaseUrl}/` : `${docsBaseUrl}${path}/`;
}

export function metadataForDoc(page: Pick<DocPage, "path" | "title" | "description" | "group">): DocMetadata {
  const canonical = canonicalDocUrl(page.path);
  const title = `${page.title} — FFDB Docs`;
  return {
    title,
    description: page.description,
    canonical,
    structuredData: safeJson({
      "@context": "https://schema.org",
      "@type": "TechArticle",
      headline: page.title,
      description: page.description,
      url: canonical,
      articleSection: page.group,
      isPartOf: { "@type": "WebSite", name: "FFDB Documentation", url: `${docsBaseUrl}/` },
      publisher: { "@type": "Organization", name: "Forever Frameworks LLC" },
      about: { "@type": "SoftwareSourceCode", name: "FFDB", codeRepository: "https://github.com/Forever-Frameworks-LLC/ffdb" },
    }),
  };
}

export function applyDocMetadata(page: DocPage): void {
  const metadata = metadataForDoc(page);
  document.title = metadata.title;
  setMeta('meta[name="description"]', "content", metadata.description);
  setMeta('link[rel="canonical"]', "href", metadata.canonical);
  setMeta('meta[property="og:title"]', "content", metadata.title);
  setMeta('meta[property="og:description"]', "content", metadata.description);
  setMeta('meta[property="og:url"]', "content", metadata.canonical);
  setMeta('meta[name="twitter:title"]', "content", metadata.title);
  setMeta('meta[name="twitter:description"]', "content", metadata.description);
  const script = document.querySelector<HTMLScriptElement>("#ffdb-doc-structured-data");
  if (script !== null) script.textContent = metadata.structuredData;
}

export function renderDocRouteHtml(shell: string, page: DocPage): string {
  const metadata = metadataForDoc(page);
  return shell
    .replace(/<title>[^<]*<\/title>/u, `<title>${escapeHtml(metadata.title)}</title>`)
    .replace(/(<meta name="description" content=")[^"]*(" \/>)/u, `$1${escapeHtml(metadata.description)}$2`)
    .replace(/(<link rel="canonical" href=")[^"]*(" \/>)/u, `$1${metadata.canonical}$2`)
    .replace(/(<meta property="og:title" content=")[^"]*(" \/>)/u, `$1${escapeHtml(metadata.title)}$2`)
    .replace(/(<meta property="og:description" content=")[^"]*(" \/>)/u, `$1${escapeHtml(metadata.description)}$2`)
    .replace(/(<meta property="og:url" content=")[^"]*(" \/>)/u, `$1${metadata.canonical}$2`)
    .replace(/(<meta name="twitter:title" content=")[^"]*(" \/>)/u, `$1${escapeHtml(metadata.title)}$2`)
    .replace(/(<meta name="twitter:description" content=")[^"]*(" \/>)/u, `$1${escapeHtml(metadata.description)}$2`)
    .replace(/(<script id="ffdb-doc-structured-data" type="application\/ld\+json">)[\s\S]*?(<\/script>)/u, `$1${metadata.structuredData}$2`)
    .replace('<div id="root"></div>', `<div id="root">${renderStaticDoc(page)}</div>`);
}

export function renderDocsSitemap(pages: readonly Pick<DocPage, "path">[]): string {
  const urls = pages.map((page) => `  <url>\n    <loc>${escapeHtml(canonicalDocUrl(page.path))}</loc>\n    <changefreq>weekly</changefreq>\n    <priority>${page.path === "/" ? "0.9" : "0.7"}</priority>\n  </url>`).join("\n");
  return `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${urls}\n</urlset>\n`;
}

function renderStaticDoc(page: DocPage): string {
  return `<div class="docs-app">
    <aside class="docs-sidebar" aria-label="Documentation navigation">
      <div class="sidebar-brand"><a href="/" class="wordmark">FFDB <span>Docs</span></a></div>
      <nav class="sidebar-nav"><ul class="nav-links"><li><a href="/docs/">Introduction</a></li><li><a href="/docs/quickstart">Quickstart</a></li><li><a href="/docs/install/docker">Install</a></li><li><a href="/docs/security">Production security</a></li></ul></nav>
      <div class="sidebar-footer"><span>Self-hosted · Apache-2.0</span><a href="/">FFDB home</a></div>
    </aside>
    <header class="docs-header"><a href="/docs/" class="wordmark">FFDB <span>Docs</span></a><div class="header-links"><a href="/">FFDB</a><a href="/docs/install/docker">Install</a><a class="portal-button" href="/app/">Open portal</a></div></header>
    <main class="docs-main" id="documentation">
      <article class="doc-article">
        <div class="breadcrumb"><span>${escapeHtml(page.group)}</span><i>›</i><span>${escapeHtml(page.title)}</span></div>
        <h1>${escapeHtml(page.title)}</h1>
        <p class="page-lead">${escapeHtml(page.description)}</p>
        ${page.sections.map(renderStaticSection).join("\n")}
      </article>
      <aside class="toc" aria-label="On this page"><strong>On this page</strong>${page.sections.map((section) => `<a href="#${sectionId(section.heading)}">${escapeHtml(section.heading)}</a>`).join("")}</aside>
    </main>
    <footer class="docs-footer"><span>© 2026 Forever Frameworks LLC. · FFDB is Apache-2.0 software.</span><nav aria-label="Legal links"><a href="/terms/">Terms</a><a href="/privacy/">Privacy</a><a href="/security/">Security &amp; disclaimer</a></nav></footer>
  </div>`;
}

function renderStaticSection(section: DocSection): string {
  const paragraphs = section.paragraphs?.map((paragraph) => `<p>${escapeHtml(paragraph)}</p>`).join("") ?? "";
  const bullets = section.bullets === undefined ? "" : `<ul>${section.bullets.map((bullet) => `<li>${escapeHtml(bullet)}</li>`).join("")}</ul>`;
  const codes = [section.code, ...(section.codes ?? [])].filter((code) => code !== undefined).map((code) => `<figure class="code-block"><figcaption class="code-header"><span>${escapeHtml(code.label)}</span><span>${escapeHtml(code.language)}</span></figcaption><pre><code>${escapeHtml(code.code)}</code></pre></figure>`).join("");
  const callout = section.callout === undefined ? "" : `<aside class="callout ${section.callout.kind}"><strong>${escapeHtml(section.callout.title)}</strong><p>${escapeHtml(section.callout.body)}</p></aside>`;
  return `<section class="doc-section" id="${sectionId(section.heading)}"><h2><a href="#${sectionId(section.heading)}">${escapeHtml(section.heading)}</a></h2>${paragraphs}${bullets}${codes}${callout}</section>`;
}

function setMeta(selector: string, attribute: string, value: string): void {
  document.querySelector(selector)?.setAttribute(attribute, value);
}

function sectionId(title: string): string {
  return title.toLowerCase().replace(/[^a-z0-9]+/gu, "-").replace(/^-|-$/gu, "");
}

function safeJson(value: unknown): string {
  return JSON.stringify(value).replaceAll("<", "\\u003c");
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

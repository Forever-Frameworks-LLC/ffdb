import { useEffect, useMemo, useRef, useState, type MouseEvent, type PropsWithChildren } from "react";

import { navigation, normalizePath, pageByPath, pages, searchPages, type DocPage, type DocSection } from "./content";
import { applyDocMetadata } from "./seo";
import { highlightCode } from "./syntax";

type Navigate = (path: string) => void;

function SearchIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5.5" /><path d="m13 13 4 4" /></svg>;
}

function MenuIcon({ close = false }: { close?: boolean }) {
  return <span className={`menu-icon ${close ? "close" : ""}`} aria-hidden="true"><i /><i /></span>;
}

function BrandMark() {
  return (
    <svg className="brand-mark" viewBox="0 0 256 256" fill="none" aria-hidden="true">
      <defs>
        <linearGradient id="ffdb-mark-primary" x1="0" y1="0" x2="256" y2="256">
          <stop stopColor="#2dd4bf" />
          <stop offset="1" stopColor="#14b8a6" />
        </linearGradient>
        <radialGradient id="ffdb-mark-glow" cx="0" cy="0" r="1" gradientUnits="userSpaceOnUse" gradientTransform="translate(128 120) scale(90)">
          <stop stopColor="#2dd4bf" stopOpacity=".5" />
          <stop offset="1" stopColor="#2dd4bf" stopOpacity="0" />
        </radialGradient>
      </defs>
      <rect width="256" height="256" rx="56" fill="#020617" />
      <ellipse cx="128" cy="120" rx="90" ry="60" fill="url(#ffdb-mark-glow)" />
      <ellipse cx="128" cy="80" rx="72" ry="24" fill="#0f172a" stroke="#1e293b" strokeWidth="2" />
      <path d="M56 80v48c0 14 32 32 72 32s72-18 72-32V80" fill="#0b1220" stroke="#1e293b" strokeWidth="2" />
      <ellipse cx="128" cy="128" rx="72" ry="24" fill="#0b1220" stroke="#1e293b" strokeWidth="2" />
      {[86, 92, 98, 104, 110, 116, 122].map((cy, index) => (
        <ellipse key={cy} cx="128" cy={cy} rx="72" ry="24" fill="url(#ffdb-mark-primary)" opacity={0.05 + index * 0.075} />
      ))}
    </svg>
  );
}

function Wordmark({ docs = true }: { docs?: boolean }) {
  return <span className="wordmark"><BrandMark /><strong>FFDB</strong>{docs && <span>Docs</span>}</span>;
}

function DocLink({ path, navigate, children, className, current, onFollow }: PropsWithChildren<{ path: string; navigate: Navigate; className?: string; current?: boolean; onFollow?: () => void }>) {
  const base = window.location.pathname.startsWith("/docs") ? "/docs" : "";
  const href = path === "/" ? `${base}/` : `${base}${path}`;
  const onClick = (event: MouseEvent<HTMLAnchorElement>) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    navigate(path);
    onFollow?.();
  };
  return <a href={href} className={className} aria-current={current ? "page" : undefined} onClick={onClick}>{children}</a>;
}

function ThemeToggle({ theme, setTheme }: { theme: "light" | "dark"; setTheme: (theme: "light" | "dark") => void }) {
  return (
    <button className="icon-button" type="button" onClick={() => setTheme(theme === "light" ? "dark" : "light")} aria-label={`Use ${theme === "light" ? "dark" : "light"} theme`}>
      {theme === "light" ? (
        <svg className="theme-icon" viewBox="0 0 20 20" fill="none" aria-hidden="true"><path d="M10 3a7 7 0 1 0 7 7c0-.45-.04-.9-.13-1.32A5.8 5.8 0 0 1 8.14 3.28 7.3 7.3 0 0 0 10 3Z" /></svg>
      ) : (
        <svg className="theme-icon" viewBox="0 0 20 20" fill="none" aria-hidden="true"><circle cx="10" cy="10" r="3.25" /><path d="M10 1.75v2M10 16.25v2M1.75 10h2M16.25 10h2M4.17 4.17l1.42 1.42M14.41 14.41l1.42 1.42M15.83 4.17l-1.42 1.42M5.59 14.41l-1.42 1.42" /></svg>
      )}
    </button>
  );
}

function Header({ openSearch, theme, setTheme, menuOpen, setMenuOpen }: { openSearch: () => void; theme: "light" | "dark"; setTheme: (theme: "light" | "dark") => void; menuOpen: boolean; setMenuOpen: (open: boolean) => void }) {
  return (
    <header className="docs-header">
      <div className="mobile-brand">
        <button className="icon-button" type="button" aria-label={menuOpen ? "Close navigation" : "Open navigation"} aria-expanded={menuOpen} onClick={() => setMenuOpen(!menuOpen)}><MenuIcon close={menuOpen} /></button>
        <a href="/" aria-label="FFDB home"><Wordmark docs={false} /></a>
      </div>
      <button type="button" className="search-trigger" onClick={openSearch} aria-label="Search documentation">
        <SearchIcon />
        <span>Search documentation…</span>
        <kbd><span>⌘</span>K</kbd>
      </button>
      <div className="header-links">
        <a href="/">FFDB</a>
        <a href="/docs/install/docker">Install</a>
        <ThemeToggle theme={theme} setTheme={setTheme} />
        <a className="portal-button" href="/app/">Open portal</a>
      </div>
    </header>
  );
}

function Sidebar({ currentPath, navigate, mobile, close }: { currentPath: string; navigate: Navigate; mobile?: boolean; close?: () => void }) {
  const currentGroup = navigation.find((group) => group.links.some((link) => link.href === currentPath))?.title ?? navigation[0]?.title ?? "";
  const [openGroups, setOpenGroups] = useState<readonly string[]>(["Start here", currentGroup]);
  const toggle = (title: string) => setOpenGroups((groups) => groups.includes(title) ? groups.filter((item) => item !== title) : [...groups, title]);

  return (
    <aside className={mobile ? "mobile-sidebar" : "docs-sidebar"} aria-label="Documentation navigation">
      <div className="sidebar-brand"><a href="/" aria-label="FFDB home"><Wordmark docs={false} /></a></div>
      <nav className="sidebar-nav">
        <ul>
          {navigation.map((group) => {
            const open = openGroups.includes(group.title) || group.title === currentGroup;
            return (
              <li className="nav-group" key={group.title}>
                <button type="button" aria-expanded={open} onClick={() => toggle(group.title)}><span>{group.title}</span><i aria-hidden="true">›</i></button>
                {open && (
                  <ul className="nav-links">
                    {group.links.map((link) => (
                      <li key={link.href}>
                        <DocLink path={link.href} navigate={navigate} current={currentPath === link.href} onFollow={close}>{link.title}</DocLink>
                        {currentPath === link.href && pageByPath.get(currentPath)?.sections.map((section) => (
                          <a className="section-link" href={`#${sectionId(section.heading)}`} key={section.heading} onClick={close}>{section.heading}</a>
                        ))}
                      </li>
                    ))}
                  </ul>
                )}
              </li>
            );
          })}
        </ul>
      </nav>
      <div className="sidebar-footer"><span>Self-hosted · Apache-2.0</span><DocLink path="/reference/http-api" navigate={navigate} onFollow={close}>API reference</DocLink></div>
    </aside>
  );
}

function SearchDialog({ query, setQuery, close, navigate }: { query: string; setQuery: (value: string) => void; close: () => void; navigate: Navigate }) {
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => { inputRef.current?.focus(); }, []);
  const results = useMemo(() => searchPages(query, query.trim() === "" ? 7 : 9), [query]);

  return (
    <div className="search-backdrop" role="presentation" onMouseDown={(event) => { if (event.currentTarget === event.target) close(); }}>
      <div className="search-dialog" role="dialog" aria-modal="true" aria-label="Search documentation">
        <div className="search-input"><SearchIcon /><input ref={inputRef} value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search pages and sections…" aria-label="Search pages and sections" /><button type="button" onClick={close}>Esc</button></div>
        <div className="search-results">
          <p>{query.trim() === "" ? "Suggested pages" : `${results.length} result${results.length === 1 ? "" : "s"}`}</p>
          {results.map((page) => (
            <DocLink className="search-result" path={page.path} navigate={navigate} onFollow={close} key={page.path}>
              <span><strong>{page.title}</strong><small>{page.description}</small></span><i aria-hidden="true">↵</i>
            </DocLink>
          ))}
          {results.length === 0 && <div className="empty-search">No documentation pages match “{query}”.</div>}
        </div>
      </div>
    </div>
  );
}

function CodeBlock({ code, label, language }: { code: string; label: string; language: string }) {
  const [copied, setCopied] = useState(false);
  const tokens = useMemo(() => highlightCode(code, language), [code, language]);
  const copy = async () => {
    await navigator.clipboard.writeText(code);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  };
  return (
    <figure className="code-block">
      <figcaption className="code-header">
        <span>{label}</span><span>{language}</span>
        <button type="button" onClick={() => void copy()} aria-label={`Copy ${label} code`}>
          <svg viewBox="0 0 20 20" aria-hidden="true"><rect x="6.5" y="6.5" width="9" height="10" rx="1.5" /><path d="M13.5 6.5v-2a1 1 0 0 0-1-1h-8a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1h2" /></svg>
          <span>{copied ? "Copied" : "Copy"}</span>
        </button>
        <span className="copy-status" role="status" aria-live="polite">{copied ? `${label} copied to clipboard` : ""}</span>
      </figcaption>
      <pre tabIndex={0} aria-label={`${label}, ${language} code`}><code>{tokens.map((token, index) => <span className={`token token-${token.kind}`} key={`${index}-${token.kind}`}>{token.value}</span>)}</code></pre>
    </figure>
  );
}

function SectionContent({ section }: { section: DocSection }) {
  return (
    <section className="doc-section" id={sectionId(section.heading)}>
      <h2><a href={`#${sectionId(section.heading)}`}>{section.heading}</a></h2>
      {section.paragraphs?.map((paragraph) => <p key={paragraph}>{paragraph}</p>)}
      {section.bullets && <ul>{section.bullets.map((bullet) => <li key={bullet}>{bullet}</li>)}</ul>}
      {section.code && <CodeBlock {...section.code} />}
      {section.codes?.map((code) => <CodeBlock {...code} key={`${code.label}-${code.language}`} />)}
      {section.callout && <aside className={`callout ${section.callout.kind}`}><strong>{section.callout.title}</strong><p>{section.callout.body}</p></aside>}
    </section>
  );
}

const introductionPaths = [
  { path: "/quickstart", label: "Quickstart", detail: "Install a release and make the first authenticated query." },
  { path: "/install/docker", label: "Docker Compose", detail: "Copy the complete stack and finish setup in the portal." },
  { path: "/client", label: "TypeScript client", detail: "Connect a browser, React Native, or Node application." },
] as const;

function IntroductionPaths({ navigate }: { navigate: Navigate }) {
  return (
    <nav className="introduction-paths" aria-label="Start with FFDB">
      {introductionPaths.map((item) => (
        <DocLink path={item.path} navigate={navigate} key={item.path}>
          <span><strong>{item.label}</strong><small>{item.detail}</small></span>
          <span aria-hidden="true">→</span>
        </DocLink>
      ))}
    </nav>
  );
}

function DocsPage({ page, navigate }: { page: DocPage; navigate: Navigate }) {
  const index = pages.findIndex((candidate) => candidate.path === page.path);
  const previous = index > 0 ? pages[index - 1] : undefined;
  const next = index < pages.length - 1 ? pages[index + 1] : undefined;
  useEffect(() => { applyDocMetadata(page); }, [page]);
  return (
    <>
      <article className={`doc-article ${page.path === "/" ? "is-introduction" : ""}`}>
        <header className="article-header">
          <div className="breadcrumb"><span>{page.group}</span><i aria-hidden="true">/</i><span>{page.path === "/" ? "Overview" : page.title}</span></div>
          <h1>{page.title}</h1>
          <p className="page-lead">{page.description}</p>
        </header>
        {page.path === "/" && <IntroductionPaths navigate={navigate} />}
        {page.sections.map((section) => <SectionContent section={section} key={section.heading} />)}
        <nav className="pager" aria-label="Previous and next pages">
          {previous ? <DocLink path={previous.path} navigate={navigate} className="pager-link previous"><span>Previous</span><strong>← {previous.title}</strong></DocLink> : <span />}
          {next ? <DocLink path={next.path} navigate={navigate} className="pager-link next"><span>Next</span><strong>{next.title} →</strong></DocLink> : <span />}
        </nav>
      </article>
      <aside className="toc" aria-label="On this page"><strong>On this page</strong>{page.sections.map((section) => <a href={`#${sectionId(section.heading)}`} key={section.heading}>{section.heading}</a>)}</aside>
    </>
  );
}

function NotFound({ navigate }: { navigate: Navigate }) {
  return <article className="doc-article not-found"><span>404</span><h1>Page not found</h1><p className="page-lead">That documentation route does not exist in the current FFDB information architecture.</p><DocLink path="/" navigate={navigate} className="primary-action">Back to introduction →</DocLink></article>;
}

function sectionId(title: string): string {
  return title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

export function DocsApp() {
  const [path, setPath] = useState(() => normalizePath(window.location.pathname));
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const [theme, setThemeState] = useState<"light" | "dark">(() => {
    const stored = window.localStorage.getItem("ffdb-docs-theme");
    if (stored === "light" || stored === "dark") return stored;
    return "dark";
  });

  const setTheme = (next: "light" | "dark") => {
    setThemeState(next);
    window.localStorage.setItem("ffdb-docs-theme", next);
  };

  const navigate: Navigate = (nextPath) => {
    const base = window.location.pathname.startsWith("/docs") ? "/docs" : "";
    const href = nextPath === "/" ? `${base}/` : `${base}${nextPath}`;
    window.history.pushState({}, "", href);
    setPath(nextPath);
    window.scrollTo({ top: 0, behavior: "instant" });
  };

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.querySelector('meta[name="theme-color"]')?.setAttribute("content", theme === "dark" ? "#111113" : "#f4f6f2");
  }, [theme]);

  useEffect(() => {
    if (!menuOpen && !searchOpen) return;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => { document.body.style.overflow = previousOverflow; };
  }, [menuOpen, searchOpen]);

  useEffect(() => {
    const onPopState = () => setPath(normalizePath(window.location.pathname));
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setSearchOpen(true);
      }
      if (event.key === "Escape") {
        setSearchOpen(false);
        setMenuOpen(false);
      }
    };
    window.addEventListener("popstate", onPopState);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("popstate", onPopState);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, []);

  const page = pageByPath.get(path);
  return (
    <div className="docs-app">
      <a className="skip-link" href="#documentation">Skip to documentation</a>
      <Sidebar currentPath={path} navigate={navigate} />
      <Header openSearch={() => setSearchOpen(true)} theme={theme} setTheme={setTheme} menuOpen={menuOpen} setMenuOpen={setMenuOpen} />
      {menuOpen && <><div className="drawer-backdrop" onClick={() => setMenuOpen(false)} /><Sidebar mobile currentPath={path} navigate={navigate} close={() => setMenuOpen(false)} /></>}
      <main className="docs-main" id="documentation">{page ? <DocsPage page={page} navigate={navigate} /> : <NotFound navigate={navigate} />}</main>
      <footer className="docs-footer">
        <span>© 2026 Forever Frameworks LLC. · FFDB is Apache-2.0 software.</span>
        <nav aria-label="Legal links"><a href="/terms/">Terms</a><a href="/privacy/">Privacy</a><a href="/security/">Security &amp; disclaimer</a></nav>
      </footer>
      {searchOpen && <SearchDialog query={searchQuery} setQuery={setSearchQuery} close={() => setSearchOpen(false)} navigate={navigate} />}
    </div>
  );
}

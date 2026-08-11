import { useGSAP } from "@gsap/react";
import gsap from "gsap";
import { ScrollTrigger } from "gsap/ScrollTrigger";
import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PropsWithChildren } from "react";

import { billingPlans, capabilities, deploymentShapes, facts, integrations, workflow } from "./content";

gsap.registerPlugin(ScrollTrigger, useGSAP);

function Arrow({ external = false }: { external?: boolean }) {
  return <span aria-hidden="true">{external ? "↗" : "→"}</span>;
}

function activateTabFromKey(event: ReactKeyboardEvent<HTMLButtonElement>, index: number, count: number, activate: (index: number) => void): void {
  let next = index;
  if (event.key === "ArrowRight") next = (index + 1) % count;
  else if (event.key === "ArrowLeft") next = (index - 1 + count) % count;
  else if (event.key === "Home") next = 0;
  else if (event.key === "End") next = count - 1;
  else return;
  event.preventDefault();
  activate(next);
  event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
}

function Navigation() {
  const [scrolled, setScrolled] = useState(false);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const onScroll = () => setScrolled(window.scrollY > 36);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    if (!open) return;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => { document.body.style.overflow = previousOverflow; };
  }, [open]);

  return (
    <header className={`site-header ${scrolled ? "is-scrolled" : ""}`}>
      <nav className="nav-shell" aria-label="Primary navigation">
        <a className="brand" href="/" aria-label="FFDB home">FFDB</a>
        <div className="desktop-nav">
          <a href="#capabilities">Capabilities</a>
          <a href="#architecture">Architecture</a>
          <a href="#security">Security</a>
          <a href="#billing">Billing</a>
          <a href="/docs/">Documentation</a>
        </div>
        <div className="nav-actions">
          <a className="nav-link" href="/app/">Portal</a>
          <a className="button button-dark button-small" href="/docs/install/docker">Install FFDB <Arrow /></a>
        </div>
        <button
          className={`menu-button ${open ? "is-open" : ""}`}
          type="button"
          aria-label={open ? "Close menu" : "Open menu"}
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
        >
          <span />
          <span />
        </button>
      </nav>
      {open && (
        <div className="mobile-menu">
          <a href="#capabilities" onClick={() => setOpen(false)}>Capabilities</a>
          <a href="#architecture" onClick={() => setOpen(false)}>Architecture</a>
          <a href="#security" onClick={() => setOpen(false)}>Security</a>
          <a href="#billing" onClick={() => setOpen(false)}>Billing</a>
          <a href="/docs/" onClick={() => setOpen(false)}>Documentation</a>
          <div className="mobile-menu-actions">
            <a className="button button-outline" href="/app/">Open portal</a>
            <a className="button button-dark" href="/docs/install/docker">Install FFDB <Arrow /></a>
          </div>
        </div>
      )}
    </header>
  );
}

function AsciiSphere() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    if (canvas === null || context === null || context === undefined) return;
    const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const chars = "░▒▓█▀▄▌▐│─┤├┴┬╭╮╰╯";
    let frame = 0;
    let time = 0;

    const resize = () => {
      const rect = canvas.getBoundingClientRect();
      const ratio = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.max(1, Math.floor(rect.width * ratio));
      canvas.height = Math.max(1, Math.floor(rect.height * ratio));
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
    };

    const draw = () => {
      const { width, height } = canvas.getBoundingClientRect();
      context.clearRect(0, 0, width, height);
      context.font = "11px ui-monospace, monospace";
      context.textAlign = "center";
      context.textBaseline = "middle";
      const radius = Math.min(width, height) * 0.43;
      const points: Array<{ x: number; y: number; z: number; char: string }> = [];
      for (let latitude = 0.14; latitude < Math.PI; latitude += 0.18) {
        for (let longitude = 0; longitude < Math.PI * 2; longitude += 0.18) {
          const sourceX = Math.sin(latitude) * Math.cos(longitude + time);
          const sourceY = Math.cos(latitude);
          const sourceZ = Math.sin(latitude) * Math.sin(longitude + time);
          const rotation = time * 0.55;
          const x = sourceX * Math.cos(rotation) - sourceZ * Math.sin(rotation);
          const z = sourceX * Math.sin(rotation) + sourceZ * Math.cos(rotation);
          points.push({
            x: width / 2 + x * radius,
            y: height / 2 + sourceY * radius,
            z,
            char: chars[Math.floor(((z + 1) / 2) * (chars.length - 1))] ?? "·",
          });
        }
      }
      points.sort((left, right) => left.z - right.z);
      for (const point of points) {
        context.fillStyle = `rgba(130, 215, 165, ${0.08 + (point.z + 1) * 0.2})`;
        context.fillText(point.char, point.x, point.y);
      }
      if (!reducedMotion) {
        time += 0.007;
        frame = window.requestAnimationFrame(draw);
      }
    };

    resize();
    draw();
    window.addEventListener("resize", resize);
    return () => {
      window.removeEventListener("resize", resize);
      window.cancelAnimationFrame(frame);
    };
  }, []);

  return <canvas ref={canvasRef} className="ascii-sphere" aria-hidden="true" />;
}

function Hero() {
  const marquee = [
    ["1 SQLite DB", "per project"],
    ["Rust workers", "isolated execution"],
    ["Logical sync", "opaque cursors"],
    ["S3-compatible", "RLS-protected storage"],
  ] as const;

  return (
    <section className="hero">
      <div className="hero-grid" aria-hidden="true" />
      <div className="sphere-wrap"><AsciiSphere /></div>
      <div className="container hero-content">
        <p className="eyebrow"><span />The backend that stays on your side of the boundary</p>
        <h1>
          <span className="hero-title-line">
            Your backend.
            <span className="hero-product-window" aria-hidden="true">
              <img src="/portal-overview.png" alt="" />
            </span>
          </span>
          <span className="hero-title-line hero-title-accent">Your boundary.</span>
        </h1>
        <div className="hero-bottom">
          <p>
            Ship sign-in, scoped SQL, storage, offline sync, and billing without handing a hosted vendor your production database. Install one signed release with Docker or native Linux services.
          </p>
          <div className="button-row">
            <a className="button button-dark" href="/docs/install/docker">Install FFDB <Arrow /></a>
            <a className="button button-outline" href="/docs/">Read the docs</a>
          </div>
        </div>
      </div>
      <div className="hero-marquee" aria-label="FFDB architecture highlights">
        <div className="marquee-track">
          {[...marquee, ...marquee].map(([value, label], index) => (
            <div className="stat-lockup" key={`${value}-${index}`}>
              <strong>{value}</strong><span>{label}</span>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}

function Reveal({ children, className = "" }: PropsWithChildren<{ className?: string }>) {
  const ref = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const node = ref.current;
    if (node === null || !("IntersectionObserver" in window)) {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver(([entry]) => {
      if (entry?.isIntersecting) {
        setVisible(true);
        observer.disconnect();
      }
    }, { threshold: 0.12 });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  return <div ref={ref} className={`reveal ${visible ? "is-visible" : ""} ${className}`}>{children}</div>;
}

function FeatureVisual({ kind }: { kind: string }) {
  if (kind === "database") {
    return (
      <svg viewBox="0 0 210 150" role="img" aria-label="Project database isolation diagram">
        {[0, 1, 2].map((item) => (
          <g key={item} transform={`translate(${item * 56 + 12} 34)`}>
            <ellipse cx="28" cy="10" rx="25" ry="9" />
            <path d="M3 10v58c0 5 11 9 25 9s25-4 25-9V10" />
            <path d="M3 39c0 5 11 9 25 9s25-4 25-9" />
          </g>
        ))}
        <path className="pulse-path" d="M40 126h130" />
      </svg>
    );
  }
  if (kind === "shield") {
    return (
      <svg viewBox="0 0 210 150" role="img" aria-label="Row-level security shield">
        <path d="M105 15 158 35v42c0 32-19 50-53 62-34-12-53-30-53-62V35l53-20Z" />
        <path className="filled-shape" d="m83 75 14 14 31-34 8 8-39 43-22-22 8-9Z" />
      </svg>
    );
  }
  if (kind === "sync") {
    return (
      <svg viewBox="0 0 210 150" role="img" aria-label="Logical sync diagram">
        <rect x="12" y="44" width="62" height="62" rx="3" />
        <rect x="136" y="44" width="62" height="62" rx="3" />
        <path className="pulse-path" d="M80 61h48l-10-10m10 10-10 10M130 90H82l10 10M82 90l10-10" />
        <circle className="filled-shape pulse-dot" cx="105" cy="75" r="5" />
      </svg>
    );
  }
  return (
    <svg viewBox="0 0 210 150" role="img" aria-label="Protected object storage diagram">
      <path d="M28 105V43l48-22 48 22v62l-48 23-48-23Z" />
      <path d="m76 21 48 22 49-22v62l-49 22V43L76 21Z" transform="translate(-0.5 0)" />
      <path className="filled-shape" d="M158 84v-9a12 12 0 0 0-24 0v9h-7v31h38V84h-7Zm-17-9a5 5 0 0 1 10 0v9h-10v-9Z" />
    </svg>
  );
}

function Capabilities() {
  const [active, setActive] = useState(0);
  return (
    <section className="section" id="capabilities">
      <div className="container">
        <Reveal className="section-heading">
          <p className="eyebrow"><span />Capabilities</p>
          <h2>Everything at the boundary.<br /><em>Nothing hidden behind a service.</em></h2>
        </Reveal>
        <div className="capability-accordion" role="list">
          {capabilities.map((feature, index) => (
            <article className={`capability-panel ${active === index ? "is-active" : ""}`} key={feature.title} role="listitem">
              <button
                className="capability-trigger"
                type="button"
                aria-expanded={active === index}
                aria-controls={`capability-panel-${index}`}
                onClick={() => setActive(index)}
                onFocus={() => setActive(index)}
              >
                <span>{feature.title}</span>
                <span aria-hidden="true">{active === index ? "−" : "+"}</span>
              </button>
              <div className="capability-body" id={`capability-panel-${index}`} aria-hidden={active !== index} inert={active !== index}>
                <div className="feature-visual"><FeatureVisual kind={feature.visual} /></div>
                <p>{feature.description}</p>
                <a href="/docs/">Explore the capability <Arrow /></a>
              </div>
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}

const controlLayers = [
  ["Application", "Use the typed client from browser, React Native, or Node without exposing a database credential."],
  ["Policy boundary", "Verify the project, user, claims, limits, and row rules before data reaches the worker."],
  ["Operator boundary", "Keep releases, storage, backups, metrics, and provider keys under the deployment owner’s control."],
] as const;

function ControlNarrative() {
  return (
    <section className="control-narrative" aria-labelledby="control-title">
      <div className="container narrative-layout">
        <div className="narrative-sticky">
          <p className="eyebrow light"><span />Control is a system property</p>
          <h2 id="control-title">
            {"Every request crosses a boundary you can name.".split(" ").map((word, index) => (
              <span className="scrub-word" key={`${word}-${index}`}>{word}{" "}</span>
            ))}
          </h2>
          <p>FFDB keeps the request path explicit from application session to project database, so policy and ownership do not disappear inside a hosted black box.</p>
        </div>
        <div className="narrative-cards">
          {controlLayers.map(([title, body]) => (
            <article key={title}>
              <span aria-hidden="true" />
              <h3>{title}</h3>
              <p>{body}</p>
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}

function Workflow() {
  const [active, setActive] = useState(0);
  const item = workflow[active] ?? workflow[0];
  return (
    <section className="workflow section" id="architecture">
      <div className="workflow-lines" aria-hidden="true" />
      <div className="container workflow-inner">
        <Reveal className="workflow-heading">
          <p className="eyebrow light"><span />From install to protected data</p>
          <h2>One system.<br />Three clear moves.</h2>
        </Reveal>
        <div className="workflow-grid">
          <div className="workflow-tabs" role="tablist" aria-label="FFDB setup steps">
            {workflow.map((step, index) => (
              <button
                type="button"
                role="tab"
                aria-selected={active === index}
                aria-controls="workflow-panel"
                id={`workflow-tab-${index}`}
                tabIndex={active === index ? 0 : -1}
                className={active === index ? "is-active" : ""}
                onClick={() => setActive(index)}
                onKeyDown={(event) => activateTabFromKey(event, index, workflow.length, setActive)}
                key={step.number}
              >
                <span>{step.number}</span>
                <strong>{step.label}</strong>
                <small>{step.title}</small>
              </button>
            ))}
          </div>
          <div className="code-window" role="tabpanel" id="workflow-panel" aria-labelledby={`workflow-tab-${active}`} tabIndex={0}>
            <div className="code-titlebar">
              <span><i /><i /><i /></span>
              <span>{item.label.toLowerCase().replace(" ", "-")}.txt</span>
              <span>FFDB</span>
            </div>
            <p>{item.body}</p>
            <pre><code>{item.code}</code></pre>
          </div>
        </div>
      </div>
    </section>
  );
}

const pipeline = [
  ["PostgreSQL control plane", "organizations · projects"],
  ["Rust API", "verified request context"],
  ["Isolated worker", "parser · limits · authorizer"],
  ["SQLite project DB", "views · triggers · RLS"],
  ["Logical sync", "snapshot · push · pull"],
  ["S3-compatible store", "short-lived signed operations"],
] as const;

function Architecture() {
  const [active, setActive] = useState(0);
  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const interval = window.setInterval(() => setActive((value) => (value + 1) % pipeline.length), 1800);
    return () => window.clearInterval(interval);
  }, []);
  return (
    <section className="section architecture-section">
      <div className="container split-grid">
        <Reveal>
          <p className="eyebrow"><span />Self-hosted architecture</p>
          <h2>SQLite where it fits.<br />Boundaries where they matter.</h2>
          <p className="lead">
            Your application talks to a normal HTTP API instead of opening a production database file. FFDB verifies the user, applies the project’s rules, and runs the query inside an isolated worker.
          </p>
          <div className="mini-facts">
            <div><strong>SQLite</strong><span>Project data</span></div>
            <div><strong>Rust</strong><span>Execution boundary</span></div>
            <div><strong>Postgres</strong><span>Control plane</span></div>
          </div>
        </Reveal>
        <Reveal className="pipeline-panel">
          <div className="panel-title"><span>Request path</span><span className="status"><i /> ready</span></div>
          {pipeline.map(([name, detail], index) => (
            <div className={`pipeline-row ${active === index ? "is-active" : ""}`} key={name}>
              <i /><div><strong>{name}</strong><span>{detail}</span></div><code>{String(index + 1).padStart(2, "0")}</code>
            </div>
          ))}
        </Reveal>
      </div>
    </section>
  );
}

function Facts() {
  return (
    <section className="section facts-section">
      <div className="container">
        <div className="section-heading row-heading">
          <Reveal>
            <p className="eyebrow"><span />Architecture facts</p>
            <h2>Specific by design.<br />Auditable by default.</h2>
          </Reveal>
          <p>Know where your data lives, which service can reach it, and which rules are applied before a query runs.</p>
        </div>
        <div className="fact-grid">
          {facts.map((fact) => <div className="fact" key={fact.label}><strong>{fact.value}</strong><span>{fact.label}</span></div>)}
        </div>
      </div>
    </section>
  );
}

function Integrations() {
  const reversed = [...integrations].reverse();
  return (
    <section className="section integrations-section">
      <div className="container integrations-heading">
        <p className="eyebrow centered"><span />Shipped interfaces<span /></p>
        <h2>One platform.<br />Clear runtime boundaries.</h2>
        <p>Start with one HTTP client, then add the session and local-replica adapter that matches browser, server, React, or React Native code.</p>
      </div>
      {[integrations, reversed].map((items, row) => (
        <div className="integration-viewport" key={row}>
          <div className={`integration-track ${row === 1 ? "reverse" : ""}`}>
            {[...items, ...items].map(([name, category], index) => (
              <div className="integration-card" key={`${name}-${index}`}><strong>{name}</strong><span>{category}</span></div>
            ))}
          </div>
        </div>
      ))}
    </section>
  );
}

const securityFeatures = [
  ["Constrained SQL execution", "Parsing, SQLite preparation, authorizer rules, resource limits, and generated RLS machinery work together."],
  ["Immutable request context", "Policy functions read verified subject, role, claims, and token context—not caller-authored SQL values."],
  ["Durable storage authorization", "State-changing provider work uses short-lived grants and authoritative SQLite reservations across nodes."],
  ["Explicit compatibility", "Documented SQLite differences fail closed instead of pretending to be byte-for-byte PostgreSQL."],
] as const;

function Security() {
  return (
    <section className="section security-section" id="security">
      <div className="container split-grid">
        <Reveal>
          <p className="eyebrow"><span />Security model</p>
          <h2>Defense in depth.<br />Visible in the source.</h2>
          <p className="lead">Keep access decisions in the backend instead of duplicating them across screens and jobs. You can review the parser, policy compiler, worker limits, and storage authorization in the software you deploy.</p>
          <div className="tag-list">
            {['Default-deny RLS', 'Worker isolation', 'Opaque cursors', 'Short-lived grants', 'Encrypted backups'].map((tag) => <span key={tag}>{tag}</span>)}
          </div>
        </Reveal>
        <div className="security-cards">
          {securityFeatures.map(([title, body], index) => (
            <Reveal className="security-card" key={title}>
              <span className="security-icon" aria-hidden="true">{String(index + 1).padStart(2, "0")}</span>
              <div><h3>{title}</h3><p>{body}</p></div>
            </Reveal>
          ))}
        </div>
      </div>
    </section>
  );
}

const developerExamples = [
  {
    label: "Install SDK",
    code: `# Keep the client and CLI on the server release version.
VERSION=0.3.13
npm install --save-exact "@ffdb/client@$VERSION"
npm install --global "@ffdb/cli@$VERSION"
ffdb --help`,
  },
  {
    label: "Create client",
    code: `import { FFDBClient } from "@ffdb/client";

const ffdb = new FFDBClient({
  baseUrl: "https://data.example.com",
  projectId: "your-project-id",
});

await ffdb.auth.signIn(email, password);`,
  },
  {
    label: "Query",
    code: `const result = await ffdb.query({
  sql: "select id, title from documents order by title",
  options: { max_rows: 100 },
});

for (const row of result.rows) {
  console.log(row[0], row[1]);
}`,
  },
] as const;

function Developers() {
  const [active, setActive] = useState(0);
  const [copied, setCopied] = useState(false);
  const example = developerExamples[active] ?? developerExamples[0];
  const copy = async () => {
    await navigator.clipboard.writeText(example.code);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  };
  return (
    <section className="section developers-section">
      <div className="container split-grid developer-grid">
        <Reveal>
          <p className="eyebrow"><span />For developers</p>
          <h2>One typed client.<br />No hidden connection.</h2>
          <p className="lead">Add sign-in, queries, storage, offline sync, platform billing, and complete project commerce without shipping a database credential to the app. Install the public client and CLI from npm; download version-matched framework, native, sync, and email integrations from the corresponding tagged GitHub Release.</p>
          <div className="benefit-grid">
            <div><strong>Tagged values</strong><span>Lossless SQL parameters</span></div>
            <div><strong>AbortSignal</strong><span>Cancellable operations</span></div>
            <div><strong>Idempotency</strong><span>Safe state changes</span></div>
            <div><strong>Stable errors</strong><span>Codes + request IDs</span></div>
          </div>
        </Reveal>
        <Reveal className="developer-code">
          <div className="code-tabs">
            <div className="code-tab-list" role="tablist" aria-label="Client examples">
              {developerExamples.map((item, index) => (
                <button
                  type="button"
                  role="tab"
                  id={`developer-tab-${index}`}
                  aria-controls="developer-code-panel"
                  aria-selected={active === index}
                  tabIndex={active === index ? 0 : -1}
                  className={active === index ? "is-active" : ""}
                  onClick={() => setActive(index)}
                  onKeyDown={(event) => activateTabFromKey(event, index, developerExamples.length, setActive)}
                  key={item.label}
                >{item.label}</button>
              ))}
            </div>
            <button type="button" className="copy-button" onClick={() => void copy()} aria-label="Copy code example">{copied ? "Copied" : "Copy"}</button>
          </div>
          <pre role="tabpanel" id="developer-code-panel" aria-labelledby={`developer-tab-${active}`} tabIndex={0}><code>{example.code}</code></pre>
          <div className="code-links"><a href="/docs/client">Client docs</a><span>|</span><a href="/docs/cli">CLI guide</a></div>
        </Reveal>
      </div>
    </section>
  );
}

const perspectives = [
  {
    role: "Platform owner",
    title: "One install, an explicit operating model.",
    body: "Choose private, team, BYO Stripe, or Connect during first-owner onboarding. The portal then exposes only the billing and administration surfaces that apply to that instance.",
  },
  {
    role: "Application developer",
    title: "One client, clear runtime boundaries.",
    body: "Use the same project contract from browser, React, React Native, and Node while each runtime keeps the session and offline adapter appropriate to its environment.",
  },
  {
    role: "Product operator",
    title: "Usage, commerce, and fulfillment stay distinct.",
    body: "Meter the FFDB platform at the organization level while every project chooses BYO Stripe credentials or Connect for its own products, memberships, refunds, and entitlements.",
  },
] as const;

function Perspectives() {
  const [active, setActive] = useState(0);
  const perspective = perspectives[active] ?? perspectives[0];
  return (
    <section className="section perspectives-section" aria-labelledby="perspectives-title">
      <div className="container perspective-shell">
        <div className="perspective-heading">
          <p className="eyebrow"><span />One product, three points of view</p>
          <h2 id="perspectives-title">The right surface for the person doing the work.</h2>
        </div>
        <div className="perspective-carousel">
          <div className="perspective-copy" role="tabpanel" id="perspective-panel" aria-labelledby={`perspective-tab-${active}`} tabIndex={0}>
            <span>{perspective.role}</span>
            <h3>{perspective.title}</h3>
            <p>{perspective.body}</p>
          </div>
          <div className="perspective-nav" role="tablist" aria-label="Product perspectives">
            {perspectives.map((item, index) => (
              <button
                type="button"
                role="tab"
                id={`perspective-tab-${index}`}
                aria-controls="perspective-panel"
                aria-selected={active === index}
                tabIndex={active === index ? 0 : -1}
                className={active === index ? "is-active" : ""}
                onClick={() => setActive(index)}
                onKeyDown={(event) => activateTabFromKey(event, index, perspectives.length, setActive)}
                key={item.role}
              >
                <span>{item.role}</span>
                <i aria-hidden="true" />
              </button>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

function Billing() {
  return (
    <section className="section billing-section" id="billing">
      <div className="container">
        <div className="section-heading">
          <p className="eyebrow"><span />Billing and payments</p>
          <h2>Start with two projects.<br /><em>Charge for what grows.</em></h2>
          <p className="lead">FFDB keeps the organization plan that pays for FFDB separate from commerce inside a customer project. Platform usage billing and project commerce are both implemented: each project can use encrypted BYO Stripe credentials or optional Connect direct charges for products, immutable prices, one-time purchases, memberships, refunds, entitlements, and paid-order fulfillment.</p>
        </div>
        <div className="deployment-grid pricing-grid">
          {billingPlans.map((plan, index) => (
            <article className={index === 2 ? "featured" : ""} key={plan.name}>
              {index === 2 && <span className="card-label">Predictable usage</span>}
              <span className="feature-number">{plan.number}</span>
              <h3>{plan.name}</h3>
              <p>{plan.description}</p>
              <strong className="deployment-price">{plan.price}<small>{plan.note}</small></strong>
              <ul>{plan.features.map((feature) => <li key={feature}><span>✓</span>{feature}</li>)}</ul>
              <a className={index === 2 ? "button button-dark" : "button button-outline"} href="/docs/billing/platform">Billing model <Arrow /></a>
            </article>
          ))}
        </div>
        <p className="pricing-disclaimer">No free organization is charged automatically. The owner chooses Stripe BYO or Connect during instance setup, FFDB provisions the plan catalog, and verified webhooks—not browser redirects—change plan entitlements. The two-project Free cap and automated read, write, logical-storage, storage byte-hour, and MAU metering are enforced through the durable reporting and reconciliation pipeline.</p>
      </div>
    </section>
  );
}

function Deployment() {
  return (
    <section className="section deployment-section" id="deployment">
      <div className="container">
        <div className="section-heading">
          <p className="eyebrow"><span />Deployment</p>
          <h2>Install the product.<br /><em>Keep the source optional.</em></h2>
          <p className="lead">Choose a tagged GitHub Release for a reproducible server install. Its bundle pins images, verifies signatures, installs the gateway and services, and supplies the complete lifecycle command.</p>
        </div>
        <div className="deployment-grid">
          {deploymentShapes.map((shape, index) => (
            <article className={index === 1 ? "featured" : ""} key={shape.name}>
              {index === 1 && <span className="card-label">Production path</span>}
              <span className="feature-number">{shape.number}</span>
              <h3>{shape.name}</h3>
              <p>{shape.description}</p>
              <strong className="deployment-price">{shape.price}</strong>
              <ul>{shape.features.map((feature) => <li key={feature}><span>✓</span>{feature}</li>)}</ul>
              <a className={index === 1 ? "button button-dark" : "button button-outline"} href="/docs/self-hosting">Deployment guide <Arrow /></a>
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}

function FinalCta() {
  return (
    <section className="section final-section">
      <div className="container">
        <div className="final-card">
          <div>
            <h2>Give your app a backend.<br />Keep control of the data.</h2>
            <p>Install a release, create an organization and project, choose its billing policy, apply its data rules, and make the first query from your application.</p>
            <div className="button-row"><a className="button button-dark" href="/docs/install/docker">Install FFDB <Arrow /></a><a className="button button-outline" href="/docs/quickstart">Build the first project</a></div>
            <small>Apache-2.0 · Release bundle · Platform billing · Project commerce</small>
          </div>
          <div className="tetrahedron" aria-hidden="true"><span /><span /><span /><i /></div>
        </div>
      </div>
    </section>
  );
}

function Footer() {
  const groups = [
    ["Product", [["Capabilities", "#capabilities"], ["Architecture", "#architecture"], ["Security", "#security"], ["Billing", "#billing"], ["Deployment", "#deployment"]]],
    ["Developers", [["Documentation", "/docs/"], ["Quickstart", "/docs/quickstart"], ["HTTP API", "/docs/reference/http-api"], ["Client packages", "/docs/client"]]],
    ["Operations", [["Install FFDB", "/docs/install/docker"], ["Self-hosting", "/docs/self-hosting"], ["Backups", "/docs/backups"], ["Observability", "/docs/observability"]]],
    ["Legal", [["Terms", "/terms/"], ["Privacy", "/privacy/"], ["Security & disclaimer", "/security/"], ["Apache-2.0", "https://www.apache.org/licenses/LICENSE-2.0"]]],
  ] as const;
  return (
    <footer>
      <div className="footer-wave" aria-hidden="true" />
      <div className="container footer-grid">
        <div className="footer-brand"><a className="brand" href="/">FFDB</a><p>A self-hostable Rust data platform with one hardened SQLite database per project.</p></div>
        {groups.map(([title, links]) => (
          <div className="footer-links" key={title}><h3>{title}</h3>{links.map(([name, href]) => <a href={href} key={name}>{name}{href.startsWith("http") ? " ↗" : ""}</a>)}</div>
        ))}
      </div>
      <div className="container footer-bottom"><span>© 2026 Forever Frameworks LLC.</span><span>SQLite at the edge of your trust boundary.</span></div>
    </footer>
  );
}

export function App() {
  const pageRef = useRef<HTMLDivElement>(null);

  useGSAP(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;

    gsap.from(".hero-title-line", {
      yPercent: 110,
      opacity: 0,
      duration: 1.15,
      stagger: 0.12,
      ease: "power4.out",
    });

    gsap.from(".hero-product-window", {
      width: 0,
      duration: 1.1,
      delay: 0.45,
      ease: "expo.out",
    });

    gsap.fromTo(".scrub-word", { opacity: 0.18 }, {
      opacity: 1,
      stagger: 0.08,
      ease: "none",
      scrollTrigger: {
        trigger: ".control-narrative",
        start: "top 65%",
        end: "center 35%",
        scrub: true,
      },
    });

    ScrollTrigger.create({
      trigger: ".control-narrative",
      start: "top top",
      end: "bottom bottom",
      pin: ".narrative-sticky",
      pinSpacing: false,
    });
  }, { scope: pageRef });

  return (
    <div ref={pageRef}>
      <a className="skip-link" href="#main">Skip to content</a>
      <Navigation />
      <main id="main">
        <Hero />
        <Capabilities />
        <Workflow />
        <ControlNarrative />
        <Architecture />
        <Facts />
        <Integrations />
        <Security />
        <Developers />
        <Perspectives />
        <Billing />
        <Deployment />
        <FinalCta />
      </main>
      <Footer />
    </div>
  );
}

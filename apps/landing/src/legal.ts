export interface LegalSection {
  readonly heading: string;
  readonly paragraphs?: readonly string[];
  readonly bullets?: readonly string[];
}

export interface LegalPage {
  readonly path: "/terms" | "/privacy" | "/security";
  readonly title: string;
  readonly description: string;
  readonly eyebrow: string;
  readonly introduction: string;
  readonly sections: readonly LegalSection[];
}

export const publicOrigin = "https://ffdb.forever-frameworks.com";
export const repositoryUrl = "https://github.com/Forever-Frameworks-LLC/ffdb";
export const contactEmail = "admin@forever-frameworks.com";
export const legalEffectiveDate = "August 3, 2026";

export const legalPages: readonly LegalPage[] = [
  {
    path: "/terms",
    title: "Terms of Use",
    description: "Terms governing the FFDB public website, documentation, release channels, and any service that expressly links to these terms.",
    eyebrow: "Public terms",
    introduction: "These Terms of Use govern access to the FFDB public website, documentation, official release and package channels, and any Forever Frameworks service that expressly links to them. The Apache-2.0 license separately governs the FFDB software itself.",
    sections: [
      {
        heading: "1. Who we are and what these terms cover",
        paragraphs: [
          "Forever Frameworks LLC (\"Forever Frameworks,\" \"we,\" or \"us\") publishes FFDB. In these terms, the \"Public Services\" are the FFDB website, documentation, official download and package-distribution surfaces, and any hosted feature that expressly links to these terms. \"FFDB Software\" means the source code and release artifacts licensed under Apache-2.0.",
          "A person or company operating an FFDB deployment is the operator of that deployment. These terms do not replace the operator's own terms with its users, and using self-hosted FFDB does not make Forever Frameworks the operator of that instance.",
        ],
      },
      {
        heading: "2. Acceptance and authority",
        paragraphs: [
          "By using the Public Services, you agree to these terms. If you use them for an organization, you represent that you have authority to bind that organization. If you do not agree, do not use the Public Services.",
          "You must be legally able to enter into these terms. The Public Services are not directed to children under 13, and a minor may use them only with permission and supervision required by applicable law.",
        ],
      },
      {
        heading: "3. Open-source license and releases",
        paragraphs: [
          "FFDB Software is offered under the Apache License 2.0. That license controls your rights to use, reproduce, modify, and distribute the software and includes its own warranty and liability terms. These website terms do not narrow rights granted by Apache-2.0.",
          "Use only release artifacts and package identities linked from the official repository and documentation. You are responsible for verifying checksums and signatures, reviewing release notes, preserving backups, and testing upgrades in your environment.",
        ],
      },
      {
        heading: "4. Accounts, instance owners, and credentials",
        paragraphs: [
          "If a Public Service requires an account, you must provide accurate information, protect credentials and recovery material, and promptly report suspected compromise. You are responsible for activity performed through credentials under your control unless applicable law provides otherwise.",
          "The first owner of a self-hosted FFDB instance has broad administrative authority over that instance. Instance owners control organizations, users, projects, billing modes, provider credentials, retention, and access. Deployers must give their own users appropriate notice and must not present Forever Frameworks as the operator of their deployment.",
        ],
      },
      {
        heading: "5. Your responsibilities",
        bullets: [
          "Comply with laws, contracts, licenses, and rights that apply to your use and data.",
          "Keep PostgreSQL, project databases, object storage, backups, secrets, and administrative endpoints appropriately protected.",
          "Configure authentication, authorization policies, TLS, email, storage, Stripe, and other providers for your actual threat model.",
          "Maintain tested backups, incident procedures, monitoring, and an upgrade process for supported security fixes.",
          "Obtain all permissions needed for data, software, and content you place in an FFDB deployment.",
        ],
      },
      {
        heading: "6. Acceptable use",
        paragraphs: ["You may not use the Public Services to interfere with other users, distribute malware, evade access controls, misrepresent identity or affiliation, infringe rights, violate law, or perform unauthorized testing. Do not send credentials, production records, personal data, or exploit details through a public issue."]
      },
      {
        heading: "7. Third-party services",
        paragraphs: [
          "FFDB can connect to independently operated services such as PostgreSQL, S3-compatible storage, email delivery, container registries, GitHub, npm, and Stripe. Their terms, privacy practices, availability, fees, and account requirements are controlled by those providers.",
          "An integration or link does not make Forever Frameworks responsible for a third-party service. You decide which providers to configure and are responsible for the credentials, permissions, webhook boundaries, and data flows you enable.",
        ],
      },
      {
        heading: "8. Paid services",
        paragraphs: [
          "Open-source self-hosting does not itself create a payment obligation to Forever Frameworks. If we later offer a paid service and you purchase it, the price, billing interval, usage measure, renewal, cancellation path, taxes, refund terms, and any service-specific commitments shown in the order or checkout will apply. A signed order or service-specific terms control if they conflict with this section.",
          "Project commerce and Stripe Connect features let a deployment operator or project sell to its own customers. Forever Frameworks is not the merchant of record for transactions processed through an operator's or project's payment account unless a separate written agreement expressly says otherwise.",
        ],
      },
      {
        heading: "9. Feedback, trademarks, and site content",
        paragraphs: [
          "You may provide feedback without restriction or payment obligation, and we may use it to improve FFDB. Do not submit material you lack permission to share.",
          "Apache-2.0 covers the FFDB Software as stated in the repository. Forever Frameworks names, logos, and branding are not licensed for misleading endorsement. Site copy, artwork, and materials outside the licensed repository remain protected by applicable intellectual-property law.",
        ],
      },
      {
        heading: "10. Changes, suspension, and availability",
        paragraphs: [
          "We may change the Public Services, release channels, documentation, or these terms. Material term changes will be identified by a new effective date. Continuing to use a Public Service after updated terms take effect means you accept them where permitted by law.",
          "We may restrict access needed to protect the Public Services, users, or others; respond to legal requirements; or address misuse. No uptime, support-response, maintenance, compatibility, or data-retention commitment applies unless stated in a separate written agreement.",
        ],
      },
      {
        heading: "11. Disclaimers",
        paragraphs: [
          "TO THE MAXIMUM EXTENT PERMITTED BY LAW, THE PUBLIC SERVICES AND FFDB SOFTWARE ARE PROVIDED \"AS IS\" AND \"AS AVAILABLE.\" FOREVER FRAMEWORKS DISCLAIMS IMPLIED WARRANTIES, INCLUDING MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE, TITLE, AND NON-INFRINGEMENT.",
          "Documentation, examples, security controls, billing logic, migration tools, and backup procedures require validation in your environment. FFDB has no stated security certification, and no documentation is a promise that a deployment is secure, compliant, uninterrupted, or error-free.",
        ],
      },
      {
        heading: "12. Limitation of liability",
        paragraphs: [
          "TO THE MAXIMUM EXTENT PERMITTED BY LAW, FOREVER FRAMEWORKS WILL NOT BE LIABLE FOR INDIRECT, INCIDENTAL, SPECIAL, CONSEQUENTIAL, EXEMPLARY, OR PUNITIVE DAMAGES, OR FOR LOST PROFITS, REVENUE, DATA, GOODWILL, OR BUSINESS INTERRUPTION, ARISING FROM THE PUBLIC SERVICES OR FFDB SOFTWARE.",
          "Where liability cannot be excluded, Forever Frameworks' aggregate liability for the event giving rise to a claim will not exceed the amount you paid Forever Frameworks for the applicable Public Service during the 12 months before the event, or USD 100 if you paid nothing. Some jurisdictions do not allow certain exclusions, so these limits apply only to the extent allowed.",
        ],
      },
      {
        heading: "13. Indemnity",
        paragraphs: ["To the extent permitted by law, you will defend and indemnify Forever Frameworks and its personnel from third-party claims arising from your deployment, data, violation of these terms, infringement of another's rights, or unlawful use. This does not apply to the extent a claim was caused by Forever Frameworks' own unlawful conduct."],
      },
      {
        heading: "14. General terms and contact",
        paragraphs: [
          "If a provision is unenforceable, the remaining provisions continue in effect. A failure to enforce a provision is not a waiver. You may not assign obligations under these terms without consent, but we may assign them in connection with a reorganization, merger, acquisition, or sale of relevant assets. These terms, the Apache-2.0 license, and any applicable service-specific written terms form the agreement for their respective subject matter.",
          `Questions about these terms may be sent to ${contactEmail}. For product defects, use the official repository issue tracker; disclose suspected vulnerabilities privately as described on the Security page.`,
        ],
      },
    ],
  },
  {
    path: "/privacy",
    title: "Privacy Notice",
    description: "How Forever Frameworks handles information on the FFDB public website, documentation, release channels, and direct communications.",
    eyebrow: "Public privacy",
    introduction: "This notice describes information handled by Forever Frameworks on the FFDB public website, documentation, official distribution surfaces, and direct communications. A self-hosted FFDB operator controls the data in its own deployment.",
    sections: [
      {
        heading: "1. Scope and roles",
        paragraphs: [
          "This notice applies to ffdb.forever-frameworks.com and Forever Frameworks' direct administration of FFDB public communications. It does not govern an independently operated FFDB instance, a project built on FFDB, or a third-party site linked from our pages.",
          "Installing FFDB does not automatically send project databases, user accounts, object contents, organization metrics, or payment-provider credentials to Forever Frameworks. The person or organization deploying an instance determines why and how that instance processes information and must provide its own privacy notice.",
        ],
      },
      {
        heading: "2. Information you provide",
        bullets: [
          "Contact details and message contents when you email or otherwise contact us.",
          "Repository issues, discussions, pull requests, and other contributions you choose to publish through GitHub.",
          "Account, order, support, or billing information if you use a future service that expressly requests it.",
          "Security-report details sent through a private reporting channel; reports must not include unnecessary credentials, production data, or personal information.",
        ],
      },
      {
        heading: "3. Information produced by access",
        paragraphs: [
          "Hosting and network systems may process standard request information such as timestamp, requested URL, response status, IP address, user agent, referrer, and security or rate-limit events. We use this information to deliver, protect, troubleshoot, and understand aggregate operation of the public surfaces.",
          "The landing and documentation applications in the published FFDB source do not intentionally include advertising pixels or third-party analytics scripts. The documentation theme preference is stored in your browser's local storage and is not a tracking identifier sent to Forever Frameworks by that feature.",
        ],
      },
      {
        heading: "4. Official distribution and third parties",
        paragraphs: [
          "GitHub, npm, container registries, payment providers, email providers, and other linked services process information under their own notices. Downloading or interacting with an artifact through those services may give that provider account, request, or telemetry data.",
          "When an FFDB operator configures Stripe, S3-compatible storage, email, or another provider, the operator and provider control those deployment data flows. Forever Frameworks does not receive them merely because FFDB contains the integration.",
        ],
      },
      {
        heading: "5. How we use information",
        bullets: [
          "Provide, secure, maintain, and troubleshoot the Public Services and release channels.",
          "Respond to communications, support questions, contributions, and vulnerability reports.",
          "Prevent fraud, abuse, unauthorized access, malware, and threats to users or infrastructure.",
          "Operate any account or paid service you request and keep required business records.",
          "Comply with legal obligations and establish, exercise, or defend legal claims.",
          "Improve documentation and product decisions using bounded, aggregate operational information.",
        ],
      },
      {
        heading: "6. Disclosure",
        paragraphs: [
          "We may disclose information to infrastructure, security, communications, payment, and professional-service providers acting for us; to a successor in a corporate transaction; when required by law or valid process; or when reasonably needed to protect rights, safety, integrity, and users. Providers should receive only information needed for their function.",
          "Public repository contributions and issue comments are visible according to the settings of the repository service. We do not treat information intentionally posted in a public channel as confidential.",
        ],
      },
      {
        heading: "7. Sale and advertising",
        paragraphs: ["The FFDB public website and documentation are not designed to sell personal information or support cross-site behavioral advertising. If that practice changes, this notice and any legally required controls must be updated before the change is introduced."],
      },
      {
        heading: "8. Retention",
        paragraphs: ["We retain information for only as long as reasonably needed for the purposes above, including security, release integrity, support history, legal obligations, dispute resolution, and business records. Retention varies by record and system. Public repository history may remain according to the repository provider's operation and open-source recordkeeping."],
      },
      {
        heading: "9. Security",
        paragraphs: ["We use measures appropriate to the nature of the public surfaces, but no network, storage system, or transmission is completely secure. Do not send secrets or production records through public channels. Follow the Security page for private vulnerability reporting."],
      },
      {
        heading: "10. International processing",
        paragraphs: ["Internet services and their providers may process information in countries other than the one where you live. Laws and protections can differ by location. Where a transfer mechanism is legally required for a service we control, we will use an applicable mechanism."],
      },
      {
        heading: "11. Children",
        paragraphs: ["The Public Services are intended for software developers, operators, and organizations and are not directed to children under 13. If you believe a child provided personal information directly to Forever Frameworks, contact us so we can evaluate and delete it where appropriate."],
      },
      {
        heading: "12. Your choices and requests",
        paragraphs: [
          "You can avoid optional direct communications, remove the local documentation theme preference through browser controls, and choose whether to use third-party distribution services. Depending on where you live and applicable exceptions, you may have rights to access, correct, delete, restrict, or obtain a copy of information Forever Frameworks controls, or to object or complain to a regulator.",
          `Send a privacy request to ${contactEmail} with enough information to identify the relevant interaction. We may need to verify the request and cannot act on data controlled solely by an independent FFDB operator or third-party provider.`,
        ],
      },
      {
        heading: "13. Changes and contact",
        paragraphs: [
          "We may update this notice as the public surfaces or practices change. The effective date identifies the current version. Material changes should be described on the relevant public surface before or when they take effect.",
          `Forever Frameworks LLC is the contact for this notice. Email ${contactEmail}. If your question concerns data in a separately operated FFDB instance, contact that instance's operator instead.`,
        ],
      },
    ],
  },
  {
    path: "/security",
    title: "Security and Product Disclaimer",
    description: "FFDB security boundaries, responsible vulnerability reporting, operator duties, and explicit limits on security and compliance claims.",
    eyebrow: "Security disclosure",
    introduction: "FFDB is designed around explicit authorization and isolation boundaries, but safe operation still depends on deployment configuration, providers, patching, backups, and application policy. This page states the public reporting path and the claims FFDB does not make.",
    sections: [
      {
        heading: "1. The documented security model",
        paragraphs: [
          "FFDB keeps organization and project control-plane state in PostgreSQL, uses a separate SQLite application database per project, constrains SQL in isolated workers, applies supported row-level policies, and authorizes S3-compatible operations through short-lived provider requests. Authentication, sync, backup, billing, provider, proxy, and administrative paths remain part of the security boundary.",
          "The production-security documentation and repository threat model describe the intended boundaries. They are engineering inputs, not a certification or a guarantee that any particular deployment is secure.",
        ],
      },
      {
        heading: "2. Operator responsibility",
        bullets: [
          "Terminate TLS at a reviewed boundary and keep PostgreSQL, worker IPC, direct Axum diagnostics, project files, metrics, and backups private.",
          "Generate independent secrets, protect provider credentials, restrict administration, and rotate credentials after suspected exposure.",
          "Configure exact storage origins, CORS, email, Stripe webhooks, trusted proxies, limits, and row-level policies for the deployment.",
          "Verify signed releases, apply supported security updates, and test rollback before an internet-facing upgrade.",
          "Run adversarial isolation, authorization, backup, restore, and provider-boundary acceptance tests against the deployed topology.",
        ],
      },
      {
        heading: "3. Report a suspected vulnerability privately",
        paragraphs: [
          `Use the official GitHub repository's private vulnerability-reporting or security-advisory channel when it is available. If that channel is unavailable, email ${contactEmail} with \"FFDB security\" in the subject. Do not open a public issue until the report has been assessed and coordinated disclosure is agreed.`,
          "Include the affected release or commit, prerequisite configuration, reproducible steps, impact, and a suggested mitigation if known. Remove credentials, access tokens, signed URLs, customer records, and production database content. Use synthetic data and the minimum evidence needed to reproduce the problem.",
        ],
      },
      {
        heading: "4. Research boundaries",
        paragraphs: ["Do not access another person's account or deployment, impair availability, run denial-of-service tests, use social engineering, send malware, persist after proving impact, or exfiltrate data. Testing must stay within systems you own or have explicit permission to assess. This page does not create a bug bounty, payment promise, safe-harbor commitment, or response deadline."],
      },
      {
        heading: "5. Disclosure process",
        paragraphs: ["We aim to acknowledge enough information to begin triage, reproduce valid reports, develop and test a fix, identify affected releases, and coordinate publication. Complexity, incomplete reproduction, dependency ownership, and release safety can affect timing. Do not assume a report is accepted or a fix is complete until that is confirmed through the private channel or an official release advisory."],
      },
      {
        heading: "6. Supported versions and authentic artifacts",
        paragraphs: [
          "The repository SECURITY.md and official release notes are the source of truth for supported versions and security fixes. Do not infer support from an npm version, container tag, branch, fork, cached documentation page, or similarly named package.",
          "Install through official tagged releases, verify checksums and Sigstore material, keep immutable version pins, and review the release manifest. Report suspicious packages, signatures, download redirects, or repository impersonation privately.",
        ],
      },
      {
        heading: "7. Secrets and reports",
        paragraphs: ["Never place bootstrap tokens, signing material, Stripe or storage secrets, JWT keys, database credentials, backups, or live user information in issues, screenshots, logs, or example projects. Revoke and rotate any secret included in a report; deleting a message does not guarantee the value was never copied or logged."],
      },
      {
        heading: "8. No certification or compliance claim",
        paragraphs: ["FFDB does not claim SOC 2, ISO 27001, PCI DSS, HIPAA, FedRAMP, or another formal certification in the current repository materials. Features such as encryption support, row-level policy enforcement, audit-oriented records, signed releases, or Stripe integration do not make an operator compliant. Compliance depends on the complete deployed system, policies, people, contracts, providers, and evidence."],
      },
      {
        heading: "9. Product and security disclaimer",
        paragraphs: [
          "FFDB Software is provided under Apache-2.0, including that license's warranty and liability terms. Public documentation and examples are provided for general engineering information and may not cover every threat, dependency failure, migration, jurisdiction, or application requirement.",
          "No security control eliminates all risk. You remain responsible for architecture review, data classification, legal requirements, capacity limits, application logic, incident response, provider configuration, and recovery testing. Do not use a passing test suite or a statement on this site as the sole basis for a high-risk deployment decision.",
        ],
      },
      {
        heading: "10. Security resources",
        bullets: [
          "Production security guide: /docs/security",
          `Repository security policy: ${repositoryUrl}/blob/main/SECURITY.md`,
          `Threat model: ${repositoryUrl}/blob/main/docs/threat-model/threat-model.md`,
          `Release history and advisories: ${repositoryUrl}/releases`,
          `Private contact fallback: ${contactEmail}`,
        ],
      },
    ],
  },
] as const;

export function legalPageForPath(pathname: string): LegalPage | undefined {
  const path = pathname.replace(/\/+$/, "") || "/";
  return legalPages.find((page) => page.path === path);
}

export function renderLegalPage(page: LegalPage): string {
  const canonical = `${publicOrigin}${page.path}/`;
  const title = `${page.title} — FFDB`;
  const structuredData = safeJson({
    "@context": "https://schema.org",
    "@type": "WebPage",
    name: page.title,
    description: page.description,
    url: canonical,
    isPartOf: { "@type": "WebSite", name: "FFDB", url: publicOrigin },
    publisher: { "@type": "Organization", name: "Forever Frameworks LLC" },
    dateModified: "2026-08-03",
  });

  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <meta name="theme-color" content="#f8f7f3" />
    <meta name="description" content="${escapeHtml(page.description)}" />
    <meta name="robots" content="index,follow,max-image-preview:large" />
    <link rel="canonical" href="${canonical}" />
    <link rel="license" href="https://www.apache.org/licenses/LICENSE-2.0" />
    <meta property="og:type" content="website" />
    <meta property="og:site_name" content="FFDB" />
    <meta property="og:title" content="${escapeHtml(title)}" />
    <meta property="og:description" content="${escapeHtml(page.description)}" />
    <meta property="og:url" content="${canonical}" />
    <meta property="og:image" content="${publicOrigin}/social-card.jpg" />
    <meta name="twitter:card" content="summary_large_image" />
    <meta name="twitter:title" content="${escapeHtml(title)}" />
    <meta name="twitter:description" content="${escapeHtml(page.description)}" />
    <meta name="twitter:image" content="${publicOrigin}/social-card.jpg" />
    <script type="application/ld+json">${structuredData}</script>
    <link rel="stylesheet" href="/legal.css" />
    <title>${escapeHtml(title)}</title>
  </head>
  <body>
    <a class="skip-link" href="#main">Skip to content</a>
    <header class="legal-header">
      <nav class="legal-nav" aria-label="Primary navigation">
        <a class="brand" href="/" aria-label="FFDB home">FFDB</a>
        <div><a href="/docs/">Documentation</a><a href="${repositoryUrl}">Source ↗</a></div>
      </nav>
    </header>
    <main id="main" class="legal-main">
      <header class="legal-hero">
        <p class="eyebrow"><span></span>${escapeHtml(page.eyebrow)}</p>
        <h1>${escapeHtml(page.title)}</h1>
        <p class="legal-intro">${escapeHtml(page.introduction)}</p>
        <p class="effective">Effective ${legalEffectiveDate}</p>
      </header>
      <div class="legal-layout">
        <aside aria-label="On this page"><strong>On this page</strong>${page.sections.map((section, index) => `<a href="#section-${index + 1}">${escapeHtml(section.heading)}</a>`).join("")}</aside>
        <article>
          ${page.sections.map((section, index) => renderSection(section, index)).join("\n")}
        </article>
      </div>
    </main>
    <footer>
      <div class="legal-footer-grid">
        <div><a class="brand" href="/">FFDB</a><p>A self-hostable Rust data platform published by Forever Frameworks LLC.</p></div>
        <nav aria-label="Legal links"><a href="/terms/">Terms</a><a href="/privacy/">Privacy</a><a href="/security/">Security &amp; disclaimer</a><a href="https://www.apache.org/licenses/LICENSE-2.0">Apache-2.0 ↗</a></nav>
      </div>
      <div class="legal-footer-bottom"><span>© 2026 Forever Frameworks LLC.</span><a href="mailto:${contactEmail}">${contactEmail}</a></div>
    </footer>
  </body>
</html>`;
}

function renderSection(section: LegalSection, index: number): string {
  const paragraphs = section.paragraphs?.map((paragraph) => `<p>${renderInlineLinks(paragraph)}</p>`).join("") ?? "";
  const bullets = section.bullets === undefined ? "" : `<ul>${section.bullets.map((bullet) => `<li>${renderInlineLinks(bullet)}</li>`).join("")}</ul>`;
  return `<section id="section-${index + 1}"><h2>${escapeHtml(section.heading)}</h2>${paragraphs}${bullets}</section>`;
}

function renderInlineLinks(value: string): string {
  const escaped = escapeHtml(value);
  return escaped
    .replaceAll(contactEmail, `<a href="mailto:${contactEmail}">${contactEmail}</a>`)
    .replace(/https:\/\/github\.com\/Forever-Frameworks-LLC\/ffdb(?:\/[A-Za-z0-9._~:/?#\[\]@!$&amp;'()*+,;=%-]*)?/gu, (url) => `<a href="${url.replaceAll("&amp;", "&")}">${url}</a>`)
    .replace(/(^|\s)(\/docs\/[a-z0-9/-]+)/gu, (_match, prefix: string, path: string) => `${prefix}<a href="${path}">${path}</a>`);
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

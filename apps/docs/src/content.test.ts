import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { navigation, normalizePath, pageByPath, pages, searchPages } from "./content";
import { clientClassSections, clientTypeSections, cliCommandSections, cliModuleSections, httpOperationSections } from "./generated-reference";
import { highlightCode, supportedLanguages } from "./syntax";

describe("documentation information architecture", () => {
  it("resolves every navigation link", () => {
    for (const group of navigation) {
      for (const link of group.links) expect(pageByPath.has(link.href), link.href).toBe(true);
    }
  });

  it("makes Docker Compose and systemd first-class install routes", () => {
    const install = navigation.find((group) => group.title === "Install");
    expect(install?.links.slice(0, 2)).toEqual([
      { title: "Docker Compose", href: "/install/docker" },
      { title: "systemd", href: "/install/systemd" },
    ]);
    expect(pageByPath.get("/install/docker")?.sections.length).toBeGreaterThanOrEqual(5);
    expect(pageByPath.get("/install/systemd")?.sections.length).toBeGreaterThanOrEqual(7);
    const systemd = JSON.stringify(pageByPath.get("/install/systemd"));
    expect(systemd).toContain('tar -xzf \\"ffdb-native-linux-amd64-$VERSION.tar.gz\\"');
    expect(systemd).toContain('cd \\"ffdb-native-$VERSION\\"');
    expect(systemd).not.toContain('cd \\"ffdb-native-linux-amd64-$VERSION\\"');
  });

  it("keeps release installation examples aligned with the packaged installer and units", () => {
    for (const path of ["/install/docker"]) {
      const samples = pageByPath.get(path)?.sections
        .flatMap((section) => [section.code, ...(section.codes ?? [])])
        .filter((sample) => sample !== undefined) ?? [];
      const airGapped = samples.filter((sample) => sample.code.includes("FFDB_RELEASE_BASE_URL=file://"));
      expect(airGapped.length, path).toBeGreaterThan(0);
      for (const sample of airGapped) {
        expect(sample.code, path).toContain("sh ./install.sh --profile single-host --start --require-signature");
      }
    }

    const systemdPage = pageByPath.get("/install/systemd");
    const systemdSamples = systemdPage?.sections
      .flatMap((section) => [section.code, ...(section.codes ?? [])])
      .filter((sample) => sample !== undefined) ?? [];
    const stagedEnvironmentIndex = systemdSamples.findIndex((sample) =>
      sample.code.includes("FFDB_S3_PUBLIC_ORIGIN=https://s3.us-east-1.amazonaws.com"),
    );
    const installIndex = systemdSamples.findIndex((sample) =>
      sample.code.includes("sudo ./install-native.sh --verified-release --env-file /root/ffdb.env"),
    );
    expect(stagedEnvironmentIndex).toBeGreaterThanOrEqual(0);
    expect(installIndex).toBeGreaterThan(stagedEnvironmentIndex);

    const nativeInstaller = readFileSync(new URL("../../../infra/release/native/install-native.sh", import.meta.url), "utf8");
    const systemdEnvironment = readFileSync(new URL("../../../infra/systemd/ffdb.env.example", import.meta.url), "utf8");
    const systemdService = readFileSync(new URL("../../../infra/systemd/ffdb-api.service", import.meta.url), "utf8");
    const gateway = readFileSync(new URL("../../../infra/systemd/ffdb-gateway.Caddyfile", import.meta.url), "utf8");
    const gatewayService = readFileSync(new URL("../../../infra/systemd/ffdb-gateway.service", import.meta.url), "utf8");
    expect(nativeInstaller).toContain('"FFDB_S3_PUBLIC_ORIGIN"');
    expect(nativeInstaller).toContain("FFDB_S3_PUBLIC_ORIGIN must be an exact HTTPS origin before installing Caddy");
    expect(nativeInstaller).toContain("FFDB_PUBLIC_BASE_URL must be an exact HTTPS origin before installing Caddy");
    expect(nativeInstaller).toContain('"$bundle_dir/systemd/ffdb-gateway.Caddyfile"');
    expect(systemdEnvironment).toContain("FFDB_S3_PUBLIC_ORIGIN=https://s3.example.com");
    expect(systemdEnvironment).toContain("FFDB_TRUSTED_PROXY_CIDRS=127.0.0.1/32,::1/128");
    expect(gateway).toContain("connect-src 'self' https://s3.example.com");
    expect(gateway).toContain("reverse_proxy 127.0.0.1:8080");
    expect(gatewayService).toContain("AmbientCapabilities=CAP_NET_BIND_SERVICE");

    const readWritePaths = systemdService.match(/^ReadWritePaths=.*$/m)?.[0];
    expect(readWritePaths).toBe("ReadWritePaths=/var/lib/ffdb/projects /var/lib/ffdb/backups /var/lib/ffdb/metrics");
    expect(JSON.stringify(systemdPage)).toContain(readWritePaths);
  });

  it("keeps runnable client, HTTP query, and migration examples aligned with public code", () => {
    const clientSource = readFileSync(new URL("../../../packages/client/src/client.ts", import.meta.url), "utf8");
    const clientTypes = readFileSync(new URL("../../../packages/client/src/types.ts", import.meta.url), "utf8");
    const clientReference = JSON.stringify(pageByPath.get("/reference/client"));
    expect(clientSource).toContain(
      "async signIn(email: string, password: string, options: RequestOptions = {}): Promise<AuthTokenPair>",
    );
    expect(clientSource).toContain(
      "pull(cursor: string | null, limit = 1_000, options: RequestOptions = {}): Promise<SyncPullResponse>",
    );
    expect(clientSource).toContain(
      "async instanceSetupStatus(options: RequestOptions = {}): Promise<PublicInstanceSetupStatus>",
    );
    expect(clientTypes).toContain("readonly setup_required: boolean;");
    expect(clientReference).toContain("ffdb.auth.signIn(email, password)");
    expect(clientReference).toContain("ffdb.sync.pull(null, 100)");
    expect(clientReference).toContain("instance.setup_required");
    expect(clientReference).not.toContain("ffdb.auth.signIn({ email, password })");
    expect(clientReference).not.toContain("ffdb.sync.pull({ cursor:");
    expect(clientReference).not.toContain("instance.setup_complete");

    const apiSource = readFileSync(new URL("../../../apps/api/src/lib.rs", import.meta.url), "utf8");
    const queryRoute = apiSource.match(/\.route\("(\/v1\/projects\/\{project_id\}\/query)", post\(query\)\)/)?.[1];
    expect(queryRoute).toBe("/v1/projects/{project_id}/query");
    const documentedQueryRoute = `$FFDB_URL${queryRoute?.replace("{project_id}", "$FFDB_PROJECT_ID")}`;
    const httpReference = JSON.stringify(pageByPath.get("/reference/http-api"));
    expect(httpReference).toContain(documentedQueryRoute);
    expect(httpReference).not.toContain("/database/query");

    const cliSource = readFileSync(new URL("../../../packages/cli/src/main.ts", import.meta.url), "utf8");
    expect(cliSource).toContain('const path = `${Date.now()}_${name}.sql`;');
    const migrations = JSON.stringify(pageByPath.get("/migrations"));
    expect(migrations).toContain("migrations/1754179200000_add_documents.sql");
    expect(migrations).toContain("migration create add_documents");
    expect(migrations).toContain("migration apply 1754179200000_add_documents.sql");
    expect(migrations).toContain("<epoch-milliseconds>_add_documents.sql");
    expect(migrations).not.toContain("20260802_documents.sql");
  });

  it("uses current package names and removes legacy hosted-client examples", () => {
    const serialized = JSON.stringify(pages);
    for (const name of ["@ffdb/client", "@ffdb/react", "@ffdb/react-native", "@ffdb/sync-client", "@ffdb/cli"]) {
      expect(serialized).toContain(name);
    }
    expect(serialized).not.toContain('from "@ffdb/client"');
    expect(serialized).not.toContain('"@ffdb/client":');
    expect(serialized).not.toContain("CRDT");
    expect(serialized).not.toContain("where owner_id = ?1");
  });

  it("uses public scoped package identities and version-matched artifacts", () => {
    const clientPage = pageByPath.get("/client");
    const cliPage = pageByPath.get("/cli");
    const client = JSON.stringify(clientPage);
    const cli = JSON.stringify(cliPage);
    const clientSamples = clientPage?.sections.flatMap((section) => [section.code, ...(section.codes ?? [])]) ?? [];
    expect(client).toContain("exact version named by the server release");
    expect(clientSamples.some((sample) => sample?.code.includes("@ffdb/client@0.3.4"))).toBe(true);
    expect(client).toContain("ffdb-client-0.3.4.tgz");
    expect(cli).toContain("exact server version");
    expect(cli).toContain("@ffdb/cli@0.3.4");
    expect(JSON.stringify(pageByPath.get("/react"))).toContain("@ffdb/react@$VERSION");
    expect(JSON.stringify(pageByPath.get("/react-native"))).toContain("@ffdb/react-native@$VERSION");
    expect(JSON.stringify(pageByPath.get("/sync-client"))).toContain("@ffdb/sync-client@$VERSION");
    expect(JSON.stringify(pageByPath.get("/authentication"))).toContain("@ffdb/email-components@$VERSION");
  });

  it("keeps generated client, CLI, and HTTP references synchronized with public code", () => {
    execFileSync(process.execPath, [new URL("../../../scripts/generate-public-doc-reference.mjs", import.meta.url).pathname, "--check"], {
      cwd: new URL("../../..", import.meta.url),
      stdio: "pipe",
    });

    const client = JSON.stringify(pageByPath.get("/reference/client"));
    for (const section of [...clientClassSections, ...clientTypeSections]) expect(client).toContain(section.heading);
    expect(clientClassSections.find((section) => section.heading === "FFDBClient class")?.bullets.length).toBeGreaterThan(80);
    expect(clientTypeSections.flatMap((section) => section.bullets)).toHaveLength(166);

    const cli = JSON.stringify(pageByPath.get("/cli"));
    for (const section of [...cliCommandSections, ...cliModuleSections]) expect(cli).toContain(section.heading);
    expect(cliCommandSections.flatMap((section) => section.bullets).length).toBe(44);

    const http = JSON.stringify(pageByPath.get("/reference/http-api"));
    for (const section of httpOperationSections) expect(http).toContain(section.heading);
    expect(httpOperationSections.flatMap((section) => section.bullets)).toHaveLength(125);

    for (const path of ["../public/reference/client.md", "../public/reference/cli.md", "../public/reference/http-api.md", "../public/openapi.json"]) {
      expect(readFileSync(new URL(path, import.meta.url), "utf8").length, path).toBeGreaterThan(1_000);
    }
  });

  it("keeps contributor and unpublished framing out of end-user pages", () => {
    const endUserPages = pages.filter((page) => page.path !== "/self-hosting");
    const serialized = JSON.stringify(endUserPages).toLowerCase();
    for (const stale of ["not published", "unpublished", "release candidate", "source checkout", "repository checkout", "workspace link", "make compose-rebuild", "pnpm --filter", "clone the repository", "not a claim that the tag exists", "future homebrew"]) {
      expect(serialized, stale).not.toContain(stale);
    }
    const contributor = JSON.stringify(pageByPath.get("/self-hosting"));
    expect(contributor).toContain("Contributor source workflow");
    expect(contributor).toContain("make compose-rebuild");
  });

  it("makes the packaged host lifecycle primary and source builds contributor-only", () => {
    const serialized = JSON.stringify(pages);
    for (const contract of [
      "ffdb-compose-bundle-VERSION.tar.gz",
      "ffdb-native-linux-amd64-$VERSION.tar.gz",
      "ffdb-native-linux-ARCH-VERSION.tar.gz",
      "https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh",
      "FFDB_RELEASE_BASE_URL=file:///srv/ffdb/releases/v0.3.4",
      "sudo ffdb-host install",
      "--bundle /srv/ffdb/releases/ffdb-compose-bundle-0.3.4.tar.gz",
      "sudo ffdb-host verify",
      "sudo ffdb-host start",
      "sudo ffdb-host status",
      "sudo ffdb-host logs api",
      "sudo ffdb-host backup create",
      "sudo ffdb-host backup restore",
      "ffdb-host update-check",
      "ffdb-host update",
      "sudo ffdb-host rollback",
      "--acknowledge-migration-risk",
      "sudo ./install-native.sh",
      "/opt/ffdb",
      "/etc/ffdb/ffdb.env",
      "ExecStart=/opt/ffdb/current/bin/ffdb-api",
      "ExecStart=/opt/ffdb/current/bin/ffdb-sync-worker",
      "FFDB_DATABASE_WORKER=/opt/ffdb/current/bin/ffdb-database-worker",
      "ffdb init ../notes-app react",
      "ffdb generate --out ../notes-app/src/ffdb.types.ts",
      "NativeSQLiteReplica",
      "ReactNativeSessionStore",
    ]) expect(serialized, contract).toContain(contract);
    const normalInstallPages = JSON.stringify([pageByPath.get("/quickstart"), pageByPath.get("/install/docker"), pageByPath.get("/install/systemd")]);
    for (const contributorCommand of ["make compose-rebuild", "pnpm --filter", "cargo build --locked", "node packages/cli/dist"]) {
      expect(normalInstallPages).not.toContain(contributorCommand);
    }
    const sourceFallback = JSON.stringify(pageByPath.get("/self-hosting"));
    for (const contributorCommand of ["pnpm install --frozen-lockfile", "make build", "make verify", "make compose-rebuild"]) expect(sourceFallback).toContain(contributorCommand);
    expect(sourceFallback).toContain("Contributor source workflow");
  });

  it("documents the zero-source single-host evaluation profile without weakening production guidance", () => {
    const stableInstaller = "https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh";
    const tokenExtraction = `sudo sh -c 'umask 077; sed -n "s/^FFDB_BOOTSTRAP_TOKEN=//p" \\
  /etc/ffdb/single-host.env > /root/ffdb-bootstrap-token'`;

    for (const path of ["/quickstart"]) {
      const page = pageByPath.get(path);
      const serialized = JSON.stringify(page);
      const samples = page?.sections.flatMap((section) => [section.code, ...(section.codes ?? [])]).filter((sample) => sample !== undefined) ?? [];
      expect(samples.some((sample) => sample.code.includes(stableInstaller)), path).toBe(true);
      expect(samples.some((sample) => sample.code.includes(tokenExtraction)), path).toBe(true);
      for (const contract of [
        "sudo ffdb-host status",
        "curl --fail http://127.0.0.1:5173/readyz",
        "PostgreSQL",
        "MinIO",
        "Mailpit",
        "FFDB",
        "gateway",
        "root-only",
        "never prints",
        "loopback-only",
        "FFDB_ENVIRONMENT=development",
        "internet production",
      ]) expect(serialized, `${path}: ${contract}`).toContain(contract);
      expect(page?.sections).toHaveLength(4);
    }

    const dockerDirect = JSON.stringify(pageByPath.get("/install/docker"));
    for (const contract of [
      "name: ffdb",
      "docker compose up --detach --wait",
      "ghcr.io/forever-frameworks-llc/ffdb-runtime:0.3.4",
      "ghcr.io/forever-frameworks-llc/ffdb-gateway:0.3.4",
      "POSTGRES_PASSWORD",
      "FFDB_BOOTSTRAP_TOKEN",
      "seven named volumes",
      "first owner",
      "no Stripe variables are required in compose.yaml",
      "Docker Compose expresses that complete topology",
      "docker compose logs --tail=200 api",
      "docker compose down --volumes deletes",
    ]) expect(dockerDirect, contract).toContain(contract);

    const overview = JSON.stringify(pageByPath.get("/self-hosting"));
    expect(overview).toContain("single-host profile for loopback-only evaluation");
    expect(overview).toContain("external profile");
    expect(overview).not.toContain("single-host installation with operator-managed");
  });

  it("ships a Docker Compose example that Docker can parse", () => {
    const docker = pageByPath.get("/install/docker");
    const samples = docker?.sections.flatMap((section) => [section.code, ...(section.codes ?? [])]).filter((sample) => sample !== undefined) ?? [];
    const compose = samples.find((sample) => sample.label === "compose.yaml")?.code;
    const environment = samples.find((sample) => sample.label === ".env")?.code;
    expect(compose).toContain("name: ffdb");
    expect(environment).toContain("FFDB_BOOTSTRAP_TOKEN=");

    const directory = mkdtempSync(join(tmpdir(), "ffdb-docs-compose-"));
    try {
      writeFileSync(join(directory, "compose.yaml"), compose ?? "");
      writeFileSync(join(directory, ".env"), environment ?? "");
      execFileSync("docker", ["compose", "--env-file", ".env", "-f", "compose.yaml", "config", "--quiet"], {
        cwd: directory,
        stdio: "pipe",
      });
    } finally {
      rmSync(directory, { force: true, recursive: true });
    }
  });

  it("distinguishes the installed compiled gateway from its private Axum service", () => {
    const quickstart = JSON.stringify(pageByPath.get("/quickstart"));
    const docker = JSON.stringify(pageByPath.get("/install/docker"));
    const systemd = JSON.stringify(pageByPath.get("/install/systemd"));
    const observability = JSON.stringify(pageByPath.get("/observability"));
    const httpApi = JSON.stringify(pageByPath.get("/reference/http-api"));

    for (const contract of [
      "compiled nginx gateway",
      "not a Vite development server",
      "private Compose network",
      "does not publish Axum port 8080 to the host",
    ]) expect(quickstart, contract).toContain(contract);

    expect(docker).toContain("compiled nginx serving static production assets");
    expect(docker).toContain("Axum's internal port 8080, not Vite");
    expect(systemd).toContain("Axum at 127.0.0.1:8080");
    expect(systemd).toContain("There is no Caddy-to-nginx hop");
    expect(observability).toContain("never publish Axum directly on host port 8080");
    expect(observability).not.toContain("source-development Compose stack");
    expect(httpApi).toContain("http://127.0.0.1:5173/openapi.json");
    expect(httpApi).not.toContain("source-development Compose stack");

    const gatewayDockerfile = readFileSync(new URL("../../../infra/docker/Dockerfile.portal", import.meta.url), "utf8");
    const runtimeStage = gatewayDockerfile.slice(gatewayDockerfile.lastIndexOf("FROM nginx:"));
    const gatewayConfig = readFileSync(new URL("../../../infra/docker/portal-nginx.conf.template", import.meta.url), "utf8");
    const contributorCompose = readFileSync(new URL("../../../compose.yaml", import.meta.url), "utf8");
    const releaseCompose = readFileSync(new URL("../../../infra/release/compose.yaml", import.meta.url), "utf8");
    const singleHostCompose = readFileSync(new URL("../../../infra/release/compose.single-host.yaml", import.meta.url), "utf8");

    expect(runtimeStage).toContain('CMD ["nginx", "-g", "daemon off;"]');
    expect(runtimeStage).not.toContain("vite");
    expect(gatewayConfig).toContain("root /usr/share/nginx/html");
    expect(gatewayConfig).toContain("proxy_pass http://api:8080");
    expect(contributorCompose).toContain('"127.0.0.1:5173:8080"');
    expect(contributorCompose).toContain('"127.0.0.1:8080:8080"');
    for (const packagedCompose of [releaseCompose, singleHostCompose]) {
      expect(packagedCompose).toContain('"127.0.0.1:${FFDB_GATEWAY_PORT:-5173}:8080"');
      expect(packagedCompose).not.toContain('"127.0.0.1:8080:8080"');
    }
  });

  it("documents current-origin local verification independently of public DNS", () => {
    const configuration = JSON.stringify(pageByPath.get("/configuration"));
    for (const contract of [
      "current browser origin",
      "VITE_FFDB_API_URL",
      "http://127.0.0.1:5173/app/",
      "ffdb.forever-frameworks.com",
      "pre-deployment 403",
      "does not depend",
    ]) expect(configuration, contract).toContain(contract);

    const portalConfiguration = readFileSync(new URL("../../../apps/portal/src/config.ts", import.meta.url), "utf8");
    const explicitOverride = portalConfiguration.indexOf("environment.VITE_FFDB_API_URL");
    const savedInstance = portalConfiguration.indexOf("ffdb.portal.active-instance-url");
    const currentOrigin = portalConfiguration.indexOf("globalThis.location?.origin");
    expect(explicitOverride).toBeGreaterThanOrEqual(0);
    expect(savedInstance).toBeGreaterThan(explicitOverride);
    expect(currentOrigin).toBeGreaterThan(savedInstance);
  });

  it("keeps the organization metrics ledger durable in every packaged topology", () => {
    const composeFiles = [
      "../../../compose.yaml",
      "../../../compose.production.yaml",
      "../../../infra/release/compose.yaml",
      "../../../infra/release/compose.single-host.yaml",
    ].map((path) => readFileSync(new URL(path, import.meta.url), "utf8"));
    for (const compose of composeFiles) {
      expect(compose).toContain("FFDB_METRICS_ROOT: /var/lib/ffdb/metrics");
      expect(compose).toContain("metrics-data:/var/lib/ffdb/metrics");
      expect(compose).toContain("metrics-data:");
    }

    const runtimeDockerfile = readFileSync(new URL("../../../infra/docker/Dockerfile.rust", import.meta.url), "utf8");
    const systemdEnvironment = readFileSync(new URL("../../../infra/systemd/ffdb.env.example", import.meta.url), "utf8");
    const systemdService = readFileSync(new URL("../../../infra/systemd/ffdb-api.service", import.meta.url), "utf8");
    const systemdTmpfiles = readFileSync(new URL("../../../infra/systemd/ffdb.tmpfiles.conf", import.meta.url), "utf8");
    expect(runtimeDockerfile).toContain("FFDB_METRICS_ROOT=/var/lib/ffdb/metrics");
    expect(systemdEnvironment).toContain("FFDB_METRICS_ROOT=/var/lib/ffdb/metrics");
    expect(systemdService).toContain("ReadWritePaths=/var/lib/ffdb/projects /var/lib/ffdb/backups /var/lib/ffdb/metrics");
    expect(systemdTmpfiles).toContain("d /var/lib/ffdb/metrics 0700 ffdb ffdb -");

    const backupGuide = JSON.stringify(pageByPath.get("/backups"));
    expect(backupGuide).toContain("coordinated recovery point with PostgreSQL billing state");
    expect(backupGuide).toContain("A project restore must not roll the metrics ledger backward");
    expect(backupGuide).toContain("sudo ffdb-host backup create /secure/ffdb-host-2026-08-03.tar.gz");
    expect(backupGuide).toContain("sudo ffdb-host backup restore /secure/ffdb-host-2026-08-03.tar.gz --yes");
    expect(backupGuide).toContain("sudo ffdb-backup create /secure/ffdb-native-2026-08-03.tar.gz");
    expect(backupGuide).toContain("validates the complete archive before replacing anything");
    expect(JSON.stringify(pages)).toContain("latest/download/install.sh");
    expect(JSON.stringify(pages)).toContain("Choose an announced tag");
  });

  it("documents released platform billing separately from full project commerce", () => {
    const billing = navigation.find((group) => group.title === "Billing and payments");
    expect(billing?.links).toEqual([
      { title: "FFDB platform billing", href: "/billing/platform" },
      { title: "Project commerce", href: "/billing/project-payments" },
    ]);

    const platform = JSON.stringify(pageByPath.get("/billing/platform"));
    for (const contract of [
      "Status: implemented for self-hosted configuration",
      "GET /v1/organizations/:organization_id/billing",
      "POST /v1/organizations/:organization_id/billing/checkout",
      "POST /v1/organizations/:organization_id/billing/portal",
      "POST /v1/billing/webhooks/stripe",
      "organizationBilling",
      "createBillingCheckout",
      "createBillingPortal",
      "pay_as_you_go",
      "Idempotency-Key",
      "billing.provider_unavailable",
      "provider_configured",
      "project_limit",
      "free_project_limit: 2",
      "raw Stripe event payload",
      "platform_byo: operator_owned_billing",
      "platform_connect: connected_operator_billing",
      "$0.20 per GB-month",
      "$7 per month",
      "ffdb billing status",
      "ffdb billing checkout",
      "ffdb billing portal",
    ]) expect(platform, contract).toContain(contract);

    const project = JSON.stringify(pageByPath.get("/billing/project-payments"));
    for (const contract of [
      "Status: complete project commerce API",
      "/v1/projects/:project_id/commerce/account",
      "/v1/projects/:project_id/commerce/account/byo",
      "/v1/projects/:project_id/commerce/account/connect/onboarding",
      "/v1/projects/:project_id/commerce/products",
      "/v1/projects/:project_id/commerce/prices",
      "/v1/projects/:project_id/commerce/checkouts/one-time",
      "/v1/projects/:project_id/commerce/checkouts/recurring",
      "/v1/projects/:project_id/commerce/customer-portal",
      "/v1/projects/:project_id/commerce/orders",
      "/v1/projects/:project_id/commerce/payments",
      "/v1/projects/:project_id/commerce/refunds",
      "/v1/projects/:project_id/commerce/subscriptions",
      "/v1/projects/:project_id/commerce/entitlements",
      "/v1/projects/:project_id/commerce/webhooks/stripe",
      "ffdb.commerce.configureByo",
      "ffdb.commerce.connectOnboarding",
      "ffdb.commerce.createProduct",
      "ffdb.commerce.createPrice",
      "ffdb.commerce.recurringCheckout",
      "ffdb.commerce.customerPortal",
      "ffdb.commerce.refund",
      "ffdb.commerce.cancelSubscription",
      "ffdb.commerce.updateFulfillment",
      "encrypted BYO Stripe credentials",
      "Accounts v2 Connect onboarding",
      "direct charges",
      "Idempotency-Key",
      "restricted",
    ]) expect(project, contract).toContain(contract);
    expect(project).not.toContain("not_released");
    expect(project).not.toContain("read-only capability");

    const cli = JSON.stringify(pageByPath.get("/cli"));
    for (const command of ["ffdb billing status", "ffdb billing checkout", "ffdb billing portal"]) {
      expect(cli, command).toContain(command);
    }
  });

  it("enforces task-oriented depth on every route", () => {
    expect(pages).toHaveLength(32);
    for (const page of pages) {
      const sections = new Map(page.sections.map((section) => [section.heading, section]));
      expect(sections.has("What, why, and when"), page.path).toBe(false);
      expect(sections.has("Prerequisites and required values"), page.path).toBe(false);
      if (page.path === "/quickstart") {
        expect(page.sections).toHaveLength(4);
      } else {
        expect(page.sections[0]?.paragraphs?.length, page.path).toBeGreaterThanOrEqual(3);
        expect(sections.get(`Requirements for ${page.title}`)?.bullets?.length, page.path).toBeGreaterThanOrEqual(4);
        expect(sections.get(`${page.title} workflow`)?.bullets?.length, page.path).toBeGreaterThanOrEqual(4);
        expect(sections.get(`Troubleshoot ${page.title.toLocaleLowerCase()}`)?.bullets?.length, page.path).toBeGreaterThanOrEqual(2);
        expect(sections.get(`Continue from ${page.title}`)?.bullets?.length, page.path).toBeGreaterThanOrEqual(2);
      }

      const visibleWords = page.sections.flatMap((section) => [
        section.heading,
        ...(section.paragraphs ?? []),
        ...(section.bullets ?? []),
        section.callout?.title ?? "",
        section.callout?.body ?? "",
      ]).join(" ").split(/\s+/u).filter(Boolean);
      expect(visibleWords.length, `${page.path}: visible words`).toBeGreaterThanOrEqual(page.path === "/quickstart" ? 120 : 160);
    }
  });

  it("gives every task page a runnable or inspectable example", () => {
    for (const page of pages.filter((candidate) => candidate.path !== "/")) {
      const samples = page.sections.flatMap((section) => [section.code, ...(section.codes ?? [])]).filter(Boolean);
      expect(samples.length, `${page.path}: code examples`).toBeGreaterThanOrEqual(1);
    }
  });

  it("indexes installation copy and code while keeping every language recognized", () => {
    expect(searchPages("copyable Docker Compose")[0]?.path).toBe("/install/docker");
    expect(searchPages("systemctl enable")[0]?.path).toBe("/install/systemd");
    for (const page of pages) {
      for (const section of page.sections) {
        for (const sample of [section.code, ...(section.codes ?? [])]) {
          if (sample === undefined) continue;
          expect(supportedLanguages.has(sample.language), `${page.path}: ${sample.language}`).toBe(true);
          expect(highlightCode(sample.code, sample.language).map((token) => token.value).join("")).toBe(sample.code);
        }
      }
    }
  });

  it("normalizes docs mounts and trailing slashes", () => {
    expect(normalizePath("/docs")).toBe("/");
    expect(normalizePath("/docs/quickstart/")).toBe("/quickstart");
  });
});

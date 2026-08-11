import {
  clientClassSections,
  clientFunctionSignatures,
  clientTypeSections,
  cliCommandSections,
  cliEnvironment,
  cliModuleSections,
  httpOperationSections,
} from "./generated-reference";

export interface CodeSample {
  readonly label: string;
  readonly language: string;
  readonly code: string;
}

export interface Callout {
  readonly kind: "note" | "warning";
  readonly title: string;
  readonly body: string;
}

export interface DocSection {
  readonly heading: string;
  readonly paragraphs?: readonly string[];
  readonly bullets?: readonly string[];
  readonly code?: CodeSample;
  readonly codes?: readonly CodeSample[];
  readonly callout?: Callout;
}

export interface DocPage {
  readonly path: string;
  readonly title: string;
  readonly description: string;
  readonly group: string;
  readonly sections: readonly DocSection[];
}

export interface NavigationGroup {
  readonly title: string;
  readonly links: readonly { readonly title: string; readonly href: string }[];
}

interface PageGuide {
  readonly what: string;
  readonly why: string;
  readonly when: string;
  readonly prerequisites: readonly string[];
  readonly requiredValues: readonly string[];
  readonly steps: readonly string[];
  readonly result: string;
  readonly failures: readonly string[];
  readonly nextSteps: readonly string[];
}

const clientSetup = `import { BrowserSessionStore, FFDBClient } from "@ffdb/client";

export const ffdb = new FFDBClient({
  baseUrl: "https://ffdb.example.com",
  projectId: "your-project-id",
  sessionStore: new BrowserSessionStore(
    window.sessionStorage,
    "my-app.ffdb-session",
  ),
});`;

const queryExample = `const result = await ffdb.query({
  sql: "select id, title from documents where title like ?1 order by title",
  parameters: [{ type: "text", value: search + "%" }],
  options: { max_rows: 100 },
});

for (const row of result.rows) {
  console.log(row[0], row[1]);
}`;

const singleHostDockerCompose = `name: ffdb

services:
  postgres:
    image: postgres:17.5-alpine
    environment:
      - POSTGRES_DB
      - POSTGRES_USER
      - POSTGRES_PASSWORD
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U $$POSTGRES_USER -d $$POSTGRES_DB"]
      interval: 3s
      timeout: 3s
      retries: 30
    volumes:
      - postgres-data:/var/lib/postgresql/data
    restart: unless-stopped

  minio:
    image: minio/minio:RELEASE.2025-04-22T22-12-26Z
    command: server /data --console-address ":9001"
    environment:
      - MINIO_ROOT_USER
      - MINIO_ROOT_PASSWORD
      - MINIO_API_CORS_ALLOW_ORIGIN
    healthcheck:
      test: ["CMD", "mc", "ready", "local"]
      interval: 3s
      timeout: 3s
      retries: 30
    ports:
      - "127.0.0.1:9000:9000"
      - "127.0.0.1:9001:9001"
    volumes:
      - minio-data:/data
    restart: unless-stopped

  minio-bootstrap:
    image: minio/mc:RELEASE.2025-04-16T18-13-26Z
    depends_on:
      minio:
        condition: service_healthy
    environment:
      - MINIO_ROOT_USER
      - MINIO_ROOT_PASSWORD
    entrypoint: ["/bin/sh", "-ec"]
    command:
      - >-
        mc alias set local http://minio:9000 "$$MINIO_ROOT_USER" "$$MINIO_ROOT_PASSWORD" &&
        mc mb --ignore-existing local/ffdb &&
        mc anonymous set none local/ffdb
    restart: "no"

  mailpit:
    image: axllent/mailpit:v1.27.8
    environment:
      MP_DATABASE: /data/mailpit.db
    healthcheck:
      test: ["CMD", "/mailpit", "readyz"]
      interval: 3s
      timeout: 3s
      retries: 30
    ports:
      - "127.0.0.1:1025:1025"
      - "127.0.0.1:8025:8025"
    volumes:
      - mailpit-data:/data
    restart: unless-stopped

  volume-init:
    image: alpine:3.22.1
    user: "0:0"
    command:
      - /bin/sh
      - -ec
      - chown -R 10001:10001 /var/lib/ffdb/projects /var/lib/ffdb/backups /var/lib/ffdb/metrics /var/lib/ffdb/sync
    read_only: true
    volumes:
      - project-data:/var/lib/ffdb/projects
      - backup-data:/var/lib/ffdb/backups
      - metrics-data:/var/lib/ffdb/metrics
      - sync-data:/var/lib/ffdb/sync
    restart: "no"

  api:
    image: ghcr.io/forever-frameworks-llc/ffdb-runtime:0.3.13
    environment:
      - FFDB_ENVIRONMENT
      - FFDB_HTTP_BIND
      - FFDB_PUBLIC_BASE_URL
      - FFDB_CORS_ALLOWED_ORIGINS
      - FFDB_DATABASE_URL
      - FFDB_NODE_ID
      - FFDB_NODE_NAME
      - FFDB_DATABASE_ROOT
      - FFDB_BACKUP_ROOT
      - FFDB_METRICS_ROOT
      - FFDB_DATABASE_WORKER
      - FFDB_MASTER_KEY
      - FFDB_BACKUP_MASTER_KEY
      - FFDB_CURSOR_HMAC_KEY
      - FFDB_BOOTSTRAP_TOKEN
      - FFDB_S3_ENDPOINT
      - FFDB_S3_PUBLIC_ENDPOINT
      - FFDB_S3_REGION
      - FFDB_S3_BUCKET
      - FFDB_S3_ACCESS_KEY_ID
      - FFDB_S3_SECRET_ACCESS_KEY
      - FFDB_EMAIL_TRANSPORT
      - FFDB_SMTP_HOST
      - FFDB_SMTP_PORT
      - FFDB_EMAIL_FROM
      - RUST_LOG
    depends_on:
      postgres:
        condition: service_healthy
      minio-bootstrap:
        condition: service_completed_successfully
      mailpit:
        condition: service_healthy
      volume-init:
        condition: service_completed_successfully
    healthcheck:
      test: ["CMD", "curl", "--fail", "--silent", "http://127.0.0.1:8080/readyz"]
      interval: 5s
      timeout: 3s
      retries: 30
      start_period: 5s
    expose: ["8080"]
    read_only: true
    tmpfs:
      - /tmp:rw,noexec,nosuid,nodev,size=64m
    volumes:
      - project-data:/var/lib/ffdb/projects
      - backup-data:/var/lib/ffdb/backups
      - metrics-data:/var/lib/ffdb/metrics
    cap_drop: ["ALL"]
    security_opt: ["no-new-privileges:true"]
    restart: unless-stopped

  sync-worker:
    image: ghcr.io/forever-frameworks-llc/ffdb-runtime:0.3.13
    command: ["/usr/local/bin/ffdb-sync-worker"]
    environment:
      - FFDB_SYNC_STATE_DIR
      - FFDB_SYNC_MAINTENANCE_INTERVAL_SECONDS
      - FFDB_SYNC_STALE_TEMPORARY_SECONDS
      - RUST_LOG
    depends_on:
      api:
        condition: service_healthy
      volume-init:
        condition: service_completed_successfully
    read_only: true
    tmpfs:
      - /tmp:rw,noexec,nosuid,nodev,size=16m
    volumes:
      - sync-data:/var/lib/ffdb/sync
    cap_drop: ["ALL"]
    security_opt: ["no-new-privileges:true"]
    restart: unless-stopped

  gateway:
    image: ghcr.io/forever-frameworks-llc/ffdb-gateway:0.3.13
    environment:
      - FFDB_S3_PUBLIC_ORIGIN
    depends_on:
      api:
        condition: service_healthy
    ports:
      - "127.0.0.1:5173:8080"
    read_only: true
    tmpfs:
      - /tmp:rw,noexec,nosuid,nodev,size=16m,mode=1777
    cap_drop: ["ALL"]
    security_opt: ["no-new-privileges:true"]
    restart: unless-stopped

volumes:
  postgres-data:
  minio-data:
  mailpit-data:
  project-data:
  backup-data:
  metrics-data:
  sync-data:`;

const singleHostDockerEnvironment = `POSTGRES_DB=ffdb
POSTGRES_USER=ffdb
POSTGRES_PASSWORD=replace-with-openssl-rand-hex-24

MINIO_ROOT_USER=ffdb-local
MINIO_ROOT_PASSWORD=replace-with-another-openssl-rand-hex-24
MINIO_API_CORS_ALLOW_ORIGIN=http://127.0.0.1:5173,http://localhost:5173

FFDB_ENVIRONMENT=development
FFDB_HTTP_BIND=0.0.0.0:8080
FFDB_PUBLIC_BASE_URL=http://127.0.0.1:5173
FFDB_CORS_ALLOWED_ORIGINS=http://127.0.0.1:5173,http://localhost:5173
FFDB_DATABASE_URL=postgres://ffdb:replace-with-the-postgres-password@postgres:5432/ffdb
FFDB_NODE_ID=019fc39c-ddbd-7d12-9849-e4ee35310132
FFDB_NODE_NAME=ffdb-single-host-01
FFDB_DATABASE_ROOT=/var/lib/ffdb/projects
FFDB_BACKUP_ROOT=/var/lib/ffdb/backups
FFDB_METRICS_ROOT=/var/lib/ffdb/metrics
FFDB_DATABASE_WORKER=/opt/ffdb/current/bin/ffdb-database-worker
FFDB_MASTER_KEY=replace-with-openssl-rand-base64-32
FFDB_BACKUP_MASTER_KEY=replace-with-an-independent-base64-key
FFDB_CURSOR_HMAC_KEY=replace-with-openssl-rand-hex-32
FFDB_BOOTSTRAP_TOKEN=replace-with-an-independent-openssl-rand-hex-32

FFDB_S3_ENDPOINT=http://minio:9000
FFDB_S3_PUBLIC_ENDPOINT=http://127.0.0.1:9000
FFDB_S3_PUBLIC_ORIGIN=http://127.0.0.1:9000
FFDB_S3_REGION=us-east-1
FFDB_S3_BUCKET=ffdb
FFDB_S3_ACCESS_KEY_ID=ffdb-local
FFDB_S3_SECRET_ACCESS_KEY=replace-with-the-minio-password

FFDB_EMAIL_TRANSPORT=smtp
FFDB_SMTP_HOST=mailpit
FFDB_SMTP_PORT=1025
FFDB_EMAIL_FROM=FFDB <noreply@localhost.test>
FFDB_SYNC_STATE_DIR=/var/lib/ffdb/sync
FFDB_SYNC_MAINTENANCE_INTERVAL_SECONDS=60
FFDB_SYNC_STALE_TEMPORARY_SECONDS=3600
RUST_LOG=ffdb=info,tower_http=info`;

const singleHostSecretCommands = `umask 077
openssl rand -hex 24       # PostgreSQL password
openssl rand -hex 24       # MinIO password
openssl rand -base64 32    # FFDB_MASTER_KEY
openssl rand -base64 32    # FFDB_BACKUP_MASTER_KEY
openssl rand -hex 32       # FFDB_CURSOR_HMAC_KEY
openssl rand -hex 32       # FFDB_BOOTSTRAP_TOKEN
uuidgen | tr '[:upper:]' '[:lower:]'  # FFDB_NODE_ID
chmod 600 .env`;

const singleHostStartCommands = `docker compose config --quiet
docker compose pull
docker compose up --detach --wait

docker compose ps
curl --fail http://127.0.0.1:5173/healthz
curl --fail http://127.0.0.1:5173/readyz
curl --fail http://127.0.0.1:5173/openapi.json >/dev/null`;

const singleHostLifecycleCommands = `docker compose ps
docker compose logs --tail=200 api
docker compose logs --tail=200 sync-worker
docker compose logs --tail=200 gateway

docker compose stop
docker compose start

# Update only after reading release notes and taking a backup.
docker compose pull
docker compose up --detach --wait`;

const routePages = [
  {
    path: "/",
    title: "FFDB documentation",
    description: "Build against one hardened SQLite database per project, with a self-hosted PostgreSQL control plane and isolated Rust workers.",
    group: "Start here",
    sections: [
      {
        heading: "What FFDB is",
        paragraphs: [
          "FFDB is a self-hostable data platform. PostgreSQL stores organizations, projects, credentials, jobs, and other control-plane state. Application rows live in a dedicated SQLite database for each project and are only reached through constrained workers.",
          "The public surface includes authentication, parameterized SQL, PostgreSQL-style row-level policies, logical offline sync, RLS-protected S3-compatible storage, a TypeScript SDK, a CLI, and a management portal.",
        ],
      },
      {
        heading: "Choose a path",
        bullets: [
          "Install a verified release bundle, then use the portal and packaged CLI to create a project.",
          "Install the version-matched @ffdb/client SDK package in an existing browser or Node application.",
          "Add the @ffdb/react, @ffdb/react-native, or @ffdb/sync-client SDK package when the runtime needs that integration.",
          "Read the production security and backup guides before exposing a deployment to untrusted traffic.",
        ],
        callout: { kind: "note", title: "Use matched releases", body: "Applications never receive a raw SQLite connection or file. Pin every @ffdb package to the signed server release version so the API and package contracts stay aligned." },
      },
    ],
  },
  {
    path: "/quickstart",
    title: "Quickstart",
    description: "Install a verified packaged release, bootstrap a project, and make one authenticated parameterized query.",
    group: "Start here",
    sections: [
      {
        heading: "Install a zero-source evaluation host",
        paragraphs: ["The packaged single-host profile evaluates the complete product from one signed GitHub Release. After signature verification, it starts digest-pinned PostgreSQL, MinIO, persistent Mailpit capture, the FFDB API and workers, and the unified gateway."],
        codes: [
          { label: "Latest stable GitHub Release", language: "sh", code: `curl -fsSLo ffdb-install.sh \\
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh
less ffdb-install.sh
sudo sh ffdb-install.sh --profile single-host --start --require-signature` },
          { label: "Reproducible exact tag", language: "sh", code: `VERSION=0.3.13
RELEASE_BASE="https://github.com/Forever-Frameworks-LLC/ffdb/releases/download/v$VERSION"
curl -fsSLo ffdb-install.sh "$RELEASE_BASE/install.sh"
less ffdb-install.sh
sudo sh ffdb-install.sh --profile single-host --start --require-signature \\
  --version "$VERSION" --release-base "$RELEASE_BASE"` },
          { label: "Verified mirror or air-gapped bundle", language: "sh", code: `sudo env \\
  FFDB_VERSION=0.3.13 \\
  FFDB_RELEASE_BASE_URL=file:///srv/ffdb/releases/v0.3.13 \\
  sh ./install.sh --profile single-host --start --require-signature` },
        ],
        callout: { kind: "warning", title: "Pin production installs", body: "The latest URL follows the stable GitHub Release. The exact-tag example pins the supported 0.3.13 release for reproducible production automation; read that release's notes before installation or upgrade." },
      },
      {
        heading: "Verify the single-host services",
        paragraphs: ["The installer generates independent strong credentials with OpenSSL, writes them root-only to /etc/ffdb/single-host.env at mode 0600, and never prints them. Every exposed host port is loopback-only, PostgreSQL has no host port, and seven named volumes preserve database, object, captured-mail, project, backup, organization-metrics, and sync state across normal stop, upgrade, rollback, and uninstall operations. The address on port 5173 is the compiled nginx gateway, not a Vite development server: it serves immutable production web files and proxies API routes to Axum at api:8080 on the private Compose network. The packaged Docker profile does not publish Axum port 8080 to the host."],
        code: { label: "Compiled nginx gateway readiness (not Vite)", language: "sh", code: `sudo ffdb-host status
# Port 5173 is the packaged nginx gateway, not a Vite server.
curl --fail http://127.0.0.1:5173/readyz` },
        callout: { kind: "warning", title: "Evaluation and local use only", body: "single-host sets FFDB_ENVIRONMENT=development, uses local HTTP MinIO and Mailpit mail capture, and concentrates every durable dependency on one host. Do not expose it to the internet. The external-provider release profile with independently backed-up PostgreSQL, HTTPS object storage, real email delivery, and TLS remains the recommended internet-production topology." },
      },
      {
        heading: "Verify locally before public DNS cutover",
        paragraphs: [
          "The packaged portal uses its current browser origin for API requests when VITE_FFDB_API_URL is absent, so http://127.0.0.1:5173/app/ talks to the same installed gateway that served it. For a separately built application, VITE_FFDB_API_URL is the explicit API-public-origin override; point it at the loopback gateway during local acceptance and replace it with the final HTTPS origin for the production build.",
          "Local acceptance does not depend on ffdb.forever-frameworks.com. A pre-deployment 403 or an unrelated response from that public hostname describes the remote host only; verify the installed release through its current loopback origin until DNS and the production gateway are deployed.",
        ],
        codes: [
          { label: "Installed current-origin checks", language: "sh", code: `curl --fail http://127.0.0.1:5173/readyz
curl --fail http://127.0.0.1:5173/openapi.json >/dev/null
# Open the same-origin portal served by the installed gateway:
# http://127.0.0.1:5173/app/` },
          { label: "Explicit application API origin", language: "env", code: `# Local acceptance build
VITE_FFDB_API_URL=http://127.0.0.1:5173

# Production build after TLS/DNS cutover
VITE_FFDB_API_URL=https://data.example.com` },
        ],
        callout: { kind: "note", title: "One origin at a time", body: "Use the current origin for the packaged portal. For a separately served application, save its browser origin under the selected project's Auth → Policy → Application URLs. The project policy takes effect immediately." },
      },
      {
        heading: "Retrieve the bootstrap token without printing it",
        paragraphs: ["The generated one-time token remains in the root-readable single-host environment file. Copy only that value into a root-owned file for ingestion by a secret manager; ffdb-host status and logs never reveal it."],
        code: { label: "Terminal", language: "sh", code: `sudo sh -c 'umask 077; sed -n "s/^FFDB_BOOTSTRAP_TOKEN=//p" \\
  /etc/ffdb/single-host.env > /root/ffdb-bootstrap-token'` },
      },
      {
        heading: "Operate the packaged lifecycle",
        paragraphs: ["ffdb-host selects the installed signed release and keeps configuration and durable volumes outside its immutable release directory."],
        code: { label: "Terminal", language: "sh", code: `sudo ffdb-host verify
sudo ffdb-host status
sudo ffdb-host logs api
sudo ffdb-host stop
sudo ffdb-host start` },
      },
      {
        heading: "Finish first-run setup in the portal",
        paragraphs: ["Open http://127.0.0.1:5173/app/ in a trusted local browser. The first-run wizard accepts the generated bootstrap token once, creates the first owner, then asks how this instance should operate: private, team, operator-owned Stripe credentials, or Stripe Connect. Private and team modes keep usage analytics without tenant charges. The BYO and Connect modes enable the deployment-owned Free, pay-as-you-go, and Pro catalog. Connect onboarding returns to the wizard; refresh completes the provider check and automatically provisions the versioned Stripe Product, Prices, and Billing Meters before the portal enters global administration."],
        code: { label: "Trusted browser", language: "yaml", code: `origin: http://127.0.0.1:5173/app/
steps:
  - Paste the one-time token from /root/ffdb-bootstrap-token
  - Create the first owner
  - Choose private, team, Stripe BYO, or Stripe Connect
  - Complete provider setup when billing is enabled
  - Continue to Global admin to create organizations and projects` },
        callout: { kind: "warning", title: "One-time credential", body: "Paste the bootstrap token only into the first-run wizard served by the trusted FFDB origin. Never embed it in application code, store it in browser persistence, or reuse it after the first owner exists." },
      },
      {
        heading: "Auditable terminal bootstrap alternative",
        paragraphs: ["Automation and headless installations may call the same one-time bootstrap API directly. This is an alternative to the portal wizard, not a prerequisite for using it. After owner creation, complete setup with POST /v1/instance or sign in to /app/ and continue the wizard."],
        code: { label: "Terminal", language: "sh", code: `export FFDB_OWNER_EMAIL=admin@example.test
read -r -s -p "Owner password: " FFDB_OWNER_PASSWORD
export FFDB_OWNER_PASSWORD
read -r -s -p "Bootstrap token: " FFDB_BOOTSTRAP_TOKEN
export FFDB_BOOTSTRAP_TOKEN

node -e '
const response = await fetch("http://127.0.0.1:5173/v1/developer/bootstrap", {
  method: "POST",
  headers: {
    "content-type": "application/json",
    "x-ffdb-bootstrap-token": process.env.FFDB_BOOTSTRAP_TOKEN,
  },
  body: JSON.stringify({
    email: process.env.FFDB_OWNER_EMAIL,
    password: process.env.FFDB_OWNER_PASSWORD,
  }),
});
if (!response.ok) throw new Error("bootstrap failed: " + response.status);
console.log("owner created");
' 
unset FFDB_BOOTSTRAP_TOKEN` },
        callout: { kind: "warning", title: "Do not log the token", body: "Production requires a unique token of at least 32 characters. Pass it from a protected secret source, remove it from the automation environment immediately, and do not place it on a command line." },
      },
      {
        heading: "Install the packaged CLI",
        paragraphs: ["The public @ffdb/cli package is separate from the server bundle. Check the registry version, review the matching release notes, and pin that version in trusted operator environments."],
        code: { label: "Terminal", language: "sh", code: `npm view @ffdb/cli version
npm install --global @ffdb/cli@0.3.13
ffdb --help` },
      },
      {
        heading: "Create and link a project",
        paragraphs: ["Log in with the CLI, create an organization and project, then link that project in the local credential file."],
        code: { label: "Terminal", language: "sh", code: `FFDB_PASSWORD="$FFDB_OWNER_PASSWORD" ffdb --url http://127.0.0.1:5173 login "$FFDB_OWNER_EMAIL"

ffdb org create "Example" example
export FFDB_ORG_ID="copy-the-returned-organization-id"

ffdb project create "$FFDB_ORG_ID" "Notes" notes local
export FFDB_PROJECT_ID="copy-the-returned-project-id"

ffdb project link "$FFDB_PROJECT_ID"
unset FFDB_OWNER_PASSWORD` },
        callout: { kind: "warning", title: "Credential separation", body: "Platform login manages organizations and projects. Database administration uses a separate scoped project developer key. Do not ship either credential in a browser bundle." },
      },
      {
        heading: "Connect the client",
        code: { label: "src/ffdb.ts", language: "ts", code: clientSetup },
        paragraphs: ["Sign in an end user before issuing RLS-scoped application queries. Use a developer key only in trusted tooling and server environments."],
      },
    ],
  },
  {
    path: "/install/docker",
    title: "Install with Docker Compose",
    description: "Copy a complete Docker Compose stack, start FFDB, and finish first-run setup in the portal.",
    group: "Install",
    sections: [
      {
        heading: "Before you start",
        paragraphs: [
          "This page is the direct Docker path. Copy compose.yaml and .env into an empty directory, generate the required infrastructure secrets, and run docker compose up. You do not clone the FFDB repository, install Rust or Node.js, or execute a source build.",
          "The example is a complete single-host installation for local evaluation and private networks. It includes PostgreSQL, MinIO, Mailpit, the FFDB API and workers, and the compiled web gateway. The first browser user still completes owner and instance onboarding; no Stripe account or billing mode is selected in advance.",
        ],
        bullets: [
          "Docker Engine 27 or newer with Docker Compose v2.",
          "At least 4 CPU cores, 8 GB of memory, and 20 GB of free disk for a useful evaluation.",
          "OpenSSL for generating independent secrets and a browser on the same trusted machine.",
          "Loopback ports 5173, 9000, 9001, 8025, and 1025 available.",
        ],
        callout: { kind: "note", title: "Why Compose instead of one docker run command", body: "FFDB has independent database, object-storage, mail, API, maintenance-worker, and gateway processes with ordered health checks and seven durable volumes. Docker Compose expresses that complete topology. A single docker run command would omit required services or hide their lifecycle inside one unsafe container." },
      },
      {
        heading: "1. Copy compose.yaml",
        paragraphs: ["Create an empty directory named ffdb, save this complete file as compose.yaml, and keep its seven named volumes. Both FFDB images are pinned to the server release version; use the same version for every FFDB image during an upgrade."],
        code: { label: "compose.yaml", language: "yaml", code: singleHostDockerCompose },
      },
      {
        heading: "2. Create .env and replace every placeholder",
        paragraphs: [
          "Save the template as .env beside compose.yaml, set its mode to 0600, and replace every replace-with value. PostgreSQL, MinIO, the database encryption key, backup key, cursor HMAC key, and bootstrap token must all be independent.",
          "Use openssl rand -hex 24 for the PostgreSQL and MinIO passwords, openssl rand -base64 32 for each encryption key, and openssl rand -hex 32 for the HMAC key and bootstrap token. Copy the PostgreSQL password into FFDB_DATABASE_URL and the MinIO password into FFDB_S3_SECRET_ACCESS_KEY. Generate a unique UUID for FFDB_NODE_ID.",
        ],
        codes: [
          { label: ".env", language: "env", code: singleHostDockerEnvironment },
          { label: "Generate each value independently", language: "sh", code: singleHostSecretCommands },
        ],
        callout: { kind: "warning", title: "Do not reuse or commit .env", body: "The bootstrap token creates the first owner; the master keys protect stored credentials and backups. Keep .env outside version control, restrict it to the host operator, and back up the encryption keys separately from the data volumes." },
      },
      {
        heading: "3. Start and verify the stack",
        paragraphs: ["Compose pulls the released images, starts dependencies in order, and waits for their health checks. The public entry point is the gateway on 127.0.0.1:5173. It serves compiled assets and proxies API traffic to private Axum port 8080; it is not a Vite development server."],
        code: { label: "Terminal", language: "sh", code: singleHostStartCommands },
        callout: { kind: "note", title: "What is listening", body: "Open http://127.0.0.1:5173/ for the product site, /docs/ for these docs, and /app/ for the portal. MinIO's local console is on 9001 and captured evaluation email is on 8025. PostgreSQL and Axum remain internal to the Compose network." },
      },
      {
        heading: "4. Create the first owner and choose the instance type",
        paragraphs: [
          "Open http://127.0.0.1:5173/app/. Paste FFDB_BOOTSTRAP_TOKEN from the protected .env file, create the first owner, and choose private, team, platform billing with your Stripe keys, or Stripe Connect. The wizard collects provider values when the selected mode needs them; no Stripe variables are required in compose.yaml.",
          "Onboarding must finish before the owner can create organizations or projects. After it completes, create the first organization and project directly in the portal and verify that the project appears in the project switcher.",
        ],
        callout: { kind: "warning", title: "Use the trusted local origin", body: "Paste the bootstrap token only into the portal served by this installation. Do not put it in application code, a browser bundle, screenshots, shell history, or support logs." },
      },
      {
        heading: "Optional signed lifecycle controller",
        paragraphs: ["The direct Compose path above is complete. Operators who also want signature verification, immutable digest resolution, installed release directories, backup/update/rollback commands, and root-owned generated configuration can use the release controller instead. It manages the same Docker topology; it is not required for the copyable Compose path."],
        codes: [
          { label: "Latest stable GitHub Release", language: "sh", code: `curl -fsSLo ffdb-install.sh \\
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh
less ffdb-install.sh
sudo sh ffdb-install.sh --profile single-host --start --require-signature
sudo ffdb-host status
# Port 5173 is the packaged nginx gateway, not a Vite server.
curl --fail http://127.0.0.1:5173/readyz` },
          { label: "Reproducible exact tag", language: "sh", code: `VERSION=0.3.13
RELEASE_BASE="https://github.com/Forever-Frameworks-LLC/ffdb/releases/download/v$VERSION"
curl -fsSLo ffdb-install.sh "$RELEASE_BASE/install.sh"
less ffdb-install.sh
sudo sh ffdb-install.sh --profile single-host --start --require-signature \\
  --version "$VERSION" --release-base "$RELEASE_BASE"
sudo ffdb-host status
# Port 5173 is the packaged nginx gateway, not a Vite server.
curl --fail http://127.0.0.1:5173/readyz` },
          { label: "Verified mirror or air-gapped bundle", language: "sh", code: `sudo env \\
  FFDB_VERSION=0.3.13 \\
  FFDB_RELEASE_BASE_URL=file:///srv/ffdb/releases/v0.3.13 \\
  sh ./install.sh --profile single-host --start --require-signature
sudo ffdb-host status` },
        ],
        callout: { kind: "warning", title: "Release discovery is separate from installation", body: "The examples pin the supported 0.3.13 release while the latest URL tracks the stable channel. The installer verifies the signed checksum list, bundle, controller, and pinned images. Mirrors must preserve the release filenames and Sigstore bundle unchanged." },
      },
      {
        heading: "Understand the single-host boundary",
        paragraphs: ["The selected release pins PostgreSQL, MinIO, Mailpit, the FFDB runtime, and the gateway by immutable image digest. The installer generates independent strong PostgreSQL, MinIO, encryption, HMAC, bootstrap, and node credentials, writes them to /etc/ffdb/single-host.env at mode 0600, preserves existing secrets on reinstall or upgrade, and never prints them."],
        bullets: ["The compiled nginx gateway listens at http://127.0.0.1:5173; this is not a Vite development server.", "The gateway serves the landing site at /, docs at /docs/, and the portal at /app/, then proxies /v1, /healthz, /readyz, and /openapi.json to the private Axum service at api:8080. Raw /metrics returns 404 at the gateway and remains private to Axum.", "The packaged Docker profiles expose Axum port 8080 only to the Compose network, never directly to the host.", "MinIO API and console listen at loopback ports 9000 and 9001.", "Mailpit SMTP and captured-mail UI listen at loopback ports 1025 and 8025.", "PostgreSQL has no host port; all exposed host ports are loopback-only.", "The seven named volumes preserve PostgreSQL, object data, captured mail, project SQLite files, encrypted backups, organization usage/billing ledgers, and sync state across normal stop, upgrade, rollback, and uninstall."],
      },
      {
        heading: "Retrieve the generated bootstrap token safely",
        paragraphs: ["The bootstrap token remains only in the root-readable single-host environment. Extract it to a root-owned file for secret-manager ingestion without writing the value to the terminal; ffdb-host status and logs do not reveal it."],
        code: { label: "Terminal", language: "sh", code: `sudo sh -c 'umask 077; sed -n "s/^FFDB_BOOTSTRAP_TOKEN=//p" \\
  /etc/ffdb/single-host.env > /root/ffdb-bootstrap-token'` },
      },
      {
        heading: "Operate an external-provider production release",
        paragraphs: ["For internet production, configure the external-provider profile with independently backed-up PostgreSQL, private HTTPS S3-compatible storage, real email delivery, and a TLS gateway. Signed release metadata pins the multi-architecture runtime and gateway images by immutable digest, and ffdb-host invokes the matching Compose model."],
        code: { label: "Terminal", language: "sh", code: `# For a directly downloaded and verified bundle:
sudo ffdb-host install \\
  --version 0.3.13 \\
  --bundle /srv/ffdb/releases/ffdb-compose-bundle-0.3.13.tar.gz

# The public/local installer already performs the install step:
sudo ffdb-host start
sudo ffdb-host status
sudo ffdb-host logs api` },
      },
      {
        heading: "Understand the service boundary",
        bullets: [
          "volume-init gives fixed UID/GID 10001 ownership of named project-data, backup-data, metrics-data, and sync-data volumes.",
          "api supervises bounded ffdb-database-worker children and writes only project, backup, and organization-metrics volumes.",
          "sync-worker maintains durable sync checkpoint artifacts and writes only sync-data.",
          "gateway is the only FFDB application ingress, bound to 127.0.0.1:${FFDB_GATEWAY_PORT:-5173}; it is compiled nginx serving static production assets and proxying API routes to Axum's internal port 8080, not Vite.",
          "All long-running containers are non-root, capability-free, resource-bounded, and use read-only root filesystems.",
        ],
        callout: { kind: "note", title: "Terminate TLS in front", body: "Forward the complete public origin to the loopback gateway, preserve Host and the client address, and set X-Forwarded-Proto: https. The integrated gateway serves /, /docs/, /app/, and the API routes." },
      },
      {
        heading: "Operate and troubleshoot",
        codes: [
          { label: "Direct Compose status, logs, and lifecycle", language: "sh", code: singleHostLifecycleCommands },
          { label: "Public TLS checks", language: "sh", code: `curl --fail --show-error https://ffdb.example.com/healthz
curl --fail --show-error https://ffdb.example.com/readyz
curl --fail --show-error \
  https://ffdb.example.com/openapi.json >/dev/null` },
          { label: "Service state and logs", language: "sh", code: `sudo ffdb-host status
sudo ffdb-host logs api
sudo ffdb-host logs sync-worker
sudo ffdb-host logs gateway` },
        ],
        paragraphs: ["For the direct path, start with docker compose ps and the bounded service logs above. A failed PostgreSQL login usually means POSTGRES_PASSWORD and the password inside FFDB_DATABASE_URL differ. A failed MinIO bootstrap usually means MINIO_ROOT_PASSWORD and FFDB_S3_SECRET_ACCESS_KEY differ. An instance.setup_required response means the owner must finish the portal wizard before creating organizations or projects. For production, also inspect response security headers, directly reload a nested /docs/ route, and verify an authorized browser upload against the configured S3 CORS and gateway CSP."],
      },
      {
        heading: "Upgrade, roll back, and retain data",
        paragraphs: ["ffdb-host update-check compares the active release with the latest stable GitHub Release without changing the host. Read the intervening release notes, complete the topology-appropriate coordinated backup, then run update. No arguments selects latest stable; --version pins an exact target for scheduled production rollouts. The controller installs beside the current release and preserves configuration and named volumes."],
        code: { label: "Check and update", language: "sh", code: `sudo ffdb-host version
sudo ffdb-host update-check

# Packaged single-host backup. External providers need their coordinated backup.
sudo ffdb-host backup create /secure/ffdb-before-update.tar.gz
sudo FFDB_REQUIRE_SIGNATURE=1 ffdb-host update
sudo ffdb-host status

# Reproducible alternative after reviewing the exact tag:
sudo ffdb-host update-check --version 0.3.13
sudo FFDB_REQUIRE_SIGNATURE=1 ffdb-host update --version 0.3.13

# If acceptance fails:
sudo ffdb-host rollback 0.3.1 --acknowledge-migration-risk

# Normal shutdown preserves durable state:
sudo ffdb-host stop` },
        callout: { kind: "warning", title: "Volumes are the data", body: "For the direct path, docker compose down preserves named volumes, while docker compose down --volumes deletes the installation's durable state. For the managed path, ffdb-host uninstall is a separate explicit action. Neither command is an update or rollback workflow; take and restore-test a coordinated backup first." },
      },
    ],
  },
  {
    path: "/install/systemd",
    title: "Install with systemd",
    description: "Install separately verified native release components as hardened Linux services behind operator-managed infrastructure.",
    group: "Install",
    sections: [
      {
        heading: "Provision dependencies",
        paragraphs: ["This installation shape assumes a Linux host with systemd, Caddy, curl, tar, cosign, PostgreSQL client tools, SQLite tooling, PostgreSQL, and S3-compatible storage already provisioned. Production FFDB rejects SMTP and requires a Resend API key. Cosign is required by the installed updater because unattended root activation must fail closed when release identity cannot be verified."],
        bullets: [
          "Use durable local or block storage for project SQLite files; verify SQLite locking before using any network filesystem.",
          "Use independently protected backup and organization-metrics directories, plus an independent 32-byte backup master key.",
          "Keep PostgreSQL and the server-facing S3 endpoint on private, allowlisted networks.",
          "Run the API and its database-worker binary from the same release; mixed protocol-v1 releases are unsupported.",
        ],
      },
      {
        heading: "Download and verify one component release",
        paragraphs: ["Choose an announced tag from the canonical GitHub Releases page and download its architecture-matched native archive, signed checksum list, and Sigstore bundle from that same tag. Verify the checksum list before trusting its archive digest; the extracted directory is ffdb-native-VERSION even though the downloaded filename includes the operating system and architecture. Configure the environment in the next step before running the installer because the installer uses its exact public S3 origin to render the gateway Content-Security-Policy."],
        code: { label: "Linux amd64", language: "sh", code: `VERSION=0.3.13
RELEASE_BASE="https://github.com/Forever-Frameworks-LLC/ffdb/releases/download/v$VERSION"
curl -fsSLO "$RELEASE_BASE/SHA256SUMS"
curl -fsSLO "$RELEASE_BASE/SHA256SUMS.sigstore.json"
curl -fsSLO "$RELEASE_BASE/ffdb-native-linux-amd64-$VERSION.tar.gz"

cosign verify-blob SHA256SUMS \\
  --bundle SHA256SUMS.sigstore.json \\
  --certificate-identity "https://github.com/Forever-Frameworks-LLC/ffdb/.github/workflows/release.yml@refs/tags/v$VERSION" \\
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
sha256sum --check --ignore-missing SHA256SUMS

tar -xzf "ffdb-native-linux-amd64-$VERSION.tar.gz"` },
        callout: { kind: "warning", title: "Advanced component path", body: "The pinned-image release bundle is the complete host installation. Native binaries, Cargo crates, and the Homebrew formula are component channels; none alone installs PostgreSQL, S3, email, TLS, configuration, units, gateway assets, backups, or monitoring." },
      },
      {
        heading: "Create the production environment file",
        paragraphs: ["Store secrets in your system secret manager when possible. For a fresh host, stage the complete configuration as a root-only file; the installer creates the ffdb account and copies this file to /etc/ffdb/ffdb.env with owner root:ffdb and mode 0640. Never commit either copy."],
        codes: [
          { label: "/root/ffdb.env", language: "env", code: `FFDB_ENVIRONMENT=production
FFDB_HTTP_BIND=127.0.0.1:8080
FFDB_PUBLIC_BASE_URL=https://ffdb.example.com
FFDB_CORS_ALLOWED_ORIGINS=https://ffdb.example.com
FFDB_TRUSTED_PROXY_CIDRS=127.0.0.1/32,::1/128
FFDB_DATABASE_URL=postgres://ffdb_runtime:REDACTED@postgres.example.net/ffdb
FFDB_POSTGRES_MAX_CONNECTIONS=20
FFDB_NODE_ID=01965555-0000-7000-8000-000000000001
FFDB_NODE_NAME=ffdb-prod-01
FFDB_DATABASE_ROOT=/var/lib/ffdb/projects
FFDB_BACKUP_ROOT=/var/lib/ffdb/backups
FFDB_METRICS_ROOT=/var/lib/ffdb/metrics
FFDB_DATABASE_WORKER=/opt/ffdb/current/bin/ffdb-database-worker
FFDB_WORKER_MAX_PROCESSES=8
FFDB_WORKER_QUEUE_CAPACITY=32
FFDB_MASTER_KEY=REPLACE_WITH_BASE64_32_BYTES
FFDB_BACKUP_MASTER_KEY=REPLACE_WITH_DIFFERENT_BASE64_32_BYTES
FFDB_CURSOR_HMAC_KEY=REPLACE_WITH_AT_LEAST_32_RANDOM_CHARACTERS
FFDB_BOOTSTRAP_TOKEN=REPLACE_WITH_AT_LEAST_32_RANDOM_CHARACTERS
FFDB_S3_ENDPOINT=https://s3.us-east-1.amazonaws.com
FFDB_S3_PUBLIC_ENDPOINT=https://s3.us-east-1.amazonaws.com
FFDB_S3_PUBLIC_ORIGIN=https://s3.us-east-1.amazonaws.com
FFDB_S3_REGION=us-east-1
FFDB_S3_BUCKET=ffdb-production
FFDB_S3_ACCESS_KEY_ID=REDACTED
FFDB_S3_SECRET_ACCESS_KEY=REDACTED
FFDB_S3_ALLOW_PRIVATE_NETWORK=false
FFDB_EMAIL_TRANSPORT=resend
FFDB_RESEND_API_KEY=REDACTED
FFDB_EMAIL_FROM="FFDB <noreply@example.com>"
RUST_LOG=ffdb=info,tower_http=info
FFDB_SYNC_STATE_DIR=/var/lib/ffdb/sync
FFDB_SYNC_MAINTENANCE_INTERVAL_SECONDS=60
FFDB_SYNC_STALE_TEMPORARY_SECONDS=3600` },
          { label: "Protect the staged file", language: "sh", code: `sudo chown root:root /root/ffdb.env
sudo chmod 0600 /root/ffdb.env` },
        ],
        callout: { kind: "warning", title: "Use exact HTTPS origins", body: "FFDB_PUBLIC_BASE_URL and FFDB_S3_PUBLIC_ORIGIN are scheme-and-authority values only, with no path or trailing slash. install-native.sh validates both before rendering the single Caddy gateway. Trust only loopback proxy CIDRs on the native topology; direct clients cannot choose their audit or rate-limit identity." },
      },
      {
        heading: "Install the verified release",
        code: { label: "Terminal", language: "sh", code: `cd "ffdb-native-$VERSION"
sudo ./install-native.sh --verified-release --env-file /root/ffdb.env
systemctl cat ffdb-api.service
systemctl cat ffdb-sync-worker.service
systemctl cat ffdb-gateway.service` },
        paragraphs: ["Pass --verified-release only after the immediately preceding Sigstore and SHA-256 checks succeed. It records the canonical signer identity on this installed release so it can later be selected as a trusted rollback target.", "The native installer creates the ffdb account and directories, installs the complete release below /opt/ffdb/releases/VERSION, atomically selects it through /opt/ffdb/current, copies the staged environment with restricted ownership, installs the units and constrained updater client, publishes the versioned static web assets, renders the Caddy site from the public and storage origins, and validates it before returning. Caddy terminates TLS, serves all three compiled sites, and proxies directly to loopback Axum; nginx is not part of this native path."],
      },
      {
        heading: "Install the API unit",
        code: { label: "/etc/systemd/system/ffdb-api.service", language: "systemd", code: `[Unit]
Description=FFDB API and isolated SQLite worker supervisor
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=ffdb
Group=ffdb
WorkingDirectory=/var/lib/ffdb
EnvironmentFile=/etc/ffdb/ffdb.env
ExecStart=/opt/ffdb/current/bin/ffdb-api
Restart=on-failure
RestartSec=5s
KillSignal=SIGINT
TimeoutStopSec=30s
UMask=0077
NoNewPrivileges=true
PrivateDevices=true
PrivateTmp=true
ProtectClock=true
ProtectControlGroups=true
ProtectHome=true
ProtectHostname=true
ProtectKernelLogs=true
ProtectKernelModules=true
ProtectKernelTunables=true
ProtectSystem=strict
ReadWritePaths=/var/lib/ffdb/projects /var/lib/ffdb/backups /var/lib/ffdb/metrics
ReadWritePaths=/var/lib/ffdb/update-requests
CapabilityBoundingSet=
AmbientCapabilities=
LockPersonality=true
MemoryDenyWriteExecute=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictRealtime=true
RestrictSUIDSGID=true
SystemCallArchitectures=native
LimitNOFILE=65536
TasksMax=512

[Install]
WantedBy=multi-user.target` },
        paragraphs: ["The API process validates and canonicalizes the database-worker binary and its project, backup, and metrics roots before listening. It spawns bounded worker children with project, database, node, route-generation, and backup-key context supplied internally."],
      },
      {
        heading: "Install the sync maintenance unit",
        code: { label: "/etc/systemd/system/ffdb-sync-worker.service", language: "systemd", code: `[Unit]
Description=FFDB durable sync-state maintenance worker
Wants=network-online.target
After=network-online.target ffdb-api.service

[Service]
Type=simple
User=ffdb
Group=ffdb
WorkingDirectory=/var/lib/ffdb
EnvironmentFile=/etc/ffdb/ffdb.env
ExecStart=/opt/ffdb/current/bin/ffdb-sync-worker
Restart=on-failure
RestartSec=5s
KillSignal=SIGINT
TimeoutStopSec=30s
UMask=0077
NoNewPrivileges=true
PrivateDevices=true
PrivateTmp=true
ProtectClock=true
ProtectControlGroups=true
ProtectHome=true
ProtectHostname=true
ProtectKernelLogs=true
ProtectKernelModules=true
ProtectKernelTunables=true
ProtectSystem=strict
ReadWritePaths=/var/lib/ffdb/sync
CapabilityBoundingSet=
AmbientCapabilities=
LockPersonality=true
MemoryDenyWriteExecute=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictRealtime=true
RestrictSUIDSGID=true
SystemCallArchitectures=native
LimitNOFILE=8192
TasksMax=64

[Install]
WantedBy=multi-user.target` },
        callout: { kind: "note", title: "Maintenance only", body: "ffdb-sync-worker cleans durable checkpoint artifacts. Snapshot, push, and pull remain inside the database worker's RLS-authorized user session." },
      },
      {
        heading: "Verify the installed static gateway",
        paragraphs: ["The native installer publishes version-matched landing, docs, and portal assets under /var/www/ffdb and renders one Caddy configuration. Caddy owns public automatic HTTPS and a loopback listener at 127.0.0.1:5173; both serve the compiled sites and proxy directly to Axum at 127.0.0.1:8080. There is no Caddy-to-nginx hop and Vite is not a runtime service."],
        code: { label: "Terminal", language: "sh", code: `sudo caddy validate --config /etc/ffdb/Caddyfile --adapter caddyfile
find /var/www/ffdb -maxdepth 2 -type f -name index.html -print` },
      },
      {
        heading: "Start and verify",
        code: { label: "Terminal", language: "sh", code: `sudo systemctl daemon-reload
sudo systemctl enable --now ffdb-api.service ffdb-sync-worker.service
sudo systemctl enable --now ffdb-gateway.service

sudo systemctl --no-pager --full status \\
  ffdb-api.service ffdb-sync-worker.service ffdb-gateway.service
journalctl -u ffdb-api.service -u ffdb-sync-worker.service -u ffdb-gateway.service --since today
# Direct Axum service diagnostic, available only on this host.
curl --fail http://127.0.0.1:8080/readyz
# Loopback request through the same Caddy process that owns public TLS.
curl --fail http://127.0.0.1:5173/readyz` },
        paragraphs: ["From outside the host, repeat readiness through HTTPS and directly reload nested /docs/ and /app/ routes. Normal clients use the gateway; the direct Axum listener is for local service diagnostics and must remain loopback-only. On supported distributions, systemd-analyze security is a useful review aid, but its score does not replace functional or threat-model testing."],
      },
      {
        heading: "Upgrade without mixing worker protocols",
        paragraphs: ["After this installer is present, use Global administration → Updates or the constrained ffdb-update host client. The root agent verifies the target manifest and Sigstore identity, creates a coordinated backup, installs beside the current release, switches the complete release atomically, restarts the application units, and requires readiness. The gateway and persisted job let the portal reconnect while Axum restarts."],
        bullets: ["Review the target release notes, state schema, rollback floor, and signature identity before confirming.", "Never rebuild or mix individual binaries; API, database worker, sync worker, units, and web assets come from one release.", "Automatic checks are enabled, but automatic application stays off until an owner explicitly configures a UTC maintenance window.", "Rollback selects only a verified installed release that passes the state-schema and rollback-floor guard.", "Run RLS isolation, auth refresh, storage signing, sync, observability, and restore acceptance before treating the rollout as complete."],
        callout: { kind: "warning", title: "Coordinated release boundary", body: "Protocol version 1 has strict decoding and no dual-version negotiation. Do not perform an in-place rolling mix of API and database-worker releases." },
      },
    ],
  },
  {
    path: "/self-hosting",
    title: "Self-hosting",
    description: "Understand the deployable services, durable state, and production separation points.",
    group: "Install",
    sections: [
      {
        heading: "Choose an installation shape",
        bullets: [
          "Use ffdb-compose-bundle-VERSION.tar.gz with the explicit single-host profile for loopback-only evaluation, or with the external profile and operator-managed PostgreSQL, HTTPS S3, real email, and TLS for internet production.",
          "Use an architecture-matched ffdb-native-linux-ARCH-VERSION.tar.gz component artifact for advanced systemd installation when every dependency and service boundary is operator-managed.",
          "Select server artifacts only from an announced tag on the canonical Forever-Frameworks-LLC/ffdb GitHub Releases page; do not infer availability from an example version.",
          "Install all public @ffdb packages from npm at the exact server version. Use checksum-listed tarballs from the matching server tag for verified offline installation.",
          "Use Kubernetes only after project placement gives each stateful API/worker pod a disjoint project database set.",
        ],
      },
      {
        heading: "Service topology",
        bullets: [
          "apps/api combines the asynchronous Rust HTTP/control-plane service with the node-local SQLite worker supervisor; the current release runs one API owner per routed project set.",
          "apps/database-worker executes project SQL inside an isolated process boundary.",
          "apps/sync-worker performs asynchronous sync and maintenance work.",
          "PostgreSQL holds control-plane state; project SQLite files and encrypted backups use separate durable volumes.",
          "An S3-compatible provider stores object bytes while project SQLite stores authorization metadata.",
        ],
      },
      {
        heading: "Durable state map",
        bullets: [
          "PostgreSQL: organizations, projects, credentials, routing, jobs, audit state, and platform migrations.",
          "Project root: one private SQLite application database per project.",
          "Backup root: encrypted FFDB backup envelopes; plaintext exists only in guarded transient worker staging.",
          "S3-compatible provider: object bytes only; SQLite remains authoritative for metadata and authorization.",
          "Sync state root: durable maintenance checkpoints, not user data or a second authorization path.",
        ],
      },
      {
        heading: "Production baseline",
        paragraphs: ["Terminate TLS before FFDB, use a narrow trusted proxy boundary, keep provider credentials server-side, isolate project and backup storage, and configure the public S3 endpoint separately from the internal endpoint."],
        callout: { kind: "warning", title: "No raw database access", body: "Do not mount project SQLite volumes into application containers or expose worker sockets. All application access must cross the verified FFDB request path." },
      },
      {
        heading: "Contributor source workflow",
        paragraphs: ["A source checkout is the supported fallback for development, auditing, and release engineering—not an operator installation. It requires Rust 1.96.1, Node 24+, pnpm 11.6, Docker, and Compose. The Makefile is the universal repository entry point: build compiles every Rust and TypeScript target, verify runs the complete quality suite, and compose-rebuild creates the contributor stack from the checkout."],
        code: { label: "Contributor checkout only", language: "sh", code: `corepack enable
pnpm install --frozen-lockfile
make build
make verify
make compose-rebuild
make status

# Compiled nginx gateway built from this checkout.
curl --fail http://127.0.0.1:5173/readyz
# Contributor-only direct Axum diagnostic.
curl --fail http://127.0.0.1:8080/readyz` },
        callout: { kind: "note", title: "Different lifecycle", body: "The contributor Compose stack is disposable build infrastructure and publishes direct Axum diagnostics. It does not install immutable releases under /opt/ffdb, place configuration under /etc/ffdb, or replace ffdb-host for production lifecycle management." },
      },
    ],
  },
  {
    path: "/configuration",
    title: "Configuration",
    description: "Configure server trust boundaries, independent secrets, provider endpoints, project identity, and runtime-specific client stores.",
    group: "Install",
    sections: [
      {
        heading: "Generate independent secrets",
        paragraphs: ["FFDB validates secret length at startup. Generate each value independently and store it in a secret manager; never copy the disposable values from compose.yaml into production."],
        code: { label: "Generate one value per line", language: "sh", code: `# FFDB_MASTER_KEY: exactly 32 random bytes, base64 encoded
openssl rand -base64 32

# FFDB_BACKUP_MASTER_KEY: a different 32-byte base64 value
openssl rand -base64 32

# FFDB_CURSOR_HMAC_KEY: at least 32 random characters
openssl rand -hex 32

# FFDB_BOOTSTRAP_TOKEN: at least 32 random characters
openssl rand -hex 32` },
        callout: { kind: "warning", title: "Key separation", body: "The master-envelope key, backup key, cursor HMAC key, and bootstrap token have different roles and rotation procedures. Never reuse one value for another." },
      },
      {
        heading: "Server configuration groups",
        bullets: [
          "HTTP: FFDB_HTTP_BIND, FFDB_PUBLIC_BASE_URL, an exact comma-separated FFDB_CORS_ALLOWED_ORIGINS instance fallback for operator/non-project routes, and the narrow FFDB_TRUSTED_PROXY_CIDRS boundary. Application browser origins are project settings in the portal.",
          "PostgreSQL: FFDB_DATABASE_URL and FFDB_POSTGRES_MAX_CONNECTIONS.",
          "Workers and usage: FFDB_NODE_ID, FFDB_NODE_NAME, FFDB_DATABASE_ROOT, FFDB_BACKUP_ROOT, FFDB_METRICS_ROOT, FFDB_DATABASE_WORKER, FFDB_WORKER_MAX_PROCESSES, and FFDB_WORKER_QUEUE_CAPACITY. A project worker executes one framed request at a time.",
          "Security: FFDB_MASTER_KEY, FFDB_BACKUP_MASTER_KEY, FFDB_CURSOR_HMAC_KEY, and FFDB_BOOTSTRAP_TOKEN.",
          "Storage: internal and browser-visible S3 endpoints, region, bucket, access key, secret key, and the private-network opt-in.",
          "Email: production uses FFDB_EMAIL_TRANSPORT=resend, FFDB_RESEND_API_KEY, and FFDB_EMAIL_FROM.",
        ],
      },
      {
        heading: "Production validation",
        bullets: [
          "FFDB_PUBLIC_BASE_URL and every CORS origin must use HTTPS. Origins cannot include credentials, query strings, fragments, or non-root paths.",
          "Leave FFDB_TRUSTED_PROXY_CIDRS empty for direct Axum access. Native installs trust loopback only; Docker installs use the deployment's explicit isolated subnet. Never use 0.0.0.0/0 or ::/0.",
          "FFDB_MASTER_KEY and FFDB_BACKUP_MASTER_KEY must each decode to exactly 32 bytes.",
          "FFDB_CURSOR_HMAC_KEY and FFDB_BOOTSTRAP_TOKEN must each contain at least 32 characters.",
          "The browser-facing FFDB_S3_PUBLIC_ENDPOINT must be public HTTPS and cannot resolve to private or local addresses.",
          "A private HTTPS FFDB_S3_ENDPOINT requires FFDB_S3_ALLOW_PRIVATE_NETWORK=true and remains bound to its exact hostname.",
          "SMTP is rejected in production. Resend credentials must remain server-side.",
          "Database, backup, and metrics roots must be specific paths, not /, empty paths, or paths containing parent traversal.",
        ],
      },
      {
        heading: "Client options",
        code: { label: "ffdb.ts", language: "ts", code: `import { BrowserSessionStore, FFDBClient } from "@ffdb/client";

const ffdb = new FFDBClient({
  baseUrl: "https://data.example.com",
  projectId: "019fc39c-ddbd-7d12-9849-e4ee35310132",
  sessionStore: new BrowserSessionStore(
    window.sessionStorage,
    "my-app.ffdb-session",
  ),
});` },
        bullets: [
          "baseUrl must use HTTP or HTTPS and cannot contain user information, a query, or a fragment.",
          "projectId identifies the project for SQL, auth, storage, and sync routes.",
          "developerKey enables trusted administration and must be omitted from browser and mobile applications.",
          "Provide fetch and a SessionStore implementation when the runtime does not expose browser defaults.",
          "BrowserSessionStore persists the end-user access/refresh pair in the Storage object you supply; sessionStorage limits persistence across browser restarts but does not defend against same-origin XSS.",
        ],
      },
      {
        heading: "Choose the browser API origin",
        paragraphs: [
          "The packaged portal uses its current browser origin when VITE_FFDB_API_URL is absent, so http://127.0.0.1:5173/app/ calls the same installed gateway that served it. A separately built application may set VITE_FFDB_API_URL to an explicit FFDB origin.",
          "Local acceptance does not depend on ffdb.forever-frameworks.com. A pre-deployment 403 from that public hostname describes the remote host only; verify the installed release through loopback until TLS and DNS are ready.",
        ],
        code: { label: "Application environment", language: "env", code: `# Local acceptance build
VITE_FFDB_API_URL=http://127.0.0.1:5173

# Production build after TLS and DNS cutover
VITE_FFDB_API_URL=https://data.example.com` },
        callout: { kind: "note", title: "Use one origin intentionally", body: "Leave the override unset for the packaged portal. For a separately served application, add its browser origin under the selected project's Auth → Policy → Application URLs; no host restart is required." },
      },
    ],
  },
  {
    path: "/database",
    title: "Database architecture",
    description: "How FFDB combines a PostgreSQL control plane with isolated SQLite application databases.",
    group: "Database",
    sections: [
      {
        heading: "Control plane and data plane",
        paragraphs: ["PostgreSQL coordinates tenant and service state. Each FFDB project owns an application SQLite file. Database workers open those files through trusted paths and apply request-specific limits and immutable auth context."],
      },
      {
        heading: "Defense in depth",
        bullets: ["Server-side SQL parsing and statement classification", "SQLite preparation and resource limits", "Authorizer denial of protected internal objects", "Generated RLS views and INSTEAD OF triggers", "Stable error normalization without raw SQLite internals"],
        callout: { kind: "note", title: "SQLite semantics remain", body: "FFDB documents intentional differences and never claims byte-for-byte PostgreSQL behavior." },
      },
      {
        heading: "Trace one request",
        paragraphs: ["A client request never opens a database file directly. The API authenticates the credential and project, dispatches to the bounded worker for that project database, and returns an ordered public result or stable error envelope."],
        code: { label: "Request boundary", language: "yaml", code: `request:
  route: POST /v1/projects/{project_id}/query
  credential: end-user session or project developer key
control_plane:
  store: PostgreSQL
  resolves: organization, project, credential, node placement
data_plane:
  file: /var/lib/ffdb/projects/{database_id}.sqlite3
  process: bounded ffdb-database-worker
  enforcement: parser + authorizer + limits + RLS
response:
  success: ordered columns and lossless tagged values
  failure: stable error code and X-Request-Id` },
      },
    ],
  },
  {
    path: "/queries",
    title: "Queries and transactions",
    description: "Execute bounded parameterized SQL and consume ordered, lossless results.",
    group: "Database",
    sections: [
      {
        heading: "Parameterized queries",
        code: { label: "documents.ts", language: "ts", code: queryExample },
        paragraphs: ["Parameters use the tagged null, integer, real, text, or base64 blob representation. Ordered rows are arrays so duplicate column names and column order remain intact."],
      },
      {
        heading: "Result values",
        bullets: ["Safe integers are JavaScript numbers.", "Integers outside JavaScript's safe range remain decimal strings.", "BLOB values use the object form { $blob: \"base64...\" }.", "Use options.max_rows and cancellation to bound application work."],
      },
    ],
  },
  {
    path: "/migrations",
    title: "Migrations",
    description: "Apply explicit, checksummed up/down migrations through trusted developer workflows.",
    group: "Database",
    sections: [
      {
        heading: "Migration format",
        code: { label: "migrations/1754179200000_add_documents.sql", language: "sql", code: `-- migrate:up
CREATE TABLE documents (id TEXT PRIMARY KEY, owner_id TEXT NOT NULL, title TEXT NOT NULL);

-- migrate:down
DROP TABLE documents;` },
        paragraphs: ["Both directions are required. ffdb migration create add_documents writes <epoch-milliseconds>_add_documents.sql in the current directory; 1754179200000 is an illustrative timestamp. The CLI hashes the stable id, name, up SQL, and down SQL to match the Rust protocol."],
      },
      {
        heading: "Apply and inspect",
        code: { label: "Terminal", language: "sh", code: `mkdir -p migrations
cd migrations
ffdb migration create add_documents
# Example output: { "path": "1754179200000_add_documents.sql" }
ffdb --project "$FFDB_PROJECT_ID" --key "$FFDB_DEVELOPER_KEY" \\
  migration apply 1754179200000_add_documents.sql
ffdb migration status` },
      },
    ],
  },
  {
    path: "/row-level-security",
    title: "Row-level security",
    description: "Use the documented PostgreSQL-style policy subset to scope every end-user query.",
    group: "Database",
    sections: [
      {
        heading: "Policy DDL",
        code: { label: "documents.sql", language: "sql", code: `ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE documents FORCE ROW LEVEL SECURITY;

CREATE POLICY documents_read ON documents
  FOR SELECT TO authenticated
  USING (owner_id = auth.uid());

CREATE POLICY documents_write ON documents
  FOR INSERT TO authenticated
  WITH CHECK (owner_id = auth.uid());` },
      },
      {
        heading: "Combination and enforcement",
        paragraphs: ["For the current command and role, permissive policies combine with OR and restrictive policies combine with AND. RLS with no applicable permissive policy denies access."],
        bullets: ["SELECT and DELETE use USING.", "INSERT uses WITH CHECK.", "UPDATE evaluates both the old row and new row.", "FOR ALL participates in every command.", "FORCE removes the trusted developer bypass."],
        callout: { kind: "warning", title: "Policy changes and replicas", body: "A policy, claim scope, or schema change can require offline clients to destroy cached rows and resnapshot." },
      },
    ],
  },
  {
    path: "/sql-support",
    title: "SQL support",
    description: "Understand the supported statement surface and intentional SQLite compatibility boundaries.",
    group: "Database",
    sections: [
      {
        heading: "Application statements",
        bullets: ["SELECT and constrained WITH queries", "INSERT, UPDATE, and DELETE through generated policy triggers", "Explicit transactions through the SDK transaction method", "Schema and policy DDL only through trusted migration mode"],
      },
      {
        heading: "Current protected-table restrictions",
        bullets: ["UPSERT against an RLS view is rejected; use a bounded UPDATE then conditional INSERT transaction.", "Callers must provide values for columns whose backing defaults they intend to use.", "Generated columns are readable but excluded from generated INSERT and UPDATE assignments.", "Unsupported syntax returns an error and never falls back to unprotected execution."],
      },
      {
        heading: "Use a transaction for dependent writes",
        paragraphs: ["Statements execute in order and commit atomically. Every value remains a tagged parameter; do not interpolate identifiers or user input into SQL. If a statement uses unsupported syntax or violates RLS, the transaction rolls back."],
        code: { label: "documents.ts", language: "ts", code: `const [updated, audited] = await ffdb.transaction({
  statements: [
    {
      sql: "update documents set title = ?1 where id = ?2",
      parameters: [
        { type: "text", value: nextTitle },
        { type: "text", value: documentId },
      ],
    },
    {
      sql: "insert into document_events (id, document_id, kind) values (?1, ?2, ?3)",
      parameters: [
        { type: "text", value: eventId },
        { type: "text", value: documentId },
        { type: "text", value: "renamed" },
      ],
    },
  ],
});` },
      },
    ],
  },
  {
    path: "/authentication",
    title: "Authentication",
    description: "Register users, verify email, manage sessions, and let the client rotate access tokens.",
    group: "Auth and storage",
    sections: [
      {
        heading: "End-user sessions",
        code: { label: "auth.ts", language: "ts", code: `const callback = new URL("/auth/complete", window.location.origin).href;

await ffdb.auth.register({ email, password, redirect_to: callback });

const session = await ffdb.auth.signIn(email, password);
const sessions = await ffdb.auth.sessions();
await ffdb.auth.signOut();` },
        paragraphs: ["The verification email opens a short FFDB-hosted transition, completes the one-time action, and returns with location.replace() to the callback supplied by your app. Add the browser origin and exact callback under the selected project's Auth → Policy → Application URLs. Both policies are validated live by the API, with no host restart. The client stores the returned access/refresh pair, deduplicates concurrent refreshes, and retries one unauthorized end-user request after a successful rotation."],
      },
      {
        heading: "Developer credentials",
        paragraphs: ["Platform sessions manage organizations and projects. Project developer keys manage schema, policies, buckets, backups, and administrative SQL. End-user access tokens carry project-scoped subject and claims for RLS."],
        callout: { kind: "warning", title: "Never mix credential modes", body: "Developer keys do not belong in browser or mobile bundles. End-user clients should be constructed without developerKey." },
      },
      {
        heading: "Install the versioned email components",
        paragraphs: ["@ffdb/email-components contains the release's React Email defaults and allowed-variable manifest. Install its exact server-matched npm version, or use the checksum-listed tarball from the matching GitHub tag for offline workflows. An application does not need it merely to register or sign in users."],
        code: { label: "Terminal", language: "sh", code: `VERSION=0.3.13
npm install --save-exact "@ffdb/email-components@$VERSION"` },
      },
    ],
  },
  {
    path: "/jwt-claims",
    title: "JWT claims",
    description: "Use verified subject, role, and claims inside FFDB policy expressions.",
    group: "Auth and storage",
    sections: [
      {
        heading: "Policy functions",
        code: { label: "tenant-policy.sql", language: "sql", code: `CREATE POLICY tenant_documents ON documents
  FOR SELECT TO authenticated
  USING (
    owner_id = auth.uid()
    OR organization_id = auth.claim('organization_id')
  );` },
        bullets: ["auth.uid() returns the verified end-user subject.", "auth.role() returns the verified project role.", "auth.claim(name) reads an allowlisted immutable claim value.", "Policy evaluation never trusts a caller-authored SQL function or token payload."],
      },
    ],
  },
  {
    path: "/storage",
    title: "Object storage",
    description: "Authorize S3-compatible object operations through the same RLS-secured project session as SQL.",
    group: "Auth and storage",
    sections: [
      {
        heading: "Storage model",
        paragraphs: ["Bytes live in an S3-compatible provider. Buckets, object metadata, ownership, checksums, versions, quotas, reservations, and multipart state live in the project database. Provider listings are never an authorization source."],
        code: { label: "upload.ts", language: "ts", code: `await ffdb.storage.upload(
  "avatars",
  "users/" + userId + "/avatar.png",
  file,
  { sizeBytes: file.size, contentType: file.type },
);

const page = await ffdb.storage.list("avatars", {
  prefix: "users/" + userId + "/", limit: 50,
});` },
      },
      {
        heading: "Authorization sequence",
        bullets: ["Validate the logical bucket and key.", "Evaluate operation-specific RLS with immutable auth context.", "Reserve quota and a single-use operation durably in SQLite.", "Mint a short-lived grant and method/key-bound provider URL.", "Commit metadata only after the provider succeeds."],
        callout: { kind: "warning", title: "Signed URLs are capabilities", body: "Keep them short-lived, method- and key-bound, out of logs and analytics, and protected with size/checksum conditions for writes." },
      },
    ],
  },
  {
    path: "/multipart-uploads",
    title: "Multipart uploads",
    description: "Upload large objects with durable authorization, part binding, and explicit completion or abort.",
    group: "Auth and storage",
    sections: [
      {
        heading: "Lifecycle",
        code: { label: "multipart.ts", language: "ts", code: `const upload = await ffdb.storage.createMultipart(
  "videos", key, { sizeBytes: file.size, contentType: file.type },
);

const part = await ffdb.storage.uploadPart(
  upload, 1, firstChunk,
  { sizeBytes: firstChunk.size, contentType: file.type },
);

await ffdb.storage.completeMultipart(upload, [part], {
  sizeBytes: file.size,
  contentType: file.type,
});` },
        bullets: ["Part numbers must be unique integers from 1 to 10,000.", "Every part is bound to the logical upload and authorization context.", "Completion verifies committed bytes and final checksum before consuming quota.", "Abort abandoned uploads; developer cleanup retries expired reservations."],
      },
    ],
  },
  {
    path: "/sync",
    title: "Sync protocol",
    description: "Synchronize logical row changes with snapshots, opaque cursors, push, and pull.",
    group: "Sync and offline",
    sections: [
      {
        heading: "Snapshot and pull",
        paragraphs: ["A snapshot returns RLS-visible tables at a consistent schema version and server sequence. Pull returns later logical changes and a replacement opaque cursor. Cursors are authenticated, bounded, project/schema/scope bound, and must not be parsed or logged."],
        code: { label: "low-level-sync.ts", language: "ts", code: `let snapshot = await ffdb.sync.snapshot(["documents"]);
let cursor = snapshot.cursor;

const pull = await ffdb.sync.pull(cursor, 1000);
if (pull.control?.type === "resnapshot_required" ||
    pull.control?.type === "invalidate_scope") {
  snapshot = await ffdb.sync.snapshot(["documents"]);
  cursor = snapshot.cursor;
} else {
  for (const change of pull.changes) {
    await applyLogicalChange(change);
  }
  cursor = pull.cursor;
}` },
        callout: { kind: "warning", title: "Replace before serving", body: "On either invalidation control, delete the affected scoped replica and atomically replace it with the new snapshot before serving cached rows again." },
      },
      {
        heading: "Push and controls",
        bullets: ["Every mutation has a unique mutation id and base row version.", "Server sequence—not client time—orders last-write-wins conflicts.", "Accepted mutations commit independently and return per-item results.", "Schema, policy, scope, or retention changes can return resnapshot_required or invalidate_scope."],
        code: { label: "push.ts", language: "ts", code: `const result = await ffdb.sync.push({
  schema_version: snapshot.schema_version,
  mutations: [{
    mutation_id: mutationId,
    table: "documents",
    primary_key: documentId,
    operation: "update",
    values: { title: nextTitle },
    base_row_version: currentRowVersion,
    client_timestamp_ms: Date.now(),
  }],
});

for (const mutation of result.results) {
  if (mutation.status === "rejected") {
    await moveToRejectedQueue(mutation.mutation_id, mutation.error_code);
  }
}` },
        callout: { kind: "note", title: "Not WAL replication", body: "FFDB synchronizes logical RLS-filtered rows. SQLite WAL frames never cross the public API." },
      },
      {
        heading: "Choose the API level",
        bullets: [
          "Use ffdb.sync.snapshot/pull/push when your application already owns a transactional replica engine and retry queue.",
          "Use OfflineSyncClient when you want FFDB's tested snapshot → push → pull orchestration around a ReplicaAdapter.",
          "Use IndexedDbReplica in browsers and NodeSQLiteReplica in Node 24+ for first-party durable local state.",
          "Use NativeSQLiteReplica on React Native after wrapping the runtime's SQLite transaction and execute APIs.",
          "Use MemoryReplica only for tests and short-lived demonstrations; it is not durable across restarts.",
        ],
      },
    ],
  },
  {
    path: "/offline",
    title: "Offline replicas",
    description: "Connect the logical protocol to a transactional local replica adapter.",
    group: "Sync and offline",
    sections: [
      {
        heading: "OfflineSyncClient",
        code: { label: "sync.ts", language: "ts", code: `import { OfflineSyncClient } from "@ffdb/sync-client";

const sync = new OfflineSyncClient(ffdb, replica, {
  pushBatchSize: 100,
  pullBatchSize: 1000,
});

await sync.mutate(mutation);
const localRow = await sync.getRow(mutation.table, mutation.primary_key);
const localRows = await sync.listRows(mutation.table);
await sync.sync();` },
        paragraphs: ["The adapter provides transactional row replacement, cursor persistence, pending mutation storage, rejection tracking, and deterministic typed row reads. mutate() makes inserts, partial updates, and deletes visible locally in the same atomic operation that queues them. FFDB ships IndexedDB for browsers, built-in SQLite for Node 24+, native-SQLite contracts for React Native, and a memory adapter only for tests."],
      },
      {
        heading: "Subscribe and schedule",
        code: { label: "sync-lifecycle.ts", language: "ts", code: `const unsubscribe = sync.subscribe((state) => {
  renderSyncState({
    phase: state.phase,
    pending: state.pending,
    lastSyncedAtMs: state.lastSyncedAtMs,
    error: state.error,
  });
});

window.addEventListener("online", () => {
  void sync.sync();
});

await sync.sync();
// Call unsubscribe() when the application scope is disposed.` },
        bullets: [
          "sync() deduplicates concurrent calls and reports snapshot, push, pull, idle, or error phase.",
          "Pending mutations are pushed in bounded batches before pulling later server changes.",
          "Use application lifecycle and connectivity signals as hints, not proof that a network request will succeed.",
          "Keep a retry/backoff policy outside tight render loops and surface rejected mutations to the user.",
        ],
      },
      {
        heading: "Replica choices",
        bullets: [
          "Browser: import IndexedDbReplica from @ffdb/sync-client/browser and use a database name scoped to the project plus signed-in authorization identity.",
          "React Native / Expo: wrap the runtime driver as NativeSQLiteDriver and use NativeSQLiteReplica.",
          "Node 24+: import NodeSQLiteReplica from @ffdb/sync-client/node and use an owner-only SQLite path; close it during graceful shutdown.",
          "Tests: MemoryReplica is useful for contract tests but loses rows, cursor, and pending mutations on reload or process exit.",
          "Custom engines: implement every ReplicaTransaction method atomically, including snapshot replacement and cursor movement.",
        ],
        callout: { kind: "warning", title: "RLS scope is cache scope", body: "Never merge rows from different users, tokens, roles, or claim sets into one visible replica. Scope invalidation means previously cached data may no longer be authorized." },
      },
    ],
  },
  {
    path: "/conflicts",
    title: "Conflict behavior",
    description: "Understand server-sequence last-write-wins, mutation receipts, tombstones, and resnapshot behavior.",
    group: "Sync and offline",
    sections: [
      {
        heading: "Deterministic ordering",
        paragraphs: ["The later server commit sequence wins update/update, update/delete, and delete/recreate conflicts. Client timestamps are diagnostic only and never order writes."],
        bullets: ["Mutation IDs are idempotent within the verified subject and access-token scope.", "Reusing an ID with different content is rejected.", "Deletes create retained tombstones so stale replicas cannot resurrect rows.", "Compaction respects separate change, tombstone, cursor, and receipt horizons."],
      },
      {
        heading: "Handle every mutation result",
        paragraphs: ["A push may apply, deduplicate, supersede, or reject each mutation independently. Remove applied and duplicate work from the pending queue, refresh superseded rows from the server sequence, and keep rejected work visible with its stable error code."],
        code: { label: "conflict-handler.ts", language: "ts", code: `const push = await ffdb.sync.push({
  schema_version: snapshot.schema_version,
  mutations: pendingMutations,
});

for (const item of push.results) {
  switch (item.status) {
    case "applied":
    case "duplicate":
      await removePendingMutation(item.mutation_id);
      break;
    case "superseded":
      await refreshAffectedRow(item.server_sequence);
      break;
    case "rejected":
      await showSyncIssue(item.mutation_id, item.error_code);
      break;
  }
}` },
      },
    ],
  },
  {
    path: "/client",
    title: "TypeScript client",
    description: "Use the only TypeScript package that communicates directly with the FFDB HTTP API.",
    group: "SDKs and tools",
    sections: [
      {
        heading: "Install and construct",
        paragraphs: ["Install @ffdb/client at the exact version named by the server release. Inspect npm only for discovery; do not float the production dependency range."],
        codes: [
          { label: "Public npm registry", language: "sh", code: `npm view @ffdb/client dist-tags --json
npm install --save-exact @ffdb/client@0.3.13
npm ls @ffdb/client` },
          { label: "Verified offline release artifact", language: "sh", code: `npm install --save-exact ./ffdb-client-0.3.13.tgz` },
          { label: "src/ffdb.ts", language: "ts", code: clientSetup },
        ],
        callout: { kind: "note", title: "Check updates without floating", body: "Run npm outdated @ffdb/client to discover a newer version, read its server and package release notes, then install that exact version and run your application tests. Use only checksum-listed tarballs for offline installation." },
      },
      {
        heading: "Public capabilities",
        bullets: ["Developer queries, transactions, migrations, schema, and policies", "End-user registration, sessions, password reset, and token rotation", "Organizations, projects, API keys, settings, logs, and backups", "Storage upload/download signing and multipart lifecycle", "Logical snapshot, pull, and push"],
      },
      {
        heading: "Generate IDs and cancel work",
        code: { label: "documents.ts", language: "ts", code: `import { generateId } from "@ffdb/client";

const controller = new AbortController();
const id = generateId("doc_");

const request = ffdb.query({
  sql: "insert into documents (id, owner_id, title) values (?1, auth.uid(), ?2)",
  parameters: [
    { type: "text", value: id },
    { type: "text", value: title },
  ],
}, { signal: controller.signal });

controller.abort();
await request;` },
        paragraphs: ["generateId uses crypto.randomUUID and accepts an optional prefix of at most 32 letters, numbers, underscores, or hyphens. Aborting rejects local request work; it does not imply that already-committed server work was rolled back."],
      },
    ],
  },
  {
    path: "/react",
    title: "React",
    description: "Provide an FFDB client and use hooks for auth, queries, sync, sessions, and storage uploads.",
    group: "SDKs and tools",
    sections: [
      {
        heading: "Install the matched packages",
        paragraphs: ["Install @ffdb/react and @ffdb/sync-client from npm at the exact @ffdb/client and server version. The matching GitHub tag also contains checksum-listed tarballs for verified offline installation."],
        code: { label: "Terminal", language: "sh", code: `VERSION=0.3.13
npm install --save-exact "@ffdb/client@$VERSION" \\
  "@ffdb/sync-client@$VERSION" "@ffdb/react@$VERSION"` },
      },
      {
        heading: "Providers and hooks",
        code: { label: "App.tsx", language: "tsx", code: `import { AuthProvider, FFDBProvider, useQuery } from "@ffdb/react";

function App() {
  return (
    <FFDBProvider client={ffdb}>
      <AuthProvider><Documents /></AuthProvider>
    </FFDBProvider>
  );
}

function Documents() {
  const query = useQuery({ sql: "select id, title from documents" }, []);
  // Render query.status, query.data, and query.error.
}` },
        bullets: ["useAuth manages current session, sign-in, sign-out, and refresh.", "useQuery cancels superseded HTTP work with AbortController.", "useSync subscribes to an OfflineSyncClient.", "useStorageUpload tracks direct provider upload state."],
      },
    ],
  },
  {
    path: "/react-native",
    title: "React Native",
    description: "Persist sessions and connect offline replicas without relying on browser storage APIs.",
    group: "SDKs and tools",
    sections: [
      {
        heading: "Install the native integration",
        paragraphs: ["React Native uses exact-version @ffdb/client, @ffdb/sync-client, and @ffdb/react-native npm packages. The matching tag provides signed offline tarballs. The integration supplies contracts and adapters; your app still chooses its SecureStore-like and SQLite implementations."],
        code: { label: "Terminal", language: "sh", code: `VERSION=0.3.13
npm install --save-exact "@ffdb/client@$VERSION" \\
  "@ffdb/sync-client@$VERSION" "@ffdb/react-native@$VERSION"` },
      },
      {
        heading: "Runtime adapters",
        paragraphs: ["@ffdb/react-native supplies contracts for asynchronous session storage and native SQLite replicas. It does not bundle a direct Expo SQLite or SecureStore implementation; wrap the runtime APIs your application already owns."],
        code: { label: "native-ffdb.ts", language: "ts", code: `import { FFDBClient } from "@ffdb/client";
import {
  NativeSQLiteReplica,
  ReactNativeSessionStore,
  type AsyncKeyValueStorage,
  type NativeSQLiteDriver,
} from "@ffdb/react-native";
import { OfflineSyncClient } from "@ffdb/sync-client";

declare const secureStorage: AsyncKeyValueStorage;
declare const sqliteDriver: NativeSQLiteDriver;

const ffdb = new FFDBClient({
  baseUrl: "https://data.example.com",
  projectId: "your-project-id",
  sessionStore: new ReactNativeSessionStore(secureStorage),
});

const replica = new NativeSQLiteReplica(sqliteDriver);
await replica.initialize();
export const sync = new OfflineSyncClient(ffdb, replica);

export const readDraft = (id: string) => sync.getRow("drafts", id);
export const listDrafts = () => sync.listRows("drafts");` },
        bullets: [
          "AsyncKeyValueStorage implements async getItem, setItem, and removeItem; inject a SecureStore-like encrypted implementation for refresh tokens.",
          "NativeSQLiteDriver implements parameterized execute plus a transaction callback that keeps every callback statement on the same atomic transaction.",
          "The SQLite runtime must support STRICT tables and ON CONFLICT ... DO UPDATE, which the replica uses for metadata, rows, pending mutations, and rejections.",
          "NativeSQLiteReplica owns reserved __ffdb_client_* tables in that local database.",
          "@ffdb/client owns HTTP and @ffdb/sync-client owns snapshot/push/pull orchestration.",
          "getRow and listRows return decoded local records without exposing the private SQLite connection.",
        ],
        callout: { kind: "note", title: "Keep cursors opaque", body: "Persist the cursor exactly as returned. Do not decode, compare, log, or construct cursors in application code." },
      },
      {
        heading: "Schedule mobile sync",
        paragraphs: ["The package intentionally does not import NetInfo, AppState, Expo SQLite, or SecureStore. The application owns those dependencies and decides when background network work is permitted."],
        code: { label: "mobile-lifecycle.ts", language: "ts", code: `import type { SyncMutation } from "@ffdb/client";

export async function onNetworkAvailable(): Promise<void> {
  await sync.sync();
}

export async function onApplicationActive(): Promise<void> {
  await sync.sync();
}

export function onUserMutation(mutation: SyncMutation): Promise<void> {
  return sync.mutate(mutation);
}` },
        bullets: ["Wire onNetworkAvailable to your NetInfo-equivalent online transition.", "Wire onApplicationActive to the React Native AppState-equivalent active transition.", "Do not run a tight polling loop while backgrounded.", "A transient initialize() failure is retryable; the replica clears its failed initialization promise before the next operation."],
      },
    ],
  },
  {
    path: "/sync-client",
    title: "Sync client",
    description: "Use the browser, Node, or custom transactional replica behind @ffdb/sync-client.",
    group: "SDKs and tools",
    sections: [
      {
        heading: "Install the runtime-neutral package",
        paragraphs: ["Install @ffdb/sync-client from npm at the same version as @ffdb/client and the server. The matching GitHub Release contains a signed tarball for offline installation. Browser and Node adapters are subpath exports of this package."],
        code: { label: "Terminal", language: "sh", code: `VERSION=0.3.13
npm install --save-exact "@ffdb/client@$VERSION" \\
  "@ffdb/sync-client@$VERSION"` },
      },
      {
        heading: "Use the bundled browser and Node adapters",
        codes: [
          { label: "browser-sync.ts", language: "ts", code: `import { OfflineSyncClient } from "@ffdb/sync-client";
import { IndexedDbReplica } from "@ffdb/sync-client/browser";

// Never share this database name across users or authorization scopes.
const replica = new IndexedDbReplica(\`ffdb-\${projectId}-\${userId}\`);
export const sync = new OfflineSyncClient(ffdb, replica);` },
          { label: "node-sync.ts", language: "ts", code: `import { OfflineSyncClient } from "@ffdb/sync-client";
import { NodeSQLiteReplica } from "@ffdb/sync-client/node";

const replica = new NodeSQLiteReplica(
  "/var/lib/my-app/ffdb-project-user.sqlite3",
);
export const sync = new OfflineSyncClient(ffdb, replica);

process.once("SIGTERM", () => void replica.close());` },
        ],
        paragraphs: ["IndexedDbReplica and NodeSQLiteReplica atomically persist rows, cursor movement, pending work, rejection bookkeeping, and optimistic writes. Both expose deterministic typed getRow and listRows reads. The Node adapter uses the Node 24 built-in node:sqlite module, so no native npm add-on is installed."],
        callout: { kind: "warning", title: "Authorization scope owns the cache", body: "Use a separate protected replica for each project and effective user/claims scope. Close and delete or quarantine its local data when that authorization is removed; never reveal rows cached under a previous identity." },
      },
      {
        heading: "Adapter responsibilities",
        bullets: ["Atomically replace a snapshot, replay still-pending optimistic edits, and persist its cursor.", "Apply ordered authoritative upserts and tombstone deletes.", "Atomically enqueue each pending mutation with its optimistic row insert, partial update, or delete.", "Read one row by primary key or list one table in deterministic primary-key order.", "Move rejected mutations aside with a stable error code and rejection timestamp.", "Destroy stale scoped rows when resnapshot is required."],
      },
      {
        heading: "Implement the current adapter contract",
        code: { label: "replica.ts", language: "ts", code: `import type {
  PendingMutation,
  RejectedMutation,
  ReplicaRecord,
} from "@ffdb/sync-client";
import type { JsonValue, SnapshotResponse } from "@ffdb/client";

export interface ReplicaAdapter {
  transaction<T>(
    work: (transaction: ReplicaTransaction) => Promise<T>,
  ): Promise<T>;
  getCursor(): Promise<{
    readonly cursor: string;
    readonly schemaVersion: number;
  } | null>;
  getRow(
    table: string,
    primaryKey: JsonValue,
  ): Promise<ReplicaRecord | null>;
  listRows(table: string): Promise<readonly ReplicaRecord[]>;
  getPending(limit: number): Promise<readonly PendingMutation[]>;
  getRejected(limit: number): Promise<readonly RejectedMutation[]>;
  enqueue(mutation: PendingMutation): Promise<void>;
}

export interface ReplicaTransaction {
  getRow(
    table: string,
    primaryKey: JsonValue,
  ): Promise<ReplicaRecord | null>;
  getPending(limit: number): Promise<readonly PendingMutation[]>;
  upsert(record: ReplicaRecord): Promise<void>;
  delete(
    table: string,
    primaryKey: JsonValue,
    rowVersion: number,
    serverSequence: number,
  ): Promise<void>;
  replaceSnapshot(snapshot: SnapshotResponse): Promise<void>;
  setCursor(cursor: string, schemaVersion: number): Promise<void>;
  clearCursor(): Promise<void>;
  removePending(mutationIds: readonly string[]): Promise<void>;
  rejectPending(mutationId: string, errorCode: string): Promise<void>;
}` },
        paragraphs: ["The transaction callback is the atomic boundary. A failed callback must roll back its row, cursor, pending, and rejection changes together. enqueue must persist the pending record and apply its optimistic row change in that same boundary; do not emulate either workflow with unrelated storage writes."],
      },
      {
        heading: "Queue a mutation",
        code: { label: "offline-write.ts", language: "ts", code: `import { generateId } from "@ffdb/client";

await sync.mutate({
  mutation_id: generateId("mut_"),
  table: "documents",
  primary_key: documentId,
  operation: "update",
  values: { title: nextTitle },
  base_row_version: currentRowVersion,
  client_timestamp_ms: Date.now(),
});

const visibleImmediately = await sync.getRow("documents", documentId);
const pending = await sync.getPending();
await sync.sync();` },
        paragraphs: ["The local row changes as soon as durable enqueue commits. Inserts replace local values, updates merge supplied fields, and deletes remove the visible row. client_timestamp_ms is diagnostic only; the server sequence remains authoritative, and a reused mutation id with different content is rejected."],
      },
      {
        heading: "Understand one sync run",
        bullets: [
          "With no cursor, snapshot first and atomically replace visible rows plus cursor and schema version.",
          "Push pending mutations in batches of 1–100 and consume exactly one result per mutation.",
          "Applied, duplicate, and superseded results leave the pending queue; rejected results move to the rejected queue.",
          "Duplicate, superseded, and rejected results atomically invalidate the old cursor and force an authoritative snapshot, so interrupted recovery resumes safely and an optimistic value cannot survive without a matching server change.",
          "Keep the pre-push cursor so the pull observes server-authoritative changes from accepted mutations.",
          "Pull batches of 1–1,000 until has_more is false; an invalidate_scope or resnapshot_required control replaces the scoped replica.",
          "sync() deduplicates concurrent callers. Abort signals stop HTTP work, while the durable queue remains the retry source of truth.",
        ],
      },
    ],
  },
  {
    path: "/cli",
    title: "CLI",
    description: "Manage platform credentials, projects, data, policies, storage, email, and operations with the ffdb binary.",
    group: "SDKs and tools",
    sections: [
      {
        heading: "Install the CLI package",
        paragraphs: ["Install @ffdb/cli at the exact server version in a trusted operator environment. Use the checksum-listed release tarball for verified offline installation."],
        code: { label: "Terminal", language: "sh", code: `npm view @ffdb/cli dist-tags --json
npm install --global @ffdb/cli@0.3.13
ffdb --help` },
        callout: { kind: "note", title: "Check before updating", body: "@ffdb/cli installs the ffdb binary. Use npm view @ffdb/cli version only for discovery, read the target release notes, then install the exact server-matched version." },
      },
      {
        heading: "Runtime, output, and errors",
        paragraphs: ["@ffdb/cli requires Node.js 24 or newer. Global options are parsed before the command: --url, --project, --key, --config, and --json. With --json, successful values are serialized for automation; without it, the CLI prints a human-readable projection. Unknown commands, invalid or missing arguments, missing credentials, declined destructive confirmations, file errors, and FFDB API errors exit non-zero and write a bounded message to stderr."],
        bullets: [
          `Supported environment variables: ${cliEnvironment.join(", ")}.`,
          "Credential precedence: explicit global flags, then environment variables, then the owner-only credential file.",
          "Commands marked [--yes] prompt before destructive work unless automation supplies that flag.",
          "JSON-file arguments are parsed before the request; invalid JSON fails locally without mutating the server.",
        ],
        code: { label: "Automation", language: "sh", code: `ffdb --url https://data.example.com --json health
FFDB_PASSWORD="$FFDB_PASSWORD" ffdb login admin@example.com
ffdb --json project list "$FFDB_ORGANIZATION_ID"` },
      },
      ...cliCommandSections,
      {
        heading: "Credential resolution",
        paragraphs: ["The CLI resolves explicit flags first, then environment variables, then its owner-only credential file. Platform login and project developer keys remain separate."],
        code: { label: "Terminal", language: "sh", code: `FFDB_PASSWORD="$FFDB_PASSWORD" ffdb --url https://ffdb.example.com login developer@example.com
ffdb project link "$FFDB_PROJECT_ID"
ffdb schema --json
ffdb policies --json
ffdb health` },
      },
      {
        heading: "Scaffold and generate schema types",
        code: { label: "Terminal", language: "sh", code: `ffdb init ../notes-app react
ffdb generate --out ../notes-app/src/ffdb.types.ts` },
        paragraphs: ["init accepts browser, react, or node and refuses to overwrite an existing generated file. generate reads the linked project's live /schema contract and atomically writes conservative TypeScript interfaces."],
        bullets: ["BLOB columns use BlobValue from @ffdb/client.", "Integer, real, date, and timestamp declarations map to number.", "Nullable columns include null.", "Unknown or unrecoverable SQLite declarations remain unknown."],
      },
      {
        heading: "Billing and project commerce",
        paragraphs: ["Platform billing commands take an explicit organization ID. Project commerce uses the project currently linked in the CLI configuration and exposes complete provider setup, catalog, Checkout, order, refund, subscription, entitlement, and fulfillment workflows."],
        code: { label: "Terminal", language: "sh", code: `ffdb billing status "$FFDB_ORGANIZATION_ID"
ffdb billing checkout "$FFDB_ORGANIZATION_ID" pay_as_you_go
ffdb billing checkout "$FFDB_ORGANIZATION_ID" pro
ffdb billing portal "$FFDB_ORGANIZATION_ID"
ffdb commerce status
ffdb commerce products
ffdb commerce orders
ffdb commerce subscriptions` },
        bullets: ["Platform Checkout and Portal require Stripe configured by the instance owner.", "A returned provider redirect is not proof that billing state changed; re-read billing status after verified webhook processing.", "Project commerce is configured independently per project with encrypted BYO Stripe credentials or optional Connect direct charges.", "Run ffdb commerce --help for BYO/Connect setup, prices, Checkout-adjacent administration, refunds, entitlements, cancellation, and paid fulfillment commands."],
      },
      ...cliModuleSections,
      {
        heading: "Crawler-friendly CLI reference",
        paragraphs: ["The same shipped command and programmatic-module reference is available as static Markdown at /docs/reference/cli.md. It covers the public executable syntax and every export from the @ffdb/cli package root."],
      },
      {
        heading: "Automation",
        bullets: ["Use --json for machine-readable output.", "Pass --yes only when an automation has already resolved a destructive target.", "The CLI does not print stored credentials; newly issued key secrets are returned once by the server.", "FFDB_CONFIG can choose an alternate credential file."],
      },
    ],
  },
  {
    path: "/billing/platform",
    title: "FFDB platform billing",
    description: "Read organization entitlements and use operator-configured Stripe Checkout and Customer Portal sessions in a self-hosted deployment.",
    group: "Billing and payments",
    sections: [
      {
        heading: "Status: implemented for self-hosted configuration",
        paragraphs: ["The released API implements deployment-owned, organization-scoped Free, pay-as-you-go, and Pro billing with durable reads, writes, logical-storage, storage byte-hour, and monthly-active-user metering. During /app/ first-run setup, the owner chooses private or team analytics without tenant charges, or enables a monetized instance with operator-owned Stripe credentials or Stripe Connect. BYO and Connect setup provision the plan catalog automatically; the operator owns the customer relationship, prices, invoices, and Stripe account. After any organization enters billing, FFDB locks the instance to that billing mode and Stripe account until every organization subscription is canceled and reconciled; same-account BYO key rotation remains available."],
        code: { label: "Current product status", language: "yaml", code: `self_hosted_billing_api: implemented
private_and_team: analytics_without_tenant_charges
platform_byo: operator_owned_billing
platform_connect: connected_operator_billing
plans: [free, pay_as_you_go, pro]
free_project_limit: 2
stripe_catalog: automatically_provisioned
usage_reporting: automatic_and_reconciled` },
        bullets: ["Free: $0, two projects, 1 GB storage, 1 million reads, 50,000 writes, and 5,000 MAU each month; reads continue at the limit while write, signup, and storage growth admission pauses.", "Pay as you go: the Free allowances, then $0.20 per GB-month from byte-hours, $0.25 per million reads, $1.50 per million writes through one million and $2.25 per million after, plus $0.005 per MAU through 50,000 and $0.015 after.", "Pro: $7 per month including 10 GB storage, 15 million reads, 750,000 writes, and 50,000 MAU, followed by the same provisioned usage dimensions and invoice reconciliation."],
        callout: { kind: "note", title: "Deployment-owned checkout", body: "Checkout and Customer Portal redirects belong to the Stripe account selected during instance setup. Receiving a redirect is never proof that billing state changed; only verified webhooks and reconciled usage update entitlements and invoices." },
      },
      {
        heading: "Read organization billing",
        paragraphs: ["GET /v1/organizations/:organization_id/billing and organizationBilling() return the organization's entitlement and instance enforcement policy. GET /billing/usage returns current reads, writes, storage, storage byte-hours, MAU, period bounds, and reporting health; GET /billing/invoices returns verified invoice history. Private, team, and explicitly exempt organizations are unmetered for billing while still retaining usage analytics."],
        code: { label: "TypeScript", language: "ts", code: `const billing = await ffdb.organizationBilling(organizationId);

console.log({
  tier: billing.tier,
  status: billing.status,
  projectLimit: billing.project_limit,
  providerConfigured: billing.provider_configured,
});` },
        bullets: ["tier is free, pay_as_you_go, or pro; status reports the current billing lifecycle state.", "billing_enforcement_enabled and billing_exempt explain whether allowances are billable for this organization.", "Free reads continue beyond the included amount; writes, new active users, and storage growth pause at their limits. Paid tiers report positive usage deltas through a durable outbox and reconcile all four dimensions before finalization.", "current_period_start_ms, current_period_end_ms, invoice history, and reporting_status make provider progress visible without treating redirects as payment proof."],
      },
      {
        heading: "Create Checkout and Customer Portal sessions",
        paragraphs: ["POST /v1/organizations/:organization_id/billing/checkout accepts exactly { tier: \"pay_as_you_go\" | \"pro\" }. POST /v1/organizations/:organization_id/billing/portal has no request body. Both require an authorized platform session and an Idempotency-Key header, and both return a short-lived redirect URL owned by the configured provider."],
        code: { label: "TypeScript", language: "ts", code: `const checkout = await ffdb.createBillingCheckout(
  organizationId,
  { tier: "pay_as_you_go" },
  { idempotencyKey: crypto.randomUUID() },
);

const portal = await ffdb.createBillingPortal(organizationId, {
  idempotencyKey: crypto.randomUUID(),
});` },
        bullets: ["Use a fresh, stable idempotency key for each logical Checkout or Portal request and reuse it only when retrying that same operation.", "Do not mark an organization paid after a redirect; verified Stripe events update the server-owned billing summary.", "When Stripe is not configured, Checkout and Portal fail with 503 billing.provider_unavailable while Free entitlement reads continue to work."],
      },
      {
        heading: "CLI billing commands",
        paragraphs: ["The trusted operator CLI exposes the same organization summary and configured provider redirects. Pass the organization ID explicitly and choose exactly pay_as_you_go or pro for Checkout."],
        code: { label: "Terminal", language: "sh", code: `ffdb billing status "$FFDB_ORGANIZATION_ID"
ffdb billing checkout "$FFDB_ORGANIZATION_ID" pay_as_you_go
ffdb billing checkout "$FFDB_ORGANIZATION_ID" pro
ffdb billing portal "$FFDB_ORGANIZATION_ID"
ffdb billing usage "$FFDB_ORGANIZATION_ID"
ffdb billing invoices "$FFDB_ORGANIZATION_ID"` },
      },
      {
        heading: "Raw HTTP contracts",
        code: { label: "HTTP", language: "sh", code: `GET /v1/organizations/:organization_id/billing
Authorization: Bearer <platform-session>

POST /v1/organizations/:organization_id/billing/checkout
Authorization: Bearer <platform-session>
Content-Type: application/json
Idempotency-Key: <unique-operation-id>

{"tier":"pro"}

POST /v1/organizations/:organization_id/billing/portal
Authorization: Bearer <platform-session>
Idempotency-Key: <unique-operation-id>` },
      },
      {
        heading: "Verified Stripe webhook boundary",
        paragraphs: ["POST /v1/billing/webhooks/stripe receives the raw Stripe event payload. The server verifies its Stripe-Signature against the configured webhook secret before parsing or applying the event; browsers and application backends do not call this endpoint as a billing mutation."],
        code: { label: "Provider request", language: "sh", code: `POST /v1/billing/webhooks/stripe
Stripe-Signature: <provider-signature>
Content-Type: application/json

<raw Stripe event bytes>` },
        bullets: ["Webhook event IDs are processed idempotently.", "Older provider events must not regress newer organization billing state.", "Platform billing is organization-scoped and remains separate from the project-payments capability contract."],
      },
    ],
  },
  {
    path: "/billing/project-payments",
    title: "Project commerce",
    description: "Sell products and memberships with project-owned Stripe credentials or Connect direct charges.",
    group: "Billing and payments",
    sections: [
      {
        heading: "Status: complete project commerce API",
        paragraphs: ["Project commerce is isolated from the organization subscription that pays for FFDB. Every project chooses exactly one provider mode: encrypted BYO Stripe credentials or optional Stripe Connect with direct charges. The same provider-neutral products, prices, Checkout, orders, payments, refunds, subscriptions, entitlements, and fulfillment APIs run above both modes."],
        code: { label: "TypeScript", language: "ts", code: `const account = await ffdb.commerce.account();

await ffdb.commerce.configureByo({
  secret_key: process.env.PROJECT_STRIPE_SECRET_KEY!,
  webhook_secret: process.env.PROJECT_STRIPE_WEBHOOK_SECRET!,
});

// Or use Accounts v2 Connect onboarding:
const onboarding = await ffdb.commerce.connectOnboarding({
  country: "US",
  email: "owner@example.com",
  return_url: "https://app.example.com/settings/payments/return",
  refresh_url: "https://app.example.com/settings/payments/refresh",
});` },
        callout: { kind: "warning", title: "Credentials are server-only", body: "Call account configuration from a trusted operator service or portal. FFDB encrypts BYO credentials with scope-bound authenticated encryption and never returns either secret." },
      },
      {
        heading: "Products and immutable prices",
        paragraphs: ["Products describe what the application sells. Prices snapshot currency, minor-unit amount, billing cadence, and recurring entitlement grants. Retiring a price prevents new Checkout sessions without changing historical orders or subscriptions."],
        code: { label: "TypeScript", language: "ts", code: `const product = await ffdb.commerce.createProduct({
  name: "Team Pro",
  description: "Ten seats and advanced exports",
  tax_code: null,
});

const price = await ffdb.commerce.createPrice({
  product_id: product.id,
  lookup_key: "team_pro_monthly",
  currency: "USD",
  unit_amount_minor: 1500,
  billing: { type: "recurring", interval: "month", interval_count: 1 },
  entitlements: {
    seats: { type: "quantity", value: 10 },
    exports: { type: "enabled", value: true },
  },
});` },
        bullets: ["Amounts are integer minor units and bounded to JavaScript's safe-integer range.", "Recurring entitlements are validated and keyed before the provider Price is created.", "Catalog reads are public by default; inactive catalog entries require commerce administration."],
      },
      {
        heading: "One-time and recurring Checkout",
        paragraphs: ["FFDB creates hosted Stripe Checkout Sessions. A one-time cart snapshots every order line before redirect. A recurring Checkout binds one immutable price to an individual, team, or organization membership subject. A browser redirect is navigation only; captured payment and active subscription webhooks are state authority."],
        code: { label: "TypeScript", language: "ts", code: `const checkout = await ffdb.commerce.recurringCheckout({
  price_id: price.id,
  quantity: 1,
  subject: { kind: "team", id: teamId },
  customer_email: "billing@example.com",
  success_url: "https://app.example.com/billing/success",
  cancel_url: "https://app.example.com/billing",
}, { idempotencyKey: checkoutAttemptId });

location.assign(checkout.url);` },
        callout: { kind: "note", title: "Idempotency is part of the contract", body: "Reuse one Idempotency-Key for retries of the same logical product, price, Checkout, refund, cancellation, or fulfillment mutation. Reusing it with different input is rejected." },
      },
      {
        heading: "Subscriptions, Customer Portal, and entitlements",
        paragraphs: ["Subscription webhooks apply only when their project metadata and connected-account binding match. Active or trialing periods materialize the immutable price entitlement set. Past-due, unpaid, paused, canceled, and expired states revoke or expire access according to the verified provider lifecycle. After Checkout binds a Stripe Customer to the subject, FFDB can create a subject-authorized Customer Portal session for payment-method and subscription self-service."],
        code: { label: "TypeScript", language: "ts", code: `const entitlements = await ffdb.commerce.entitlements({
  kind: "team",
  id: teamId,
});

const portal = await ffdb.commerce.customerPortal({
  subject: { kind: "team", id: teamId },
  return_url: "https://app.example.com/settings/billing",
});

await ffdb.commerce.cancelSubscription(
  subscriptionId,
  { at_period_end: true },
  { idempotencyKey: cancellationId },
);` },
      },
      {
        heading: "Refunds and paid fulfillment",
        paragraphs: ["Refund reservations are serialized against captured funds, preventing concurrent over-refunds. Provider refund webhooks reconcile final state. Physical or asynchronous fulfillment can move to processing or fulfilled only while captured funds minus successful and pending refunds still cover the full order total."],
        code: { label: "TypeScript", language: "ts", code: `const refund = await ffdb.commerce.refund({
  payment_id: paymentId,
  amount_minor: 500,
  reason: "requested_by_customer",
}, { idempotencyKey: refundAttemptId });

await ffdb.commerce.updateFulfillment(
  orderId,
  "fulfilled",
  "carrier tracking 123",
  { idempotencyKey: fulfillmentAttemptId },
);` },
      },
      {
        heading: "Webhook boundary and raw HTTP routes",
        paragraphs: ["BYO projects receive the exact raw provider body at POST /v1/projects/:project_id/commerce/webhooks/stripe. Connect uses one deployment endpoint at POST /v1/commerce/webhooks/stripe-connect: FFDB verifies its dedicated endpoint secret before parsing event.account, resolves exactly one connected project, and rechecks account and livemode. The BYO endpoint rejects Connect events. Both paths bind payload hashes to durable event IDs before applying ordered changes.", "Unused BYO or Connect configuration can be removed with commerce.disconnectAccount(). Disconnect is audited and idempotent, removes only FFDB's local binding and encrypted project secrets, never closes the external Stripe account, and fails with commerce.account_in_use after catalog, customer, order, or subscription state exists."],
        code: { label: "HTTP", language: "sh", code: `GET  /v1/projects/:project_id/commerce/account
DELETE /v1/projects/:project_id/commerce/account
POST /v1/projects/:project_id/commerce/account/byo
POST /v1/projects/:project_id/commerce/account/connect/onboarding
GET|POST /v1/projects/:project_id/commerce/products
GET|POST /v1/projects/:project_id/commerce/prices
POST /v1/projects/:project_id/commerce/checkouts/one-time
POST /v1/projects/:project_id/commerce/checkouts/recurring
POST /v1/projects/:project_id/commerce/customer-portal
GET  /v1/projects/:project_id/commerce/orders
GET  /v1/projects/:project_id/commerce/payments
POST /v1/projects/:project_id/commerce/refunds
GET  /v1/projects/:project_id/commerce/subscriptions
GET  /v1/projects/:project_id/commerce/entitlements
POST /v1/projects/:project_id/commerce/webhooks/stripe  # BYO only
POST /v1/commerce/webhooks/stripe-connect              # Connect only` },
        callout: { kind: "warning", title: "No implied payment compliance", body: "Using FFDB does not transfer tax, dispute, privacy, fulfillment, restricted-business, or merchant compliance obligations away from the project owner." },
      },
    ],
  },
  {
    path: "/host-updates",
    title: "Host updates and rollback",
    description: "Check, apply, monitor, and safely reverse signed native FFDB releases from the portal or host console.",
    group: "Operations",
    sections: [
      {
        heading: "Use the native lifecycle boundary",
        paragraphs: ["A packaged native installation includes a root-owned, path-activated updater. The portal and Axum API can request only six typed operations: inspect, check, install an exact version, roll back to an installed exact version, read a job, or update the bounded schedule. They cannot supply a URL, filesystem path, command, or shell argument. The updater accepts only canonical releases whose checksum list has a valid GitHub Actions Sigstore identity and whose requested asset is named by the release manifest."],
        bullets: ["FFDB releases live side by side under /opt/ffdb/releases, and /opt/ffdb/current changes atomically only after verification and backup succeed.", "Caddy and the compiled portal stay available while Axum and the sync worker restart, so the progress screen can reconnect to the persisted job.", "Every install and rollback takes a coordinated host backup, holds one global update lock, records an instance audit event, and requires readiness after restart.", "The API runs as ffdb and only writes a bounded request into the updater queue. It never receives root, sudo, or general process-execution authority."],
        callout: { kind: "note", title: "Docker remains host controlled", body: "Packaged Docker installations continue to use the signed ffdb-host update and rollback workflow. FFDB does not mount the Docker host or its root socket into the API container merely to offer a portal button." },
      },
      {
        heading: "Verify lifecycle services",
        paragraphs: ["The native installer enables the request watcher and periodic release check with the application services. Automatic checks are on by default; automatic application is off. Inspect the service boundary after a fresh install or upgrade before relying on portal controls."],
        code: { label: "Native host", language: "sh", code: `sudo systemctl --no-pager --full status \\
  ffdb-update-agent.path \\
  ffdb-update-check.timer
sudo -u ffdb /usr/local/bin/ffdb-update inspect
sudo journalctl -u ffdb-update-agent.service --since today --no-pager` },
      },
      {
        heading: "Check and install from the portal",
        paragraphs: ["Open Global administration, then Updates. Check for updates reads the stable channel without restarting services. Install is shown only for a newer compatible signed release. Owners and instance administrators may inspect and check; install, rollback, and schedule changes require a platform session issued within the previous 15 minutes. If the session is older, the portal asks for the account password through the normal sign-in endpoint, replaces the session, and retries the exact pending action. Passwords are never forwarded to the updater."],
        bullets: ["Review the target version, signature identity, compatibility state schema, release notes link, and backup requirement before confirming.", "The API returns a job immediately. Keep the progress view open; temporary connection failures during Axum restart are expected and are retried with a bounded backoff.", "Success requires the selected version to be active and both direct API and gateway readiness to pass. A failed health check restores the previous active release and leaves the failure record available for diagnosis.", "Use the job ID and request ID when correlating the portal state with the immutable audit log and system journal."],
      },
      {
        heading: "Configure checks and maintenance",
        paragraphs: ["The stable channel is the only production channel. Choose the check interval and, only if unattended maintenance is acceptable, explicitly enable automatic application with a UTC maintenance window. A check outside the window records availability but does not restart the host. Disabling automatic checks stops network discovery without hiding already installed versions or job history."],
        callout: { kind: "warning", title: "Automatic apply is opt in", body: "A fresh installation never applies a release automatically. Enable it only after off-host backups, monitoring, alert delivery, and a staffed rollback procedure have been tested." },
      },
      {
        heading: "Roll back a compatible installed release",
        paragraphs: ["Rollback selects an already verified release stored on the host; it never downloads an arbitrary older binary. FFDB compares the active and target state-schema metadata and the release rollback floor before enabling the action. A rollback that would cross an incompatible control-plane or durable-state boundary is rejected. Restore the pre-update backup with the documented recovery procedure instead of bypassing that guard."],
        bullets: ["A rollback creates another backup before changing the active symlink.", "API, database-worker, sync-worker, web assets, units, and gateway configuration move as one versioned set.", "The previous failed release remains installed for investigation, but it is not selected automatically again.", "If the portal cannot reconnect, inspect the persisted job and journal locally; do not repeatedly submit the same operation."],
      },
      {
        heading: "Recover from an interrupted job",
        code: { label: "Native host", language: "sh", code: `sudo -u ffdb /usr/local/bin/ffdb-update inspect
sudo -u ffdb /usr/local/bin/ffdb-update job "$JOB_ID"
sudo systemctl status ffdb-api.service ffdb-sync-worker.service ffdb-gateway.service
sudo journalctl -u ffdb-update-agent.service -u ffdb-api.service \\
  -u ffdb-sync-worker.service --since today --no-pager
curl --fail http://127.0.0.1:8080/readyz
curl --fail http://127.0.0.1:5173/readyz` },
        paragraphs: ["Jobs and the active-release pointer survive API and host restarts. If power is lost before the atomic switch, the old release stays active. If it is lost after the switch, systemd starts the selected release and the retained job records the last completed phase. Resolve the current state before submitting a new install or rollback."],
      },
    ],
  },
  {
    path: "/backups",
    title: "Backups and restore",
    description: "Create encrypted project backups, verify integrity, and restore through explicit operational workflows.",
    group: "Operations",
    sections: [
      {
        heading: "Encrypted artifacts",
        paragraphs: ["Backups are encrypted before becoming durable. A backup file begins with the FFDB backup envelope rather than a SQLite header. Store the base64-encoded backup master key separately from backup files."],
        code: { label: "Terminal", language: "sh", code: `ffdb backup create
ffdb backup list
ffdb backup integrity
ffdb backup restore "$FFDB_BACKUP_ID" --yes` },
        callout: { kind: "warning", title: "Restore is destructive", body: "Resolve the exact project and backup, verify integrity, and quiesce conflicting writes before authorizing a restore." },
      },
      {
        heading: "Complete single-host recovery archive",
        paragraphs: ["The packaged single-host controller creates a complete, coordinated recovery point directly from the installed host. It quiesces mutation-serving services, logically dumps PostgreSQL, archives project, metrics, encrypted-backup, sync, MinIO, and Mailpit volumes plus root-only configuration, writes a versioned manifest and SHA-256 checksums, publishes at mode 0600, and resumes the stack even when create fails. It never overwrites an archive."],
        code: { label: "Create and restore a packaged host", language: "sh", code: `sudo ffdb-host backup create /secure/ffdb-host-2026-08-03.tar.gz

# Restore requires the exact FFDB version, a stopped host, and explicit confirmation.
sudo ffdb-host stop
sudo ffdb-host backup restore /secure/ffdb-host-2026-08-03.tar.gz --yes` },
        callout: { kind: "warning", title: "A host archive contains secrets and customer data", body: "Encrypt and replicate the mode-0600 archive off host with an independently managed recovery key. Practice the restore on an isolated host before treating it as verified." },
      },
      {
        heading: "Validation before destructive restore",
        paragraphs: ["Restore refuses a running host and validates the complete archive before replacing anything: safe paths and regular file types, exact profile/version, all checksums with no unverified files, archived Compose configuration, PostgreSQL dump readability, and SQLite quick_check for every project and organization ledger. It restores PostgreSQL transactionally, rechecks restored SQLite volumes, restores ownership, starts the full stack, and requires compiled-gateway readiness."],
        bullets: ["Native systemd installs ffdb-backup and uses the same create/restore syntax; stop ffdb-api and ffdb-sync-worker before restore.", "Native archives include local state, PostgreSQL, configuration, and object metadata; external S3 object bytes still require a provider backup at the same recovery point.", "The external-provider Compose profile fails closed because FFDB cannot atomically copy operator-owned PostgreSQL and S3 providers."],
        code: { label: "Native Linux/systemd", language: "sh", code: `sudo ffdb-backup create /secure/ffdb-native-2026-08-03.tar.gz
sudo systemctl stop ffdb-sync-worker.service ffdb-api.service
sudo ffdb-backup restore /secure/ffdb-native-2026-08-03.tar.gz --yes` },
      },
      {
        heading: "Preserve the organization billing ledger",
        paragraphs: ["Project backup artifacts alone do not contain the per-organization usage ledger below FFDB_METRICS_ROOT. The complete host workflow captures it at a coordinated recovery point with PostgreSQL billing state. For an external-provider deployment, quiesce the API, snapshot both sides, encrypt and replicate them off host, and test the pair together. A project restore must not roll the metrics ledger backward because later successful operations remain billable history."],
        bullets: ["Packaged Compose uses the metrics-data named volume mounted at /var/lib/ffdb/metrics.", "Native systemd uses /var/lib/ffdb/metrics with mode 0700 and grants write access only to ffdb-api.", "After disaster recovery, validate each organization database with SQLite quick_check and reconcile reads, writes, storage byte-hours, and MAU before reopening billable writes."],
      },
    ],
  },
  {
    path: "/observability",
    title: "Observability",
    description: "Inspect retained project and instance performance without retaining raw SQL or parameter values.",
    group: "Operations",
    sections: [
      {
        heading: "Performance workspace",
        paragraphs: ["Open Observability in the portal for retained traffic, latency, errors, saturation, storage, route rankings, and privacy-safe query fingerprints. The current-project scope is available to organization members. The entire-instance scope and optional project filter require an instance owner or administrator."],
        bullets: ["Choose 1 hour, 6 hours, 24 hours, 7 days, or the full 30-day retention window.", "Charts use a server-selected bounded resolution and refresh every 30 seconds.", "Route tables use stable templates, so concrete project and resource IDs are excluded.", "Capacity reports live worker processes, execution slots in use, and database and backup filesystem headroom.", "A dropped-sample warning means recorder capacity was exceeded; the portal never silently presents an incomplete sample as complete."],
        code: { label: "Operator API", language: "sh", code: `# Project scope: any member of the owning organization
curl --fail \\
  -H "Authorization: Bearer $FFDB_PLATFORM_SESSION" \\
  "http://127.0.0.1:5173/v1/projects/$FFDB_PROJECT_ID/observability?range=24h"

# Instance scope: owner or administrator only
curl --fail \\
  -H "Authorization: Bearer $FFDB_PLATFORM_SESSION" \\
  "http://127.0.0.1:5173/v1/instance/observability?range=7d"` },
      },
      {
        heading: "Query privacy boundary",
        paragraphs: ["FFDB records execution duration and row counts inside the isolated database worker, but never persists raw SQL. A bounded lexer preserves keywords, operators, and structure while replacing identifiers, comments, strings, numbers, blobs, and bind parameters. The normalized shape is capped at 96 tokens and 320 characters and hashed with SHA-256 for grouping."],
        bullets: ["No raw SQL, table or column names, comments, literal values, or bound parameters enter the telemetry tables.", "Successful statements and failed executions are counted; idempotency replays are not timed twice.", "Logical database size is sampled after successful database operations, so an idle upgraded project may initially have no size sample.", "Retained minute aggregates live in control-plane PostgreSQL and are deleted after 30 days."],
        callout: { kind: "note", title: "Normalized shapes are still operational metadata", body: "Keep project and instance observability behind platform authentication and include PostgreSQL telemetry tables in your normal retention and recovery policy." },
      },
      {
        heading: "Prometheus and request correlation",
        paragraphs: ["The retained API powers the portal. The separate /metrics endpoint remains an instance-wide, current-process Prometheus scrape for external alerting. Every inbound API request also receives X-Request-Id for correlation with structured logs and immutable audit events."],
        bullets: ["ffdb_http_requests_total is labeled by method, stable route, and status class.", "ffdb_http_request_duration_seconds is labeled by method and stable route.", "ffdb_http_requests_inflight, authentication failures, and rate-limit denials remain available.", "Prometheus labels never include project, user, request, object key, SQL, or token IDs."],
        code: { label: "Packaged gateway checks", language: "sh", code: `curl --fail http://127.0.0.1:5173/healthz
curl --fail http://127.0.0.1:5173/readyz
curl --fail http://127.0.0.1:5173/metrics
ffdb logs 100
ffdb health` },
        callout: { kind: "warning", title: "Keep the private API private", body: "The compiled nginx gateway proxies these paths to Axum inside the private Compose network. Packaged Docker profiles never publish Axum directly on host port 8080." },
      },
    ],
  },
  {
    path: "/security",
    title: "Production security",
    description: "Preserve FFDB's trust boundaries when deploying it behind your own network and storage providers.",
    group: "Operations",
    sections: [
      {
        heading: "Deployment checklist",
        bullets: ["Terminate TLS and configure an exact trusted proxy boundary.", "Keep PostgreSQL, worker IPC, project files, and backup volumes off public networks.", "Use HTTPS allowlisted S3 endpoints and keep the internal endpoint distinct from the browser-visible endpoint.", "Rotate platform, project, JWT, storage-grant, cursor, and backup secrets independently.", "Review the threat model and run the release checklist before changing a trust-boundary crate."],
        callout: { kind: "warning", title: "No certification claim", body: "FFDB implements security controls at the architecture level but does not claim formal certifications. Your deployment and operating practices remain part of the security boundary." },
      },
      {
        heading: "Verify the exposed boundary",
        paragraphs: ["Run these checks from the host and repeat the HTTPS checks from an external operator network. Expected results are a loopback-only gateway, no public PostgreSQL or Axum listener, healthy dependencies, and restrictive ownership on configuration and durable data."],
        code: { label: "Linux host", language: "sh", code: `docker compose ps
ss -lntp | grep -E ':(5173|8080|5432|9000|9001|8025|1025)\\b'
curl --fail --include http://127.0.0.1:5173/readyz
curl --fail --include http://127.0.0.1:5173/openapi.json >/dev/null

# Managed installation configuration must be operator-readable only.
sudo stat -c '%a %U:%G %n' /etc/ffdb/ffdb.env
sudo ffdb-host verify` },
        callout: { kind: "note", title: "Expected listener", body: "In packaged Docker deployments, port 5173 is the loopback nginx gateway. Axum port 8080 and PostgreSQL 5432 stay on the private Compose network. The evaluation-only MinIO and Mailpit ports are also bound to loopback." },
      },
    ],
  },
  {
    path: "/reference/client",
    title: "Client API reference",
    description: "The complete @ffdb/client class, method, function, interface, return-type, runtime, and error reference.",
    group: "Reference",
    sections: [
      {
        heading: "Environment and error contract",
        paragraphs: ["@ffdb/client supports Node.js 24+, modern browsers, React Native, and other runtimes that provide standards-compatible fetch, URL, Headers, AbortController, and cryptographic randomness. BrowserSessionStore and BrowserDeveloperSessionStore additionally require Web Storage; use memory or application-supplied stores elsewhere."],
        bullets: [
          "Every HTTP failure rejects with FFDBError: message, code, status, requestId, and bounded details.",
          "AbortSignal cancellation rejects with AbortError. DNS, TLS, and fetch failures retain the runtime error.",
          "Safe GET/idempotent work may retry within the documented bound; unkeyed mutations are not silently replayed.",
          "RequestOptions supplies optional signal, idempotencyKey, and retry controls to public network methods.",
        ],
        code: { label: "Error handling", language: "ts", code: `import { FFDBError } from "@ffdb/client";

try {
  await ffdb.schema();
} catch (error) {
  if (error instanceof FFDBError) {
    console.error(error.code, error.status, error.requestId);
  }
}` },
      },
      {
        heading: "FFDBClient",
        bullets: ["query(request, options) and transaction(request, options)", "migrate(spec), rollbackMigration(id), schema(), and policies()", "organizations(), projects(), API keys, auth settings, logs, and backup methods", "health(), readiness(), and metrics()", "setProjectId(id) and setDeveloperKey(key) for trusted tooling"],
      },
      {
        heading: "Subclients",
        bullets: ["client.auth: registration, verification, sign-in/out, refresh, password reset, sessions", "client.storage: buckets, list, upload, download URL, multipart, cleanup", "client.sync: snapshot, pull, push"],
      },
      {
        heading: "Class and module examples",
        paragraphs: ["Construct FFDBClient once per application boundary, then use its typed auth, storage, sync, commerce, and platform-management surfaces. The examples below use only package-root exports and public class properties."],
        code: { label: "Public client surfaces", language: "ts", code: `const signedIn = await ffdb.auth.signIn(email, password);
const buckets = await ffdb.storage.buckets();
const changes = await ffdb.sync.pull(null, 100);
const commerce = await ffdb.commerce.account();
const instance = await ffdb.instanceSetupStatus();

console.log(signedIn.user.id, buckets.length, changes.has_more);
console.log(commerce.status, instance.setup_required);` },
      },
      ...clientClassSections,
      {
        heading: "Exported functions",
        paragraphs: ["Functions below are exported from the @ffdb/client package root; their signatures match the published package declarations."],
        bullets: clientFunctionSignatures,
        code: { label: "ID generation", language: "ts", code: `import { generateId } from "@ffdb/client";

const documentId = generateId("doc_");` },
      },
      ...clientTypeSections,
      {
        heading: "Crawler-friendly client reference",
        paragraphs: ["The complete published declarations are also available as static Markdown at /docs/reference/client.md, including every exported class, public method, function, interface, and type alias."],
      },
    ],
  },
  {
    path: "/reference/errors",
    title: "Error envelopes",
    description: "Handle stable error codes and request IDs without depending on internal provider or SQLite messages.",
    group: "Reference",
    sections: [
      {
        heading: "FFDBError",
        code: { label: "errors.ts", language: "ts", code: `import { FFDBError } from "@ffdb/client";

try {
  await ffdb.query(request);
} catch (error) {
  if (error instanceof FFDBError) {
    console.error(error.code, error.status, error.requestId);
  }
}` },
        bullets: ["code is the stable machine-readable identifier.", "status is the HTTP status when the request reached the API.", "requestId correlates safe operator logs.", "details contains bounded structured values safe for that public error."],
      },
    ],
  },
  {
    path: "/reference/http-api",
    title: "HTTP API",
    description: "Use the OpenAPI contract as the source of truth for routes, schemas, and stable error envelopes.",
    group: "Reference",
    sections: [
      {
        heading: "OpenAPI contract",
        paragraphs: ["A running deployment serves its current OpenAPI document at /openapi.json through the same compiled nginx gateway used by applications. In packaged Docker releases that gateway is the only host-published FFDB ingress and it proxies the request to Axum on the private Compose network. Prefer @ffdb/client for TypeScript applications because it normalizes tagged worker envelopes, token refresh, retries, storage provider calls, and lossless SQL values."],
        code: { label: "Packaged gateway", language: "sh", code: `curl --fail http://127.0.0.1:5173/openapi.json > ffdb-openapi.json` },
        callout: { kind: "note", title: "Request IDs", body: "Every API response includes X-Request-Id. Preserve it when reporting failures, but never include credentials or signed provider URLs." },
      },
      {
        heading: "Direct HTTP example",
        paragraphs: ["Use the exact deployed origin and a route-appropriate credential. Preserve the response request ID, send parameters as typed JSON rather than interpolating SQL text, and bound result rows."],
        code: { label: "Parameterized query", language: "sh", code: `curl --fail-with-body \
  --request POST \
  --header "Authorization: Bearer $FFDB_USER_ACCESS_TOKEN" \
  --header "Content-Type: application/json" \
  --data '{"sql":"select id, title from documents where id = ?1","parameters":[{"type":"text","value":"doc_123"}],"options":{"max_rows":1}}' \
  "$FFDB_URL/v1/projects/$FFDB_PROJECT_ID/query"` },
      },
      {
        heading: "Arguments, returns, and errors",
        paragraphs: ["Each generated operation entry below combines path-level and operation-level parameters, marks every value required or optional, names the credential scheme, summarizes the JSON request body, lists success and error responses, and identifies routes that require Idempotency-Key. The deployed /openapi.json remains authoritative for complete JSON Schema."],
        bullets: [
          "developerBearer is an opaque platform session for instance and organization administration.",
          "projectBearer accepts the route-appropriate project developer key or end-user JWT.",
          "userBearer is a project end-user access token evaluated with project identity and RLS claims.",
          "Errors use the stable code/message/request_id/details envelope and include X-Request-Id.",
        ],
      },
      ...httpOperationSections,
      {
        heading: "Machine-readable and static references",
        paragraphs: ["Use /docs/openapi.json for the versioned OpenAPI 3.1 document and /docs/reference/http-api.md for crawler-friendly operation summaries. On an installed FFDB host, /openapi.json serves the exact deployed contract through the compiled gateway."],
      },
    ],
  },
] as const;

type RoutePath = (typeof routePages)[number]["path"];

const pageGuides: Record<RoutePath, PageGuide> = {
  "/": {
    what: "This documentation maps the released FFDB server, SDK packages, security boundaries, and operator workflows.",
    why: "Starting here prevents a deployment choice, credential type, or local-replica design from being mistaken for a supported product guarantee.",
    when: "Use it before installing FFDB or when deciding which guide owns the next task.",
    prerequisites: ["A workload that needs self-hosted auth, scoped SQL, storage, or offline sync.", "An operator who can own PostgreSQL, S3-compatible storage, email, TLS, and backups."],
    requiredValues: ["The intended installation shape: packaged Docker release or packaged native components.", "The application runtime: browser, React, React Native, Node, or direct HTTP."],
    steps: ["Choose the packaged installation route and bring the service to ready.", "Create an organization and project, then separate platform credentials from end-user sessions.", "Add the matching SDK packages and issue one parameterized, RLS-scoped query.", "Finish with backup, observability, and production-security checks."],
    result: "You can name the supported installation, request path, credential boundary, and next task from the public release contract.",
    failures: ["Server and package versions differ — select one announced release version and pin every component to it.", "Billing behavior is unclear — check the instance mode in Global admin: private/team retain analytics without charges, while BYO/Connect enable the operator-owned Free, PAYG, and Pro contract."],
    nextSteps: ["Follow Quickstart for the shortest packaged path.", "Compare installation shapes in Deployment overview."],
  },
  "/quickstart": {
    what: "Quickstart takes the packaged, zero-source single-host evaluation profile from reviewed installer to one authenticated application query.",
    why: "It proves the complete product with digest-pinned PostgreSQL, MinIO, Mailpit, FFDB services, and gateway before you invest in production providers, schema, policies, uploads, or offline behavior.",
    when: "Use it for a local evaluation, demo, or isolated clean host; single-host is not the internet-production topology.",
    prerequisites: ["A supported host with Docker Compose and curl, plus permission to create durable volumes.", "Access to the official release channel or an operator-supplied verified bundle."],
    requiredValues: ["Release installer URL, published checksum/signature material, and the explicit single-host profile selection.", "Loopback FFDB origin, owner email/password, root-only generated bootstrap token, organization slug, and project name."],
    steps: ["Download ffdb-install.sh to a file, inspect it, and run it with --profile single-host --start --require-signature.", "Verify sudo ffdb-host status and the loopback gateway readiness endpoint before creating credentials.", "Extract the generated bootstrap token into a root-only file without printing it, open /app/, create the first owner, and choose private, team, Stripe BYO, or Stripe Connect.", "For Connect, complete onboarding and refresh so FFDB provisions and verifies the plan catalog, then enter Global admin and create an organization and project; use the terminal API/CLI only when an auditable headless path is preferred.", "Install the client SDK package, sign in an end user, and run the sample query."],
    result: "The preserved-volume evaluation stack is ready on loopback, the project exists, an end-user session is stored, and the query returns only rows visible to that user.",
    failures: ["Checksum or signature verification fails — delete the artifact and stop; do not install it.", "Readiness fails — inspect packaged service status and logs before creating credentials.", "A single-host port is exposed beyond loopback — stop the stack and restore the packaged bindings; do not use this profile on the internet.", "The query returns unauthorized rows — stop the evaluation and treat it as a security incident."],
    nextSteps: ["Define schema and row-level policies.", "Move internet production to the external-provider Docker profile or packaged systemd installation."],
  },
  "/install/docker": {
    what: "This guide starts a complete FFDB single-host evaluation directly from a copyable Docker Compose file and a protected environment file.",
    why: "It gives Docker users the actual service definition, required values, health checks, volumes, and lifecycle commands instead of making a local source tree or installer script the only visible path.",
    when: "Use the direct Compose path for local evaluation or a trusted private network. Use the signed external-provider release profile before admitting internet traffic or production data.",
    prerequisites: ["Docker Engine 27 or newer with Compose v2, OpenSSL, sufficient host resources, and the documented loopback ports.", "For later production: operator-managed PostgreSQL, S3-compatible storage, email delivery, DNS, TLS, and off-host backups."],
    requiredValues: ["Independent PostgreSQL, MinIO, encryption, backup, HMAC, and bootstrap secrets plus a unique node UUID.", "The exact local public origin, database URL, object-storage origins, and release-matched FFDB image version."],
    steps: ["Save the complete compose.yaml and .env examples in an empty directory.", "Generate every secret independently, copy matching provider passwords into their FFDB values, and restrict .env to the operator.", "Run docker compose config, pull, and docker compose up --detach --wait.", "Verify health, readiness, OpenAPI, and service status through the compiled loopback gateway.", "Open /app/, create the first owner, choose the instance type, finish onboarding, and create the first organization and project."],
    result: "The complete seven-volume stack reports ready on loopback, the first owner can finish instance setup, and no source repository or developer toolchain is involved.",
    failures: ["Compose rejects a missing value — replace the named placeholder rather than removing the check.", "A container is unhealthy — inspect docker compose ps and bounded logs, then correct the specific dependency or matching credential.", "Organization creation returns instance.setup_required — finish the owner setup wizard.", "The single-host stack is being exposed to the internet — stop and move to the external-provider production profile."],
    nextSteps: ["Complete the Quickstart application query.", "Move production dependencies off host and schedule coordinated encrypted backups and alerts."],
  },
  "/install/systemd": {
    what: "This guide installs packaged FFDB server components as hardened systemd services with one Caddy HTTPS and static-asset gateway.",
    why: "It supports operators who need native service supervision while preserving the tested binary protocol and filesystem boundaries.",
    when: "Choose it when PostgreSQL, S3, Resend, Caddy, secrets, DNS, and Linux service management are operator-owned.",
    prerequisites: ["A current Linux distribution with systemd, Caddy, curl, rsync, and root access for reviewed installation steps.", "Public DNS for the configured HTTPS origin, durable local or block storage, and an independently protected backup location."],
    requiredValues: ["Matching API, database-worker, sync-worker, and web assets from one release bundle.", "The complete production environment values plus stable ffdb user and directory ownership."],
    steps: ["Verify the release artifacts and install the packaged binaries without rebuilding them.", "Install the supplied sysusers, tmpfiles, environment template, and service units.", "Publish the packaged web assets and review the supplied single-process Caddy gateway.", "Start the API and maintenance worker, then let Caddy obtain the public certificate.", "Verify loopback and public TLS routes, logs, file permissions, and systemd hardening."],
    result: "The API, maintenance worker, and gateway run as the ffdb account, only approved state directories are writable, and Caddy serves both public HTTPS and loopback acceptance traffic for a ready API.",
    failures: ["API and database-worker versions differ — stop and reinstall the coordinated release.", "The service cannot write state — compare ownership and ReadWritePaths with the packaged declarations.", "SIGINT drain exceeds the timeout — inspect active work before increasing a measured limit."],
    nextSteps: ["Run the production acceptance checks.", "Document the coordinated binary and web-asset upgrade procedure."],
  },
  "/self-hosting": {
    what: "Deployment overview compares the supported packaged Docker and packaged native installation shapes.",
    why: "Choosing the topology first avoids confusing the packaged development-mode single-host evaluation profile with the external-provider or native production shapes and unproven multi-node storage.",
    when: "Use it during architecture review, capacity planning, or before changing an existing installation shape.",
    prerequisites: ["An inventory of provider, storage, networking, backup, and operator capabilities.", "A decision among single-host evaluation, external-provider Docker production, native systemd production, or future fenced multi-node placement."],
    requiredValues: ["Expected active projects, request rate, storage growth, recovery objectives, and public origins.", "Ownership for PostgreSQL, S3, email, TLS, secrets, monitoring, and incident response."],
    steps: ["Compare single-host evaluation, external-provider Docker production, and native systemd production against the team's operating model.", "Map every durable state location and its backup owner.", "Confirm that each API node owns a disjoint project database set before scaling.", "Record the chosen path and acceptance gates."],
    result: "One supported topology is selected with explicit durable-state, ingress, upgrade, and recovery ownership.",
    failures: ["The design mounts one project database into multiple unfenced writers — redesign placement before deployment.", "The only backup is a copied live SQLite file — use the encrypted FFDB backup workflow instead."],
    nextSteps: ["Open the selected install guide.", "Complete Configuration and Production security."],
  },
  "/configuration": {
    what: "Configuration defines the server, provider, secret, and client values that form FFDB trust boundaries.",
    why: "Several similar-looking URLs and keys have intentionally different exposure and rotation rules.",
    when: "Use it before first start, when adding a node, or during a reviewed rotation or provider migration.",
    prerequisites: ["Provisioned PostgreSQL, S3-compatible storage, email delivery, DNS, and TLS.", "A secret manager or a protected release-environment file workflow."],
    requiredValues: ["Public and internal endpoints, allowed origins, database URL, node ID/name, filesystem roots, worker limits, independent master/backup/cursor/bootstrap keys, S3 credentials, and email sender.", "For clients: base URL, project ID, session store, and trusted-only developer key when applicable."],
    steps: ["Generate independent secrets and record their rotation owners.", "Fill every required packaged template value without changing reserved paths.", "Run production configuration validation.", "Start the service and verify readiness plus provider access.", "Construct clients with only runtime-appropriate values."],
    result: "The server starts without placeholders, clients contain no operator secrets, and each provider is reached through its intended boundary.",
    failures: ["Production validation reports a weak or missing value — replace it; never downgrade the environment mode.", "Browser assets contain a developer key — revoke it and rebuild from a clean configuration."],
    nextSteps: ["Install the selected server package.", "Review key rotation and production security."],
  },
  "/database": {
    what: "Database architecture explains how PostgreSQL control-plane state and per-project SQLite application state are separated.",
    why: "The separation determines backup scope, scaling, query authorization, and which service may access each file.",
    when: "Use it before schema design, deployment topology changes, or incident investigation.",
    prerequisites: ["Basic familiarity with PostgreSQL and SQLite.", "A project whose application data and platform metadata can be classified."],
    requiredValues: ["Project identity, node/route generation, database root, backup root, metrics root, and worker executable boundary.", "Ownership of control-plane, project-data, and organization-metrics backups."],
    steps: ["Classify each datum as platform metadata, project application data, object bytes, or maintenance state.", "Trace an HTTPS request through API verification and the isolated worker.", "Confirm no application component receives a raw database path or PostgreSQL credential.", "Assign backup and restore procedures to both data planes."],
    result: "Every datum has one authoritative store and every query crosses the documented verification and worker boundaries.",
    failures: ["Application code opens a project SQLite file directly — remove that path and use the API.", "Project rows appear in PostgreSQL — revisit the data classification."],
    nextSteps: ["Design parameterized queries.", "Define migrations and row-level policies."],
  },
  "/queries": {
    what: "Queries and transactions show how to send bounded, parameterized SQL and decode ordered tagged values.",
    why: "Parameters preserve types and prevent SQL text from becoming an authorization or injection boundary.",
    when: "Use it for application reads/writes after a project schema and identity policy exist.",
    prerequisites: ["A ready project, an authenticated end-user session or trusted developer credential, and existing tables.", "Knowledge of the documented SQLite SQL subset."],
    requiredValues: ["SQL text, tagged parameters, optional max_rows, project ID, and AbortSignal when cancellation matters.", "For transactions, an ordered statement list and an idempotency strategy."],
    steps: ["Write SQL with numbered placeholders instead of interpolated values.", "Encode each parameter with the matching tagged type.", "Set an explicit row bound for reads.", "Issue the query or transaction and decode rows by the returned column order.", "Handle FFDBError by stable code and request ID."],
    result: "The server returns ordered columns and rows, and RLS constrains the result without client-authored owner predicates.",
    failures: ["A statement includes interpolated user text — replace it with a tagged parameter.", "The server rejects unsupported SQL — use SQL support to redesign rather than bypassing the parser."],
    nextSteps: ["Generate schema types with the CLI.", "Add RLS tests for two users."],
  },
  "/migrations": {
    what: "Migrations apply versioned project schema and policy changes through the trusted administration surface.",
    why: "Stable IDs, checksums, and explicit ordering make schema state auditable and prevent accidental drift.",
    when: "Use them for every durable application-schema change, including policy DDL.",
    prerequisites: ["A linked project and trusted developer credential.", "A reviewed migration with a unique stable ID and tested forward/rollback behavior."],
    requiredValues: ["Migration ID, ordered SQL statements, expected checksum, and target project.", "A backup and maintenance decision for destructive or long-running changes."],
    steps: ["Create the migration file and keep statements deterministic.", "Test it against representative schema and RLS fixtures.", "Apply it with the packaged CLI.", "Inspect migration history and live schema.", "Run application and isolation checks before promotion."],
    result: "The migration appears once with its stable checksum and the live schema matches the reviewed change.",
    failures: ["An ID exists with different SQL — create a new migration instead of rewriting history.", "A protected-table change is rejected — split or redesign it within current SQL support."],
    nextSteps: ["Regenerate SDK types.", "Review policy behavior and rollback readiness."],
  },
  "/row-level-security": {
    what: "Row-level security defines PostgreSQL-style policy DDL that FFDB compiles into protected SQLite enforcement.",
    why: "Policies keep authorization at the backend boundary instead of relying on every client to remember filters.",
    when: "Use it before exposing a table to end-user queries and whenever roles or claim semantics change.",
    prerequisites: ["A migrated table with a stable primary key.", "Documented subject, role, and claim rules for select and mutation paths."],
    requiredValues: ["Policy name, table, command, role set, USING expression, and WITH CHECK expression where required.", "Test identities representing allowed, denied, anonymous, and changed-scope cases."],
    steps: ["Write the narrowest policy expression using verified auth functions.", "Apply policy DDL through a migration.", "Test select, insert, update, and delete as multiple users.", "Inspect compiled policy metadata.", "Repeat tests after schema or claim changes."],
    result: "Authorized rows remain usable while disallowed rows are filtered or rejected consistently across reads, writes, storage, and sync.",
    failures: ["A policy form is unsupported — FFDB fails closed; rewrite it to the documented subset.", "A client adds an owner filter to compensate — fix the server policy and keep client filters about product behavior only."],
    nextSteps: ["Review JWT claims.", "Run query and sync isolation tests."],
  },
  "/sql-support": {
    what: "SQL support states the exact application and protected-table SQL boundary.",
    why: "FFDB intentionally supports a constrained subset so parsing, authorization, limits, and RLS rewriting remain enforceable.",
    when: "Check it before adopting a SQLite feature, query builder output, trigger, or migration pattern.",
    prerequisites: ["The final SQL emitted by the application or migration tool.", "Knowledge of whether the target table is RLS protected."],
    requiredValues: ["Statement class, target objects, parameter count, and expected result bound.", "Any required SQLite feature such as STRICT or RETURNING."],
    steps: ["Classify the statement as application SQL, migration DDL, or operator work.", "Compare its constructs with the supported matrix.", "Test parsing and authorization in a non-production project.", "Measure row and resource bounds.", "Fail the release if unsupported syntax is security-significant."],
    result: "The chosen SQL either executes through the documented boundary or is rejected before reaching production.",
    failures: ["A library emits hidden unsupported syntax — configure or replace its dialect output.", "A proposal needs raw PRAGMA, ATTACH, or extension loading — redesign; those are not application escape hatches."],
    nextSteps: ["Implement the query with tagged parameters.", "Add a migration fixture for accepted and rejected forms."],
  },
  "/authentication": {
    what: "Authentication covers end-user account/session flows and their separation from platform and developer credentials.",
    why: "Mixing credential classes can expose administrative authority to browsers or break RLS scope.",
    when: "Use it when implementing registration, sign-in, refresh, password reset, verification, sessions, or sign-out.",
    prerequisites: ["A project with auth settings and email delivery configured.", "A runtime-appropriate SessionStore and an exact public origin."],
    requiredValues: ["User email/password or one-time token, project ID, client base URL, and session-storage key.", "For administration only: platform login or scoped developer key kept outside client bundles."],
    steps: ["Configure project auth and email templates.", "Construct the client with a session store.", "Register or sign in and observe the returned user/session.", "Let the client rotate short-lived access tokens.", "Sign out and verify local plus server session invalidation."],
    result: "The client exposes the correct authenticated user and subsequent queries run under that immutable verified scope.",
    failures: ["Refresh is rejected — clear the invalid session and require sign-in.", "Email never arrives — inspect the outbox/provider status with the request ID, without exposing tokens."],
    nextSteps: ["Define JWT-claim policies.", "Test session revocation and multi-user RLS."],
  },
  "/jwt-claims": {
    what: "JWT claims documents the immutable user context available to policies.",
    why: "Policies must read server-verified identity and claims rather than caller-supplied SQL values.",
    when: "Use it when a policy depends on subject, role, token, session, email, or custom claims.",
    prerequisites: ["An authenticated project user and a documented claim schema.", "A policy migration and tests for missing, malformed, and changed claims."],
    requiredValues: ["Subject/user ID, role, email, session ID, token ID, and bounded custom-claim keys.", "Expected behavior when a claim is absent or null."],
    steps: ["Define the smallest stable custom-claim schema.", "Reference claims through documented auth functions in policy DDL.", "Issue or refresh a session containing the expected server-owned claims.", "Test allowed and denied rows.", "Invalidate cached scope when claims change."],
    result: "Policies produce deterministic access from verified claims and clients cannot expand scope by changing SQL parameters.",
    failures: ["A policy trusts a request parameter as identity — replace it with auth context.", "A changed claim leaves stale offline rows visible — invalidate and resnapshot the scoped replica."],
    nextSteps: ["Apply row-level policies.", "Review offline cache scope."],
  },
  "/storage": {
    what: "Object storage explains RLS-authorized metadata plus short-lived provider operations for file bytes.",
    why: "Separating authoritative metadata from provider bytes keeps authorization and quota checks inside the project boundary.",
    when: "Use it for buckets, uploads, downloads, listings, versions, deletes, or cleanup.",
    prerequisites: ["Configured internal/public S3 endpoints, bucket, credentials, and browser CORS.", "A project table/policy model that defines who may access each object."],
    requiredValues: ["Bucket name, object key, content type, exact size, checksum where required, and project session.", "For browser operations, the exact public S3 origin allowed by CSP and CORS."],
    steps: ["Create or select a logical bucket.", "Request an upload authorization with declared metadata.", "Send bytes directly to the returned provider URL without logging it.", "Commit or verify the upload through FFDB.", "Request a fresh download URL only after RLS authorization."],
    result: "Object bytes exist in S3 while FFDB metadata, quota, versions, and visibility remain authoritative and RLS constrained.",
    failures: ["Provider upload succeeds but commit fails — preserve the request ID and use bounded cleanup/retry.", "Browser upload is blocked — align exact S3 CORS and gateway CSP origins."],
    nextSteps: ["Use multipart uploads for large objects.", "Add storage lifecycle and quota monitoring."],
  },
  "/multipart-uploads": {
    what: "Multipart uploads coordinate large object parts, reservations, completion, and cleanup.",
    why: "The lifecycle binds every provider part to one authorized logical upload and verifies final bytes before quota is consumed.",
    when: "Use it when objects exceed the chosen single-upload threshold or need resumable part transfer.",
    prerequisites: ["A configured storage bucket and an authenticated user authorized by project RLS.", "A client capable of slicing bytes and retaining returned part metadata."],
    requiredValues: ["Object key, total size, content type, upload ID, unique part numbers 1–10,000, part sizes, and completion list.", "A retry policy that preserves upload and part identity."],
    steps: ["Initiate the logical multipart upload with final metadata.", "Upload each numbered part through its short-lived authorization.", "Record the returned part result exactly once.", "Complete with the ordered unique part list and final metadata.", "Abort or clean up abandoned uploads."],
    result: "FFDB verifies committed bytes and checksum, finalizes metadata, and consumes the correct quota once.",
    failures: ["A part URL expires — request a new authorization for the same logical part.", "Completion reports a size/checksum mismatch — do not retry with altered metadata; reconcile parts or abort."],
    nextSteps: ["Test interrupted upload recovery.", "Monitor cleanup of expired reservations."],
  },
  "/sync": {
    what: "Sync protocol documents RLS-filtered snapshots, mutation pushes, logical pulls, and scope controls.",
    why: "The protocol enables offline replicas without copying SQLite WAL frames or weakening server authorization.",
    when: "Use the low-level API when the application owns a durable transactional replica and retry queue.",
    prerequisites: ["An authenticated project session, stable primary keys, and sync-compatible schema.", "A replica store that can atomically replace rows, cursor, and pending state."],
    requiredValues: ["Opaque cursor, schema version, mutation ID, table, primary key, operation, values, base row version, and batch limits.", "A stable cache key for the verified auth scope."],
    steps: ["Fetch a snapshot when no valid cursor exists.", "Queue local mutations with unique IDs.", "Push bounded batches and consume every per-item result.", "Pull changes from the pre-push cursor until has_more is false.", "Replace the scoped replica on invalidate_scope or resnapshot_required."],
    result: "The replica converges on server-authoritative logical rows and retains rejected mutations for user-visible resolution.",
    failures: ["A cursor is rejected or invalidated — discard the affected scoped rows and atomically resnapshot.", "A mutation ID is reused with different content — reject the local operation and generate a new stable ID."],
    nextSteps: ["Use OfflineSyncClient for maintained orchestration.", "Review conflict behavior and cache-scope rules."],
  },
  "/offline": {
    what: "Offline replicas connect the sync protocol to durable runtime-specific local storage.",
    why: "A correct adapter preserves atomic rows, cursor, pending mutations, and rejections across interruption and restart.",
    when: "Use it when users must read or queue writes without continuous network access.",
    prerequisites: ["A supported sync schema and authenticated user scope.", "A durable transactional storage engine for production; MemoryReplica is test-only."],
    requiredValues: ["Replica instance, push/pull batch sizes, lifecycle/network triggers, retry policy, and scope-derived storage key.", "A UI path for pending, rejected, error, and last-synced state."],
    steps: ["Construct OfflineSyncClient with the FFDB client and replica.", "Subscribe the UI to sync state.", "Enqueue mutations before reporting them durable.", "Trigger sync on explicit user action and reasonable connectivity/lifecycle hints.", "Destroy or replace cached scope when authorization changes."],
    result: "Rows, cursor, and queued work survive the expected runtime lifecycle and converge after connectivity returns.",
    failures: ["The adapter commits rows and cursor separately — fix the transaction boundary before production use.", "A background retry loop drains battery or floods requests — add bounded backoff and lifecycle gates."],
    nextSteps: ["Use IndexedDbReplica in browsers or NodeSQLiteReplica in Node 24+.", "Use the React Native adapter where applicable."],
  },
  "/conflicts": {
    what: "Conflict behavior explains deterministic last-write-wins ordering, tombstones, receipts, and rejected mutations.",
    why: "Users need predictable outcomes when disconnected writers update or delete the same row.",
    when: "Use it while designing offline UX, reconciliation, retry behavior, or retention policy.",
    prerequisites: ["Stable mutation IDs, base row versions, and user-visible rejected-operation handling.", "A replica that applies server sequence monotonically."],
    requiredValues: ["Server sequence, row version, operation, mutation receipt status, tombstone retention, and cursor horizon.", "Product rules for showing superseded or rejected edits."],
    steps: ["Capture a row version before editing.", "Queue the mutation with diagnostic client time.", "Push and record the server result.", "Pull the authoritative logical change.", "Show rejected or superseded outcomes without silently resurrecting data."],
    result: "Concurrent updates resolve by server commit sequence and deletes remain protected from stale resurrection.",
    failures: ["Client time changes ordering — remove that logic; timestamps are diagnostic only.", "A tombstone disappears before lagging cursors advance — extend retention and force safe resnapshot."],
    nextSteps: ["Design rejected-mutation UX.", "Test update/delete and delete/recreate races."],
  },
  "/client": {
    what: "The TypeScript client is the supported HTTP SDK package for auth, data, storage, sync, and administration.",
    why: "It preserves tagged values, session rotation, stable errors, cancellation, and request conventions across runtimes.",
    when: "Use it in browser, Node, React, or React Native code that talks directly to FFDB.",
    prerequisites: ["A ready FFDB origin and project ID.", "The verified @ffdb/client SDK package from the release channel or private package registry."],
    requiredValues: ["baseUrl, projectId, runtime session store, and optional fetch implementation.", "developerKey only for trusted server/operator tools; never for browser bundles."],
    steps: ["Install the version-matched SDK package.", "Construct FFDBClient with runtime-safe configuration.", "Sign in an end user or configure trusted administration.", "Issue a parameterized request with an optional AbortSignal.", "Handle FFDBError by code, status, and requestId."],
    result: "The application communicates through the public API with typed request/response contracts and no raw database credential.",
    failures: ["The SDK version targets a different server contract — install the package set shipped for that release.", "A developer key appears in built assets — revoke it immediately and remove it from client configuration."],
    nextSteps: ["Add React providers or native stores.", "Generate project schema types with the CLI."],
  },
  "/react": {
    what: "The React SDK package provides FFDB context, auth state, query lifecycle, sync state, and storage-upload hooks.",
    why: "Shared providers centralize client identity and prevent components from creating inconsistent session or request state.",
    when: "Use it in React applications after constructing one @ffdb/client instance.",
    prerequisites: ["Version-matched @ffdb/client and @ffdb/react SDK packages.", "A stable FFDBClient instance and an application error/loading strategy."],
    requiredValues: ["client prop, provider order, query request, dependency list, and optional OfflineSyncClient.", "Accessible UI states for loading, anonymous, authenticated, empty, and error results."],
    steps: ["Place FFDBProvider around the application.", "Nest AuthProvider where session state is needed.", "Call hooks only below their providers.", "Render status before data and cancel superseded work.", "Test unmount, refetch, sign-out, and error recovery."],
    result: "Components observe one consistent client/session and do not commit stale request state after unmount or dependency changes.",
    failures: ["A hook reports a missing provider — correct the provider boundary instead of constructing a hidden client.", "A query loops — stabilize request inputs and dependency values."],
    nextSteps: ["Add RLS-aware application screens.", "Connect useSync for offline state if needed."],
  },
  "/react-native": {
    what: "The React Native package supplies session-storage and native-SQLite adapter contracts without bundling a runtime database.",
    why: "Applications retain control of Expo/native dependencies while FFDB owns session validation and replica semantics.",
    when: "Use it for React Native or Expo apps that need durable sessions or offline SQLite replicas.",
    prerequisites: ["Version-matched client, sync-client, and react-native SDK packages.", "An encrypted async key-value store and SQLite runtime supporting STRICT tables, upsert, and atomic transactions."],
    requiredValues: ["AsyncKeyValueStorage, NativeSQLiteDriver, database location, session key, and lifecycle/network triggers.", "A user-scope strategy that prevents different identities sharing visible cached rows."],
    steps: ["Wrap the runtime storage in AsyncKeyValueStorage.", "Wrap SQLite execute and transaction APIs in NativeSQLiteDriver.", "Construct ReactNativeSessionStore and NativeSQLiteReplica.", "Initialize the replica and construct OfflineSyncClient.", "Wire active/online events to bounded sync attempts."],
    result: "Valid sessions, rows, cursors, and queued mutations persist across app restart without browser APIs.",
    failures: ["Persisted session JSON is invalid — the store removes it and requires sign-in.", "Initialization fails transiently — fix the runtime cause and retry; the adapter clears the failed promise."],
    nextSteps: ["Test cold-start and account-switch behavior.", "Implement rejected-mutation UI."],
  },
  "/sync-client": {
    what: "The sync-client SDK package provides OfflineSyncClient orchestration and the ReplicaAdapter contract.",
    why: "It centralizes snapshot-push-pull ordering and supplies durable browser and Node adapters while retaining a custom adapter contract.",
    when: "Use it when low-level sync is needed but the app should not reimplement batching, receipts, controls, and state publication.",
    prerequisites: ["Version-matched @ffdb/client and @ffdb/sync-client packages.", "IndexedDB in a supported browser, Node 24+ for built-in SQLite, NativeSQLiteReplica on React Native, or another transaction-tested adapter."],
    requiredValues: ["Replica adapter, push batch 1–100, pull batch 1–1,000, optional clock, mutation values, and AbortSignal.", "A listener for idle, snapshot, push, pull, and error phases."],
    steps: ["Choose IndexedDbReplica, NodeSQLiteReplica, NativeSQLiteReplica, or implement and transaction-test a custom adapter.", "Construct OfflineSyncClient with bounded options.", "Enqueue a mutation with a unique valid ID.", "Call sync and observe the phase sequence.", "Inspect rejected work and retry only according to its stable error."],
    result: "Concurrent sync calls share one run and durable state advances atomically through snapshot, push, and pull.",
    failures: ["The server returns missing or duplicate mutation results — fail the run without deleting pending work.", "The adapter loses pending work after restart — replace it before claiming offline durability."],
    nextSteps: ["Integrate lifecycle scheduling.", "Run the offline runtime acceptance matrix."],
  },
  "/cli": {
    what: "The packaged CLI manages platform credentials, organizations, projects, migrations, policies, storage, operations, scaffolding, and type generation.",
    why: "It gives operators a scriptable public surface without direct access to PostgreSQL or project files.",
    when: "Use it from an operator workstation or trusted automation environment.",
    prerequisites: ["The verified @ffdb/cli SDK package or packaged ffdb executable.", "Network access to a ready FFDB origin and an owner-only configuration location."],
    requiredValues: ["API URL, platform login or scoped project developer key, organization/project IDs, output mode, and confirmation policy.", "FFDB_CONFIG when the default credential path is unsuitable."],
    steps: ["Install the packaged CLI and verify ffdb --help.", "Log in for platform management or link a project developer key.", "Run a read-only health/schema command first.", "Apply the intended migration or management command.", "Use --json and explicit confirmation behavior in automation."],
    result: "The command returns stable human or JSON output and stores credentials only in the protected configured location.",
    failures: ["The CLI resolves the wrong project — pass an explicit target and inspect configuration before mutation.", "Automation waits for confirmation — resolve the target first, then use --yes only for that reviewed action."],
    nextSteps: ["Scaffold the application runtime.", "Generate and commit schema types."],
  },
  "/billing/platform": {
    what: "Platform billing is the released organization-entitlement contract for self-hosted deployments, including an always-readable summary and operator-configured Stripe Checkout, Customer Portal, and webhook handling.",
    why: "It lets each deployer choose private or team analytics without tenant charges, or run a monetized BYO/Connect instance that enforces the operator-owned Free, pay-as-you-go, and Pro contract.",
    when: "Use it when reading an organization's tier or limits, offering a configured billing redirect, or operating the Stripe webhook boundary.",
    prerequisites: ["An authorized platform session and the target organization ID for summary, Checkout, or Portal calls.", "A self-hosted server operator must configure Stripe before Checkout, Portal, or webhook processing is available; Free reads do not require Stripe."],
    requiredValues: ["organization_id, the intended tier pay_as_you_go or pro, and an Idempotency-Key for each logical Checkout or Portal operation.", "For provider processing: Stripe credentials, verified raw webhook payloads, provider event IDs, and server-owned organization billing state."],
    steps: ["Call organizationBilling() or GET /v1/organizations/:organization_id/billing and inspect tier, status, project_limit, allowances, and provider_configured.", "Treat Free with project_limit 2 as a working entitlement and handle project-limit enforcement instead of assuming billing is unavailable.", "If the operator configured Stripe, call createBillingCheckout() with pay_as_you_go or pro and a stable idempotency key, then send the user to the returned URL.", "For an existing provider customer, call createBillingPortal() with a stable idempotency key and use its short-lived redirect.", "Treat redirects as navigation only; let verified, idempotent Stripe webhooks update state and re-read the billing summary."],
    result: "Private/team instances retain analytics without billing enforcement; monetized BYO/Connect instances enforce Free allowances and reconcile operator-owned paid entitlements, usage, and invoices.",
    failures: ["Checkout or Portal returns billing.provider_unavailable — keep Free behavior active and ask the server operator to configure Stripe.", "Project creation reaches the Free two-project limit — do not bypass it; present the current entitlement and available operator-configured upgrade path.", "A redirect succeeds but billing remains unchanged — wait for or diagnose the verified webhook and re-read the summary.", "Webhook signature verification fails — reject the event without applying state and inspect the operator's endpoint-secret configuration."],
    nextSteps: ["Configure Project commerce independently for application sales.", "Review production security for provider-secret and webhook-boundary operation."],
  },
  "/billing/project-payments": {
    what: "Project commerce is the released, project-scoped sales contract exposed by client.commerce and /v1/projects/:project_id/commerce routes.",
    why: "It gives applications one tenant-bound model for products, prices, purchases, memberships, refunds, entitlements, and fulfillment without coupling them to FFDB platform billing.",
    when: "Use it when the application itself sells a one-time product or recurring membership through Stripe.",
    prerequisites: ["A project owner/admin session or commerce_manage developer key for configuration and management.", "Either project-owned Stripe secret/webhook keys or deployment-enabled Stripe Connect; HTTPS return, refresh, success, and cancel URLs for production."],
    requiredValues: ["Project ID, provider mode, product and immutable price terms, integer minor-unit amount, currency, and durable idempotency keys.", "For memberships: an individual, team, or organization subject and explicit entitlement map."],
    steps: ["Configure BYO credentials with commerce.configureByo() or create Accounts v2 onboarding with commerce.connectOnboarding().", "For BYO, register the account summary's per-project webhook URL; for Connect, register the single deployment /v1/commerce/webhooks/stripe-connect URL with its dedicated project-Connect endpoint secret.", "Create the product and immutable one-time or recurring price.", "Create a hosted Checkout session and navigate to its URL.", "Treat the redirect as navigation only and let verified webhooks reconcile orders, payments, invoices, subscriptions, refunds, and entitlements.", "Read entitlements for the authenticated membership subject and advance fulfillment only after the order is paid."],
    result: "The project sells through its own merchant account while FFDB maintains tenant-bound, idempotent, webhook-reconciled commerce state.",
    failures: ["Account status is restricted — inspect requirements_due and finish provider onboarding.", "A Connect event sent to the per-project BYO route is rejected — deliver it to the global account-routed Connect endpoint.", "Disconnect returns commerce.account_in_use — preserve the binding because provider-bound commerce records exist.", "A Checkout redirect returns but no access appears — diagnose the signed webhook instead of trusting the browser redirect.", "A reused idempotency key has different input — issue a new key for the new logical operation.", "Fulfillment is rejected — verify captured funds still cover the order after pending and successful refunds."],
    nextSteps: ["Run the project-commerce acceptance matrix with Stripe sandbox events.", "Configure production webhook delivery and merchant operational ownership."],
  },
  "/host-updates": {
    what: "Host updates and rollback is the signed native-release lifecycle exposed to instance administrators through a narrow root-owned agent.",
    why: "Operators need a visible, auditable upgrade path without granting the portal or API general root or shell execution.",
    when: "Use it after a native install, before each server upgrade, when choosing an automatic check policy, or when a verified release must be rolled back.",
    prerequisites: ["A packaged native installation with the updater path unit and check timer active.", "Off-host backup custody, working readiness checks, and an owner or instance-administrator platform account."],
    requiredValues: ["The exact announced release version, canonical signature identity, compatibility state schema, and retained job ID.", "For automatic apply, an explicit UTC maintenance window and operational coverage."],
    steps: ["Inspect the installed and available versions plus updater capabilities.", "Check the stable release channel and review compatibility and release notes.", "Reauthenticate if required, confirm the mandatory backup, and submit the exact install version.", "Follow the persisted job through verification, backup, activation, restart, and readiness.", "If acceptance fails, select only a compatible installed release or restore the coordinated backup."],
    result: "One signed release is active atomically, the complete versioned service set is ready, and the audit/job record identifies every lifecycle phase.",
    failures: ["Signature, checksum, or manifest verification fails — quarantine the download and do not activate it.", "The compatibility guard rejects rollback — restore the coordinated backup rather than bypassing the state boundary.", "The portal disconnects during restart — keep the job ID and let bounded reconnect polling resume before submitting anything else."],
    nextSteps: ["Run release acceptance against auth, RLS, storage, sync, observability, and restore.", "Review retained jobs and updater timer policy during normal operations."],
  },
  "/backups": {
    what: "Backups and restore covers encrypted project-database backups plus complete packaged-host recovery archives, integrity checks, retention, and explicit restore.",
    why: "Copying a live SQLite file or keeping only PostgreSQL does not recover complete FFDB application state.",
    when: "Use it before production launch, before upgrades, and during scheduled restore exercises or incidents.",
    prerequisites: ["A configured independent backup root and backup master key.", "Separate PostgreSQL, object-storage, and organization-metrics backup/replication plans."],
    requiredValues: ["Exact organization/project, backup ID, integrity result, encryption key custody, retention, and recovery objectives.", "A quiescence plan, destination route for project restore, and coordinated PostgreSQL/metrics-ledger recovery point."],
    steps: ["Create a project backup through the trusted operation.", "For complete host recovery, run ffdb-host backup create on packaged single-host or ffdb-backup create on native systemd.", "Encrypt and replicate the resulting mode-0600 archive off host.", "Run integrity checks and isolated restore drills on schedule.", "Stop mutation services before restore and pass --yes only after resolving the exact versioned archive.", "Verify schema, RLS, storage metadata, sync behavior, usage summaries, reporting reconciliation, and both API and gateway readiness."],
    result: "A documented restore recreates the intended project database without exposing durable plaintext.",
    failures: ["Integrity fails — quarantine the artifact and select a known-good backup.", "The backup key is unavailable — restore is impossible; fix key escrow before production."],
    nextSteps: ["Record restore evidence against recovery objectives.", "Review key rotation and incident response."],
  },
  "/observability": {
    what: "Observability is the retained project and instance performance workspace plus the lower-level Prometheus, log, and request-correlation interfaces.",
    why: "Operators need time-windowed QPS, latency, errors, saturation, route hot spots, and query hot spots without copying customer SQL into a telemetry store.",
    when: "Use it for routine capacity review, performance regression analysis, incident triage, and alert validation.",
    prerequisites: ["A platform session with organization membership for project scope or instance administration for instance scope.", "Migration 14 applied to control-plane PostgreSQL and enough database capacity for minute aggregates."],
    requiredValues: ["Project or instance scope and a 1h, 6h, 24h, 7d, or 30d range.", "For request-specific diagnosis, a safe X-Request-Id and bounded log window."],
    steps: ["Open Observability and select the narrowest useful scope and time range.", "Compare QPS, latency percentiles, and error rate across the chart window.", "Inspect worker execution-slot and filesystem saturation.", "Sort routes and normalized query fingerprints by frequency or p95 latency.", "Correlate a specific failure by request ID, resolve the cause, and confirm recovery in the next retained buckets."],
    result: "Operators can identify traffic, latency, error, worker, disk, route, and query-shape pressure without retaining raw SQL, identifiers, or values.",
    failures: ["Dropped samples is nonzero — inspect recorder/PostgreSQL and worker saturation before trusting the affected window as complete.", "Charts stay empty after traffic — verify migration 14 and wait for the five-second aggregate flush.", "Health is green while readiness is red — investigate dependencies rather than restarting blindly."],
    nextSteps: ["Define thresholds from measured production baselines.", "Connect request-level findings to incident response and scaling."],
  },
  "/security": {
    what: "Production security is the deployment checklist for preserving FFDB's documented trust boundaries.",
    why: "Self-hosting makes network, secret, storage, proxy, backup, and operational controls part of the product boundary.",
    when: "Use it before admitting untrusted traffic, after topology changes, and during every release review.",
    prerequisites: ["Completed architecture, configuration, backup, and observability decisions.", "An owner for threat review, incident response, patching, and secret rotation."],
    requiredValues: ["Exact public/trusted proxy origins, private network allowlists, storage paths, provider endpoints, independent secrets, and resource limits.", "Evidence for RLS, auth, storage, sync, backup, and restore acceptance."],
    steps: ["Verify TLS and the trusted proxy boundary.", "Restrict PostgreSQL, worker IPC, project files, backups, metrics, and provider credentials.", "Confirm exact S3 endpoint and browser-origin controls.", "Run security and isolation acceptance for the release.", "Record residual risks and rollback authority."],
    result: "The deployed system matches the threat model and no undocumented convenience path bypasses authorization or isolation.",
    failures: ["A service needs broad root/capability access — stop and redesign the permission boundary.", "A control is assumed because of a certification claim — replace it with tested evidence; FFDB claims no formal certification."],
    nextSteps: ["Run the manual acceptance plan.", "Schedule key rotation and restore exercises."],
  },
  "/reference/client": {
    what: "Client API reference maps the current FFDBClient methods and subclients.",
    why: "It is the fastest way to select the correct public method without depending on internal HTTP or worker envelopes.",
    when: "Use it while implementing or reviewing SDK calls after reading the task guide for that domain.",
    prerequisites: ["A version-matched @ffdb/client package.", "The server release's OpenAPI contract for final route/schema authority."],
    requiredValues: ["Method request object, tagged values, options such as signal/idempotency key, and required credential class.", "Expected response and FFDBError handling."],
    steps: ["Choose the domain subclient or top-level method.", "Open its exported TypeScript declaration.", "Construct the typed request with runtime-safe credentials.", "Handle the documented response and errors.", "Verify behavior against the matching task guide."],
    result: "The implementation calls an exported, version-matched method with the correct request, credential, and cancellation semantics.",
    failures: ["A method exists only in internal code — do not depend on it until exported and documented.", "Type declarations and OpenAPI disagree — stop and report release-contract drift."],
    nextSteps: ["Open the relevant auth, storage, sync, or query guide.", "Use Error envelopes for recovery behavior."],
  },
  "/reference/errors": {
    what: "Error envelopes define stable FFDBError fields for safe user behavior and operator correlation.",
    why: "Applications should branch on stable codes and status, not provider text or internal SQLite messages.",
    when: "Use it for every request boundary, retry policy, UI error state, and support workflow.",
    prerequisites: ["A client request capable of failing or being aborted.", "A logging policy that allows request IDs but excludes credentials and signed URLs."],
    requiredValues: ["Stable code, HTTP status, requestId, bounded details, Retry-After where present, and operation idempotency.", "A user-safe message and operator escalation path."],
    steps: ["Catch FFDBError at the request boundary.", "Branch on code/status and distinguish cancellation.", "Honor Retry-After only for retry-safe work.", "Show a safe action to the user.", "Send requestId and bounded context to operators."],
    result: "Failures produce predictable UI and diagnostics without exposing internal or secret values.",
    failures: ["Code retries a non-idempotent mutation blindly — stop and reconcile the operation first.", "UI displays raw provider/SQLite text — replace it with stable public messaging."],
    nextSteps: ["Add error fixtures to application tests.", "Use Observability to correlate live request IDs."],
  },
  "/reference/http-api": {
    what: "HTTP API reference points to the running release's OpenAPI document and public routing conventions.",
    why: "The deployed document is authoritative for exact routes and schemas, while the SDK handles common envelope and session details.",
    when: "Use it for non-TypeScript clients, contract generation, gateway review, or SDK contract verification.",
    prerequisites: ["A ready FFDB deployment and access to /openapi.json.", "A generator or HTTP client that preserves tagged values, headers, and error envelopes."],
    requiredValues: ["Base URL, project/credential context, Content-Type, request body, X-Request-Id response, and idempotency header where required.", "The OpenAPI document from the exact deployed release."],
    steps: ["Download /openapi.json from the target deployment.", "Pin it with the application build or generated client.", "Implement authentication and tagged values exactly.", "Exercise success plus 400/401/403/409/413/429 responses.", "Regenerate and review diffs during upgrades."],
    result: "The non-TypeScript integration matches the deployed public contract and preserves stable errors and request correlation.",
    failures: ["A copied static contract differs from the deployment — use the running release document.", "The gateway does not proxy /openapi.json or /v1 — fix routing before client work."],
    nextSteps: ["Prefer the TypeScript client when applicable.", "Add contract-diff checks to release validation."],
  },
};

const contextualIntroHeadings: Readonly<Record<string, string>> = {
  "/": "Start with the right FFDB path",
  "/quickstart": "From install to first query",
  "/install/docker": "Run the complete stack with Compose",
  "/install/systemd": "Run FFDB as native Linux services",
  "/self-hosting": "Choose a deployment topology",
  "/configuration": "Define the deployment boundary",
  "/database": "How project databases are isolated",
  "/queries": "Execute trusted application queries",
  "/migrations": "Change schemas without drift",
  "/row-level-security": "Enforce access beside the data",
  "/sql-support": "Know the supported SQLite surface",
  "/authentication": "Establish an end-user session",
  "/jwt-claims": "Use verified identity in policies",
  "/storage": "Authorize files through project metadata",
  "/multipart-uploads": "Upload large objects safely",
  "/sync": "Move changes between server and replica",
  "/offline": "Keep a durable local replica",
  "/conflicts": "Resolve competing offline writes",
  "/client": "Connect a TypeScript application",
  "/react": "Bind FFDB state to React",
  "/react-native": "Bring sessions and replicas to native apps",
  "/sync-client": "Orchestrate offline synchronization",
  "/cli": "Operate FFDB from the terminal",
  "/billing/platform": "Charge for hosted FFDB usage",
  "/billing/project-payments": "Sell products and memberships",
  "/host-updates": "Apply signed releases without widening root access",
  "/backups": "Recover the complete data boundary",
  "/observability": "Diagnose a live FFDB deployment",
  "/security": "Harden the production boundary",
  "/reference/client": "Find the correct client method",
  "/reference/errors": "Handle failures predictably",
  "/reference/http-api": "Call the deployed HTTP contract",
};

const compactQuickstartSections: readonly DocSection[] = [
  {
    heading: "Install the local evaluation profile",
    paragraphs: [
      "This signed release starts FFDB, PostgreSQL, MinIO, Mailpit, the workers, and the compiled gateway on one loopback-only host. It is the fastest path to a working local instance; use the Docker or systemd installation guide for customization and internet production.",
    ],
    code: {
      label: "Terminal",
      language: "sh",
      code: `curl -fsSLo ffdb-install.sh \\
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh
less ffdb-install.sh
sudo sh ffdb-install.sh --profile single-host --start --require-signature`,
    },
    callout: {
      kind: "warning",
      title: "Local evaluation only",
      body: "The single-host profile uses FFDB_ENVIRONMENT=development. Read Install with Docker Compose or Install with systemd before exposing FFDB to the internet.",
    },
  },
  {
    heading: "Verify the gateway",
    paragraphs: [
      "Port 5173 is the compiled nginx gateway, not a Vite development server. It serves the production portal and proxies API requests to Axum on the private Compose network; the packaged profile does not publish Axum port 8080 to the host.",
    ],
    code: {
      label: "Terminal",
      language: "sh",
      code: `sudo ffdb-host status
curl --fail http://127.0.0.1:5173/readyz
# Open http://127.0.0.1:5173/app/ in a trusted browser`,
    },
  },
  {
    heading: "Create the owner and first project",
    paragraphs: [
      "The installer writes independent secrets to the root-only /etc/ffdb/single-host.env file and never prints them. Copy the one-time bootstrap token into a protected file, then use the portal to create the owner, choose the instance mode, create an organization, and create a project.",
    ],
    code: {
      label: "Copy the one-time token",
      language: "sh",
      code: `sudo sh -c 'umask 077; sed -n "s/^FFDB_BOOTSTRAP_TOKEN=//p" \\
  /etc/ffdb/single-host.env > /root/ffdb-bootstrap-token'`,
    },
    bullets: [
      "Open http://127.0.0.1:5173/app/ and paste the protected token.",
      "Create the first owner and select private, team, Stripe BYO, or Stripe Connect operation.",
      "Create an organization and project, then copy the project ID.",
    ],
  },
  {
    heading: "Connect an application",
    paragraphs: ["Install the version-matched client, replace the project ID, sign in an end user, and issue the first parameterized query."],
    codes: [
      { label: "Terminal", language: "sh", code: `npm install @ffdb/client@0.3.13` },
      { label: "src/ffdb.ts", language: "ts", code: clientSetup },
      { label: "First query", language: "ts", code: queryExample },
    ],
    callout: {
      kind: "note",
      title: "Continue with the task guides",
      body: "Use Install for release pinning and production topology, Authentication for end-user sessions, Migrations for schema changes, and Client API for exact method signatures.",
    },
  },
];

export const pages: readonly DocPage[] = routePages.map((page) => {
  if (page.path === "/quickstart") return { ...page, sections: compactQuickstartSections };
  const guide = pageGuides[page.path];
  const subject = page.title.toLocaleLowerCase();
  return {
    ...page,
    sections: [
      {
        heading: contextualIntroHeadings[page.path] ?? `Understand ${subject}`,
        paragraphs: [guide.what, guide.why, guide.when],
      },
      {
        heading: `Requirements for ${page.title}`,
        bullets: [
          ...guide.prerequisites.map((item) => `Prerequisite — ${item}`),
          ...guide.requiredValues.map((item) => `Required value — ${item}`),
        ],
      },
      ...page.sections,
      {
        heading: `${page.title} workflow`,
        bullets: guide.steps.map((step, index) => `${index + 1}. ${step}`),
      },
      {
        heading: `Verify ${subject}`,
        paragraphs: [guide.result],
      },
      {
        heading: `Troubleshoot ${subject}`,
        bullets: guide.failures,
      },
      {
        heading: `Continue from ${page.title}`,
        bullets: guide.nextSteps,
      },
    ],
  };
});

export const navigation: readonly NavigationGroup[] = [
  { title: "Start here", links: [["Introduction", "/"], ["Quickstart", "/quickstart"]].map(([title, href]) => ({ title, href })) },
  { title: "Install", links: [["Docker Compose", "/install/docker"], ["systemd", "/install/systemd"], ["Deployment overview", "/self-hosting"], ["Configuration", "/configuration"]].map(([title, href]) => ({ title, href })) },
  { title: "Database", links: [["Architecture", "/database"], ["Queries", "/queries"], ["Migrations", "/migrations"], ["Row-level security", "/row-level-security"], ["SQL support", "/sql-support"]].map(([title, href]) => ({ title, href })) },
  { title: "Auth and storage", links: [["Authentication", "/authentication"], ["JWT claims", "/jwt-claims"], ["Object storage", "/storage"], ["Multipart uploads", "/multipart-uploads"]].map(([title, href]) => ({ title, href })) },
  { title: "Sync and offline", links: [["Sync protocol", "/sync"], ["Offline replicas", "/offline"], ["Conflict behavior", "/conflicts"]].map(([title, href]) => ({ title, href })) },
  { title: "SDKs and tools", links: [["TypeScript client", "/client"], ["React", "/react"], ["React Native", "/react-native"], ["Sync client", "/sync-client"], ["CLI", "/cli"]].map(([title, href]) => ({ title, href })) },
  { title: "Billing and payments", links: [["FFDB platform billing", "/billing/platform"], ["Project commerce", "/billing/project-payments"]].map(([title, href]) => ({ title, href })) },
  { title: "Operations", links: [["Backups and restore", "/backups"], ["Observability", "/observability"], ["Production security", "/security"]].map(([title, href]) => ({ title, href })) },
  { title: "Reference", links: [["Client API", "/reference/client"], ["Error envelopes", "/reference/errors"], ["HTTP API", "/reference/http-api"]].map(([title, href]) => ({ title, href })) },
] as const;

export const pageByPath = new Map(pages.map((page) => [page.path, page]));

export function searchPages(query: string, limit = 9): readonly DocPage[] {
  const normalized = normalizeSearch(query);
  if (normalized === "") return pages.slice(0, limit);
  return pages.filter((page) => normalizeSearch(pageSearchText(page)).includes(normalized)).slice(0, limit);
}

function pageSearchText(page: DocPage): string {
  return [
    page.title,
    page.description,
    page.group,
    ...page.sections.flatMap((section) => [
      section.heading,
      ...(section.paragraphs ?? []),
      ...(section.bullets ?? []),
      section.callout?.title ?? "",
      section.callout?.body ?? "",
      section.code?.code ?? "",
      ...(section.codes?.map((code) => code.code) ?? []),
    ]),
  ].join(" ");
}

function normalizeSearch(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9@]+/gu, " ").trim();
}

export function normalizePath(pathname: string): string {
  const withoutDocs = pathname === "/docs" ? "/" : pathname.replace(/^\/docs(?=\/)/, "");
  const clean = withoutDocs.replace(/\/+$/, "");
  return clean === "" ? "/" : clean;
}

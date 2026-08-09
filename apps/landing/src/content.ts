export const capabilities = [
  {
    number: "01",
    title: "Keep every project isolated",
    description:
      "Each project gets a separate SQLite application database, so one app cannot accidentally read or exhaust another app’s data.",
    visual: "database",
  },
  {
    number: "02",
    title: "Give each user only their rows",
    description:
      "Define access rules next to your schema. FFDB applies them to every supported read and write instead of relying on each screen to remember a filter.",
    visual: "shield",
  },
  {
    number: "03",
    title: "Keep working through a connection drop",
    description:
      "Built-in sessions and ordered logical sync let an app queue changes locally, reconnect, and reconcile with the server without downloading a database file.",
    visual: "sync",
  },
  {
    number: "04",
    title: "Protect files with the same rules",
    description:
      "Store uploads in S3-compatible infrastructure while FFDB keeps metadata, quotas, versions, and authorization tied to the project database.",
    visual: "storage",
  },
] as const;

export const workflow = [
  {
    number: "I",
    label: "Install FFDB",
    title: "Start a complete single-host release",
    body: "For an evaluation or isolated host, download the stable installer from the canonical GitHub Releases channel. It resolves the latest stable tag, verifies its signed assets, and starts PostgreSQL, object storage, captured email, FFDB, and a compiled nginx gateway. Port 5173 is that gateway—not a Vite development server. It serves the production landing, docs, and portal files and proxies API routes to the private Axum service on port 8080. It generates root-only secrets and preserves durable volumes. Pin an exact tag for reproducible production installs and use the external-provider profile before serving internet traffic.",
    code: `curl -fsSLo ffdb-install.sh \\
  https://github.com/Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh
less ffdb-install.sh
sudo sh ffdb-install.sh --profile single-host \\
  --start --require-signature
sudo ffdb-host status
# Compiled nginx gateway readiness; no Vite server runs here.
curl --fail http://127.0.0.1:5173/readyz`,
  },
  {
    number: "II",
    label: "Protect data",
    title: "Describe data and access together",
    body: "Ship the table and its access policy in one reviewed migration, then let the server enforce it for every client.",
    code: `-- migrate:up
CREATE TABLE documents (
  id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  title TEXT NOT NULL
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_read ON documents
  FOR SELECT TO authenticated
  USING (owner_id = auth.uid());`,
  },
  {
    number: "III",
    label: "Connect",
    title: "Connect with the typed client",
    body: "Install the public, version-matched @ffdb packages independently from the server bundle. The matching GitHub Release also carries verified offline tarballs. The client handles end-user sessions, ordered SQL results, storage signing, logical sync, platform billing, and complete project commerce workflows.",
    code: `import { FFDBClient } from "@ffdb/client";

const ffdb = new FFDBClient({
  baseUrl: "https://data.example.com",
  projectId: "your-project-id",
});

await ffdb.auth.signIn(email, password);
const result = await ffdb.query({
  sql: "select id, title from documents order by title",
});`,
  },
] as const;

export const facts = [
  { value: "1", label: "isolated SQLite application database per project" },
  { value: "2", label: "separate developer and end-user execution modes" },
  { value: "1", label: "ordered logical change stream for every replica" },
  { value: "0", label: "raw SQLite files exposed through the public API" },
] as const;

export const integrations = [
  ["@ffdb/client", "Browser, Node + native HTTP"],
  ["@ffdb/react", "React providers and hooks"],
  ["@ffdb/react-native", "Native storage + SQLite adapter"],
  ["@ffdb/sync-client", "Browser + Node durable replicas"],
  ["@ffdb/email-components", "Transactional email defaults"],
  ["@ffdb/cli", "Projects, migrations + operations"],
  ["Stripe Checkout", "Platform plan subscriptions"],
  ["Stripe Billing", "Tier catalog + customer portal"],
  ["Verified webhooks", "Replay-safe entitlements"],
  ["React", "Web applications"],
  ["React Native", "Native applications"],
  ["Expo", "Native SQLite integration"],
  ["Node.js", "Servers and tooling"],
  ["S3-compatible", "Object bytes"],
  ["PostgreSQL", "Control plane"],
  ["SQLite", "Project data"],
] as const;

export const deploymentShapes = [
  {
    number: "01",
    name: "Single-host evaluation",
    price: "Apache-2.0",
    description: "Start FFDB and its local dependencies from one signed, pinned release.",
    features: ["PostgreSQL, MinIO, and Mailpit included", "Signature and checksum verification", "Root-only generated secrets", "Data-preserving upgrades and rollback"],
  },
  {
    number: "02",
    name: "Production self-hosting",
    price: "Your infrastructure",
    description: "Run the external-provider release profile or install the same release binaries as Linux systemd services.",
    features: ["Docker Compose or systemd", "Bring PostgreSQL, HTTPS S3, email, and TLS", "Choose your region and network", "Own upgrades and backups"],
  },
  {
    number: "03",
    name: "Monetized FFDB instance",
    price: "Operator-owned",
    description: "Choose Stripe BYO or Connect during first-run setup to offer Free, usage, and subscription plans from your own deployment.",
    features: ["Automatic Stripe catalog provisioning", "Free, pay-as-you-go, and Pro", "Durable metering and reconciliation", "Operator-owned invoices and Customer Portal"],
  },
] as const;

export const billingPlans = [
  {
    number: "01",
    name: "Free",
    price: "$0",
    note: "No payment method",
    description: "On a monetized instance, start with two projects and enforced included usage; private and team instances stay uncharged while retaining usage analytics.",
    features: ["2 active projects on a monetized instance's Free policy", "1 GB organization storage allowance", "1M reads and 50k writes / month", "5k monthly active users", "Automatic metering with write, signup, and storage admission"],
  },
  {
    number: "02",
    name: "Pay as you go",
    price: "Usage",
    note: "Free base + metered overage",
    description: "Enable additional projects with durable usage reporting, reconciliation, invoices, and transparent metered overage.",
    features: ["$0.20 / GB-month, prorated from byte-hours", "$0.25 / million reads", "$1.50 / million writes up to 1M", "$2.25 / million writes after 1M", "$0.005 / MAU up to 50k; $0.015 beyond"],
  },
  {
    number: "03",
    name: "Pro",
    price: "$7",
    note: "Monthly subscription",
    description: "A predictable subscription with larger included allowances, automatic overage reporting, and invoice history.",
    features: ["10 GB storage", "15M reads / month", "750k writes / month", "50k monthly active users", "Customer Portal upgrades and cancellation"],
  },
] as const;

export const packageReleaseStatus = {
  registry: "public-ffdb-scope",
  installMode: "npm-or-verified-release-assets",
} as const;

export const currentPackageNames = [
  "@ffdb/client",
  "@ffdb/react",
  "@ffdb/react-native",
  "@ffdb/sync-client",
  "@ffdb/email-components",
  "@ffdb/cli",
] as const;

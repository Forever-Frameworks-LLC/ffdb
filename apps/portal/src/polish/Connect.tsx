import { useState, type KeyboardEvent } from "react";
import {
  ArrowRight,
  Check,
  ChevronRight,
  Clipboard,
  Cloud,
  Code2,
  ExternalLink,
  KeyRound,
  Laptop,
  LockKeyhole,
  Server,
  ShieldCheck,
  Smartphone,
  TerminalSquare,
} from "lucide-react";

import type { PortalConfiguration } from "../config.js";
import "./connect.css";

const FFDB_VERSION = "0.3.9";

type RuntimeId = "react" | "expo" | "node";

interface ConnectPanelProps {
  readonly configuration: PortalConfiguration;
  onNotice(message: string): void;
  onOpenAuth(): void;
}

interface CodeBlock {
  readonly id: string;
  readonly label: string;
  readonly value: string;
}

interface RuntimeGuide {
  readonly id: RuntimeId;
  readonly label: string;
  readonly eyebrow: string;
  readonly title: string;
  readonly detail: string;
  readonly icon: typeof Code2;
  readonly docsPath: string;
  readonly blocks: readonly CodeBlock[];
  readonly trusted: boolean;
}

interface ReadinessStep {
  readonly title: string;
  readonly detail: string;
}

interface ReadinessGuide {
  readonly title: string;
  readonly detail: string;
  readonly actionLabel?: string;
  readonly steps: readonly ReadinessStep[];
}

const runtimeReadinessGuides: Readonly<Record<RuntimeId, ReadinessGuide>> = {
  react: {
    title: "Before your first React web request",
    detail: "Use the addresses your app actually runs on. FFDB does not infer a framework, domain, or local development port.",
    actionLabel: "Configure web and auth URLs",
    steps: [
      { title: "Install the exact SDK release", detail: "Keep the client, React, and sync packages on the same version as this FFDB deployment." },
      { title: "Copy this project’s public values", detail: "Use the API origin and project ID above. Never add the portal’s project credential to browser code." },
      { title: "Register each real browser origin", detail: "Add the exact scheme, host, and port shown by every development, preview, and production browser. Web origins never include a path." },
      { title: "Separate auth and storage URLs", detail: "Register exact verification and password-reset return pages as auth redirects. Signed storage requests may also require the provider’s own CORS allowlist." },
    ],
  },
  expo: {
    title: "Before your first Expo / native request",
    detail: "Native apps, Expo Web, and authentication returns use different URL rules. Configure only the surfaces your app actually ships.",
    actionLabel: "Configure auth and web URLs",
    steps: [
      { title: "Install the exact native SDK release", detail: "Keep the client, React, React Native, and sync packages aligned with this FFDB deployment." },
      { title: "Copy public Expo configuration", detail: "The API origin and project ID are safe in EXPO_PUBLIC_* variables. Developer keys are not." },
      { title: "Treat native and web networking separately", detail: "iOS and Android API calls do not use browser CORS. Expo Web needs the exact HTTP(S) origin displayed by its browser." },
      { title: "Choose an auth return strategy", detail: "Native auth currently returns through an allowed HTTPS universal/app link. Custom URI schemes such as my-app:// are not accepted yet." },
    ],
  },
  node: {
    title: "Before your first Node request",
    detail: "A trusted Node process connects directly to FFDB. It does not need a browser origin or authentication redirect allowlist.",
    steps: [
      { title: "Install the exact SDK release", detail: "Keep the client and sync packages on the same version as this FFDB deployment." },
      { title: "Store project values server-side", detail: "Load the API origin, project ID, and a least-privilege developer key from a secret manager or ignored environment file." },
      { title: "No browser URL allowlist required", detail: "Node is not subject to browser CORS. Configure application URLs only for a separate browser or native client that your server supports." },
      { title: "Use the trusted path for setup", detail: "Apply migrations, create buckets, and run administrative jobs with scoped credentials before end users connect." },
    ],
  },
};

export function ConnectPanel({ configuration, onNotice, onOpenAuth }: ConnectPanelProps) {
  const [runtime, setRuntime] = useState<RuntimeId>("react");
  const [copied, setCopied] = useState<string | null>(null);
  const apiUrl = configuration.apiUrl.replace(/\/$/u, "");
  const guides = runtimeGuides(apiUrl, configuration.projectId);
  const activeGuide = guides.find((guide) => guide.id === runtime) ?? guides[0]!;
  const readiness = runtimeReadinessGuides[runtime];

  const copy = async (id: string, label: string, value: string) => {
    try {
      await globalThis.navigator.clipboard.writeText(value);
      setCopied(id);
      onNotice(`${label} copied to the clipboard.`);
      globalThis.setTimeout(() => setCopied((current) => current === id ? null : current), 1_800);
    } catch {
      onNotice(`Could not copy ${label.toLowerCase()}. Select the text and copy it manually.`);
    }
  };

  const selectRelativeTab = (event: KeyboardEvent<HTMLButtonElement>, current: RuntimeId) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const index = guides.findIndex((guide) => guide.id === current);
    const delta = event.key === "ArrowRight" ? 1 : -1;
    const next = guides[(index + delta + guides.length) % guides.length]!;
    setRuntime(next.id);
    globalThis.document.getElementById(`connect-tab-${next.id}`)?.focus();
  };

  return (
    <div className="connect-route">
      <header className="connect-hero">
        <div className="connect-hero__copy">
          <span className="connect-eyebrow"><Cloud size={14} /> Project connection</span>
          <h1>Bring {configuration.projectName} into your app.</h1>
          <p>Use the active project in a browser, native app, or trusted Node process. Every value below follows the project selector in the workspace.</p>
        </div>
        <div className="connect-hero__status" aria-label="Connection target">
          <span><span className="connect-live-dot" /> Selected project</span>
          <strong>{configuration.projectName}</strong>
          <code>{configuration.projectId}</code>
        </div>
      </header>

      <section className="connect-facts" aria-label="Project connection values">
        <ConnectionFact icon={<Cloud size={16} />} label="API origin" value={apiUrl} copyId="fact-api" copied={copied} onCopy={copy} />
        <ConnectionFact icon={<Code2 size={16} />} label="Project ID" value={configuration.projectId} copyId="fact-project" copied={copied} onCopy={copy} />
        <ConnectionFact icon={<ShieldCheck size={16} />} label="SDK release" value={`Exact version ${FFDB_VERSION}`} />
      </section>

      <section className="connect-guide" aria-labelledby="connect-guide-title">
        <div className="connect-guide__heading">
          <div><span className="connect-section-label">Choose a runtime</span><h2 id="connect-guide-title">Copy a working foundation</h2></div>
          <a href={`${apiUrl}${activeGuide.docsPath}`} target="_blank" rel="noreferrer">Read {activeGuide.label} docs <ExternalLink size={13} /></a>
        </div>
        <div className="connect-tabs" role="tablist" aria-label="Application runtime">
          {guides.map((guide) => {
            const RuntimeIcon = guide.icon;
            return <button
              aria-controls={`connect-panel-${guide.id}`}
              aria-selected={runtime === guide.id}
              id={`connect-tab-${guide.id}`}
              key={guide.id}
              role="tab"
              tabIndex={runtime === guide.id ? 0 : -1}
              type="button"
              onClick={() => setRuntime(guide.id)}
              onKeyDown={(event) => selectRelativeTab(event, guide.id)}
            ><RuntimeIcon size={17} /><span>{guide.label}</span>{guide.trusted ? <small>Trusted</small> : null}</button>;
          })}
        </div>
        <div className="connect-runtime" id={`connect-panel-${activeGuide.id}`} role="tabpanel" aria-labelledby={`connect-tab-${activeGuide.id}`} tabIndex={0}>
          <div className="connect-runtime__intro">
            <span>{activeGuide.eyebrow}</span>
            <h3>{activeGuide.title}</h3>
            <p>{activeGuide.detail}</p>
            {activeGuide.trusted ? <div className="connect-trust-note"><LockKeyhole size={16} /><span><strong>Server-side only</strong> Developer keys belong in a secret manager or an ignored local environment file—never in browser or <code>EXPO_PUBLIC_*</code> variables.</span></div> : <div className="connect-trust-note is-safe"><ShieldCheck size={16} /><span><strong>Public configuration only</strong> Project IDs and API origins identify the target. End-user access still requires authentication and is authorized by RLS.</span></div>}
          </div>
          <div className="connect-code-stack">
            {activeGuide.blocks.map((block) => <CodeCard block={block} copied={copied === block.id} key={block.id} onCopy={copy} />)}
          </div>
        </div>
      </section>

      <section className="connect-readiness" aria-labelledby="runtime-readiness-title">
        <div className="connect-readiness__intro">
          <span className="connect-section-label">Runtime checklist</span>
          <h2 id="runtime-readiness-title">{readiness.title}</h2>
          <p>{readiness.detail}</p>
          {readiness.actionLabel === undefined ? null : <button type="button" onClick={onOpenAuth}><KeyRound size={15} /> {readiness.actionLabel} <ArrowRight size={14} /></button>}
        </div>
        <ol className="connect-steps">
          {readiness.steps.map((step, index) => <li key={step.title}><span>{String(index + 1).padStart(2, "0")}</span><div><strong>{step.title}</strong><p>{step.detail}</p></div></li>)}
        </ol>
      </section>

      <footer className="connect-footer-note"><TerminalSquare size={16} /><span>The repository’s <code>examples/field-notes</code> project exercises React, auth, RLS, transactions, offline sync, storage, sessions, diagnostics, and a Node SQLite replica on localhost.</span><ChevronRight size={15} /></footer>
    </div>
  );
}

function ConnectionFact({ icon, label, value, copyId, copied, onCopy }: {
  readonly icon: React.ReactNode;
  readonly label: string;
  readonly value: string;
  readonly copyId?: string;
  readonly copied?: string | null;
  onCopy?(id: string, label: string, value: string): Promise<void>;
}) {
  return <article><span className="connect-fact-icon">{icon}</span><div><small>{label}</small><strong title={value}>{value}</strong></div>{copyId === undefined || onCopy === undefined ? null : <CopyControl copied={copied === copyId} label={label} onClick={() => void onCopy(copyId, label, value)} />}</article>;
}

function CodeCard({ block, copied, onCopy }: { readonly block: CodeBlock; readonly copied: boolean; onCopy(id: string, label: string, value: string): Promise<void> }) {
  return <article className="connect-code-card"><header><span>{block.label}</span><CopyControl copied={copied} label={block.label} onClick={() => void onCopy(block.id, block.label, block.value)} /></header><pre tabIndex={0}><code>{block.value}</code></pre></article>;
}

function CopyControl({ copied, label, onClick }: { readonly copied: boolean; readonly label: string; onClick(): void }) {
  return <button className={copied ? "connect-copy is-copied" : "connect-copy"} type="button" aria-label={copied ? `${label} copied` : `Copy ${label}`} title={copied ? "Copied" : `Copy ${label}`} onClick={onClick}>{copied ? <Check size={14} /> : <Clipboard size={14} />}<span>{copied ? "Copied" : "Copy"}</span></button>;
}

function runtimeGuides(apiUrl: string, projectId: string): readonly RuntimeGuide[] {
  return [
    {
      id: "react",
      label: "React web",
      eyebrow: "Browser application",
      title: "React providers with a session-scoped client",
      detail: "Start with end-user auth and add the sync client when the product needs optimistic offline work.",
      icon: Laptop,
      docsPath: "/docs/react",
      trusted: false,
      blocks: [
        { id: "react-install", label: "Install", value: `npm install --save-exact @ffdb/client@${FFDB_VERSION} @ffdb/react@${FFDB_VERSION} @ffdb/sync-client@${FFDB_VERSION}` },
        { id: "react-env", label: ".env.local", value: `VITE_FFDB_API_URL=${apiUrl}\nVITE_FFDB_PROJECT_ID=${projectId}` },
        { id: "react-client", label: "src/ffdb.ts", value: `import { BrowserSessionStore, FFDBClient } from "@ffdb/client";\n\nexport const ffdb = new FFDBClient({\n  baseUrl: import.meta.env.VITE_FFDB_API_URL,\n  projectId: import.meta.env.VITE_FFDB_PROJECT_ID,\n  sessionStore: new BrowserSessionStore(\n    sessionStorage,\n    "my-app.ffdb",\n  ),\n});` },
      ],
    },
    {
      id: "expo",
      label: "Expo / native",
      eyebrow: "React Native application",
      title: "OS-protected sessions for Expo",
      detail: "Adapt Expo SecureStore to FFDB’s async session contract. Add a verified native SQLite driver when durable offline sync is required.",
      icon: Smartphone,
      docsPath: "/docs/react-native",
      trusted: false,
      blocks: [
        { id: "expo-install", label: "Install", value: `npm install --save-exact @ffdb/client@${FFDB_VERSION} @ffdb/react@${FFDB_VERSION} @ffdb/react-native@${FFDB_VERSION} @ffdb/sync-client@${FFDB_VERSION}\nnpx expo install expo-secure-store` },
        { id: "expo-env", label: ".env.local", value: `EXPO_PUBLIC_FFDB_API_URL=${apiUrl}\nEXPO_PUBLIC_FFDB_PROJECT_ID=${projectId}` },
        { id: "expo-client", label: "src/ffdb.ts", value: `import * as SecureStore from "expo-secure-store";\nimport { FFDBClient } from "@ffdb/client";\nimport { ReactNativeSessionStore } from "@ffdb/react-native";\n\nconst secureStorage = {\n  getItem: SecureStore.getItemAsync,\n  setItem: SecureStore.setItemAsync,\n  removeItem: SecureStore.deleteItemAsync,\n};\n\nexport const ffdb = new FFDBClient({\n  baseUrl: process.env.EXPO_PUBLIC_FFDB_API_URL!,\n  projectId: process.env.EXPO_PUBLIC_FFDB_PROJECT_ID!,\n  sessionStore: new ReactNativeSessionStore(secureStorage),\n});` },
      ],
    },
    {
      id: "node",
      label: "Node",
      eyebrow: "Trusted server or automation",
      title: "A scoped project client for Node 24+",
      detail: "Use this path for migrations, schema inspection, jobs, and server-side database operations that need a project developer key.",
      icon: Server,
      docsPath: "/docs/client",
      trusted: true,
      blocks: [
        { id: "node-install", label: "Install", value: `npm install --save-exact @ffdb/client@${FFDB_VERSION} @ffdb/sync-client@${FFDB_VERSION}` },
        { id: "node-env", label: ".env", value: `FFDB_API_URL=${apiUrl}\nFFDB_PROJECT_ID=${projectId}\nFFDB_DEVELOPER_KEY=ffdb_dev_replace_me` },
        { id: "node-client", label: "src/ffdb.ts", value: `import { FFDBClient } from "@ffdb/client";\n\nexport const ffdb = new FFDBClient({\n  baseUrl: process.env.FFDB_API_URL!,\n  projectId: process.env.FFDB_PROJECT_ID!,\n  developerKey: process.env.FFDB_DEVELOPER_KEY!,\n});` },
      ],
    },
  ];
}

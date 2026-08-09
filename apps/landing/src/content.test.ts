import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { billingPlans, capabilities, currentPackageNames, deploymentShapes, packageReleaseStatus, workflow } from "./content";

describe("landing product content", () => {
  it("uses the public workspace package names", () => {
    const serialized = JSON.stringify(workflow);
    expect(serialized).toContain("@ffdb/client");
    expect(serialized).not.toContain('from "@ffdb/client"');
    expect(currentPackageNames).toEqual([
      "@ffdb/client",
      "@ffdb/react",
      "@ffdb/react-native",
      "@ffdb/sync-client",
      "@ffdb/email-components",
      "@ffdb/cli",
    ]);
    expect(currentPackageNames).toHaveLength(6);
  });

  it("describes the current security and deployment model", () => {
    const serialized = JSON.stringify({ capabilities, deploymentShapes });
    expect(serialized).toContain("separate SQLite application database");
    expect(serialized).toContain("S3-compatible");
    expect(serialized).toContain("Monetized FFDB instance");
    expect(serialized).toContain("Automatic Stripe catalog provisioning");
    expect(serialized.toLowerCase()).toContain("checksum");
    expect(serialized).not.toContain("One-time + subscription payments");
  });

  it("offers all scoped packages through npm and verified release assets", () => {
    expect(packageReleaseStatus.registry).toBe("public-ffdb-scope");
    expect(packageReleaseStatus.installMode).toBe("npm-or-verified-release-assets");
    expect(JSON.stringify(workflow)).not.toContain("make compose-rebuild");
    expect(JSON.stringify(workflow)).not.toContain("pnpm install --frozen-lockfile");
    expect(JSON.stringify(workflow)).toContain("--profile single-host");
    expect(JSON.stringify(workflow)).toContain("--require-signature");
    expect(JSON.stringify(workflow)).toContain("Forever-Frameworks-LLC/ffdb/releases/latest/download/install.sh");
    expect(JSON.stringify(workflow)).toContain("Pin an exact tag");
    expect(JSON.stringify(workflow)).toContain("/readyz");
    expect(JSON.stringify(workflow)).not.toContain("not published yet");
  });

  it("keeps local-repository and unpublished framing out of the public landing page", () => {
    const serialized = `${JSON.stringify(workflow)} ${readFileSync(new URL("./App.tsx", import.meta.url), "utf8")}`.toLowerCase();
    for (const stale of ["not published", "unpublished", "release candidate", "source checkout", "workspace link", "make compose-rebuild", "clone the repository"]) {
      expect(serialized, stale).not.toContain(stale);
    }
  });

  it("identifies the packaged 5173 listener as compiled nginx rather than Vite", () => {
    const install = workflow[0];
    expect(install.body).toContain("compiled nginx gateway");
    expect(install.body).toContain("Port 5173 is that gateway—not a Vite development server");
    expect(install.body).toContain("private Axum service on port 8080");
    expect(install.code).toContain("Compiled nginx gateway readiness; no Vite server runs here");
  });

  it("restores the configurable free, usage, and subscription plan model", () => {
    const application = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    expect(billingPlans.map((plan) => plan.name)).toEqual(["Free", "Pay as you go", "Pro"]);
    expect(JSON.stringify(billingPlans)).toContain("2 active projects");
    expect(JSON.stringify(billingPlans)).toContain("Customer Portal");
    expect(application).toContain('href="/docs/billing/platform"');
    expect(application).toContain("complete project commerce");
    expect(application).toContain("FFDB provisions the plan catalog");
    expect(application).not.toContain("project payment status");
    expect(application).not.toContain("Price IDs are operator-configured");
  });
});

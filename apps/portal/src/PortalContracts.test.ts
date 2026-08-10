import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { navigationGroups, pathForRoute, portalRoutes, routeFromLocation, type PortalRoute } from "./data.js";

const sourcePath = (path: string) => resolve(process.cwd(), "src", path);
const appSource = readFileSync(sourcePath("App.tsx"), "utf8");
const overviewSource = readFileSync(sourcePath("polish/OverviewWorkspace.tsx"), "utf8");
const databaseSource = readFileSync(sourcePath("polish/DatabaseActivity.tsx"), "utf8");
const observabilitySource = readFileSync(sourcePath("polish/Observability.tsx"), "utf8");
const authSource = readFileSync(sourcePath("polish/AuthSync.tsx"), "utf8");
const managedTableSource = readFileSync(sourcePath("polish/ManagedTable.tsx"), "utf8");
const polishedTableSource = readFileSync(sourcePath("polish/PolishedDataTable.tsx"), "utf8");
const commerceSource = readFileSync(sourcePath("Commerce.tsx"), "utf8");
const indexSource = readFileSync(resolve(process.cwd(), "index.html"), "utf8");
const prepaintSource = readFileSync(resolve(process.cwd(), "public", "prepaint.css"), "utf8");
const themePrepaintSource = readFileSync(resolve(process.cwd(), "public", "theme-prepaint.js"), "utf8");
const gatewaySource = readFileSync(resolve(process.cwd(), "..", "..", "infra", "docker", "portal-nginx.conf.template"), "utf8");
const rootCss = readFileSync(sourcePath("styles.css"), "utf8");
const databaseCss = readFileSync(sourcePath("polish/database-activity.css"), "utf8");
const observabilityCss = readFileSync(sourcePath("polish/observability.css"), "utf8");
const overviewCss = readFileSync(sourcePath("polish/overview-workspace.css"), "utf8");
const authCss = readFileSync(sourcePath("polish/auth-sync.css"), "utf8");
const managedTableCss = readFileSync(sourcePath("polish/managed-table.css"), "utf8");
const responsiveCss = [
  rootCss,
  overviewCss,
  authCss,
  databaseCss,
  managedTableCss,
  readFileSync(sourcePath("polish/account-admin.css"), "utf8"),
  readFileSync(sourcePath("polish/operate-routes.css"), "utf8"),
].join("\n");

const expectedPanel: Readonly<Record<PortalRoute, string>> = {
  Overview: "ProductionOverviewPanel",
  Projects: "ProductionWorkspacePanel",
  Members: "ProductionWorkspacePanel",
  "SQL Editor": "SqlEditorPanel",
  Migrations: "MigrationsPanel",
  Database: "PolishedDatabasePanel",
  Policies: "ProductionPoliciesPanel",
  Auth: "AuthRoute",
  Storage: "ProductionStoragePanel",
  Sync: "SyncRoute",
  Email: "ProductionEmailPanel",
  Activity: "PolishedActivityPanel",
  Observability: "ObservabilityPanel",
  Backups: "ProductionBackupsPanel",
  Usage: "ProductionUsagePanel",
  Products: "CommercePanel",
  Orders: "CommercePanel",
  Subscriptions: "CommercePanel",
  Instance: "InstancePanel",
  "Instance Billing": "InstancePanel",
  "Instance Users": "InstancePanel",
  Settings: "ProductionSettingsPanel",
  Account: "ProductionAccountPanel",
};

describe("portal route/action coverage matrix", () => {
  it("contains each route exactly once and makes every route URL round-trip", () => {
    expect(new Set(portalRoutes).size).toBe(portalRoutes.length);
    for (const route of portalRoutes) {
      const path = pathForRoute(route, "project_qa", "organization_qa");
      expect(routeFromLocation(path), `${route} at ${path}`).toBe(route);
    }
  });

  it("exposes every route through sidebar navigation or the account control", () => {
    const sidebarRoutes = navigationGroups.flatMap((group) => group.items.map((item) => item.label));
    expect(new Set([...sidebarRoutes, "Account"])).toEqual(new Set(portalRoutes));
  });

  it.each(portalRoutes)("renders %s with its production panel rather than a placeholder", (route) => {
    const escapedRoute = route.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
    expect(appSource).toMatch(new RegExp(`case ["']${escapedRoute}["']:\\s*return\\s*<${expectedPanel[route]}\\b`, "u"));
  });

  it("routes create-menu and overview actions to purposeful destinations", () => {
    for (const tuple of [
      '["New project", "Projects", "database"]',
      '["New migration", "Migrations", "code"]',
      '["Storage bucket", "Storage", "archive"]',
      '["Invite member", "Members", "users"]',
    ]) expect(appSource).toContain(tuple);

    for (const destination of ["SQL Editor", "Migrations", "Storage", "Database", "Members", "Activity"]) {
      expect(overviewSource).toContain(`onNavigate("${destination}")`);
    }
    expect(overviewSource).not.toMatch(/ow-quick-actions[\s\S]*?onClick=\{\(\) => onNotice\(/u);
  });

  it("returns invalid or signed-out account sessions to the authentication gate", () => {
    expect(appSource).toContain("if (developerAccess === null) {");
    expect(appSource).toContain("onAuthenticationRequired={() => setDeveloperAccess(null)}");
    expect(appSource).toContain("onSignedOut={props.onAuthenticationRequired}");
  });

  it("does not repeat the active route as a second page banner", () => {
    expect(appSource).not.toContain('className="page-header"');
    expect(appSource).not.toContain('className="project-identity"');
    expect(commerceSource).not.toMatch(/<h1>\{(?:copy|viewTitle)\./u);
  });

  it("prepaints the selected light or dark shell before React and CSS load", () => {
    expect(indexSource).toContain('src="%BASE_URL%theme-prepaint.js"');
    expect(indexSource).toContain('<meta name="color-scheme" content="light dark"');
    expect(indexSource).toContain('href="%BASE_URL%prepaint.css"');
    expect(themePrepaintSource).toContain('localStorage.getItem("ffdb.portal.theme")');
    expect(themePrepaintSource).toContain('root.classList.toggle("dark"');
    expect(prepaintSource).toContain("background: #f6f6f7;");
    expect(prepaintSource).toContain("background: #121214;");
    expect(prepaintSource).toContain("color-scheme: dark;");
    expect(rootCss).toContain(":root:not(.dark) {");
    expect(rootCss).toContain(":root.dark {");
    expect(rootCss).toContain(".main-content.main-content--workbench { padding: 0;");
  });

  it("makes every scope selector row a single full-size native dropdown target", () => {
    expect(appSource.match(/className="scope-native-select"/gu)).toHaveLength(3);
    expect(rootCss).toContain(".scope-native-select { position: absolute;");
    expect(rootCss).toContain("inset: 0;");
    expect(appSource).toContain('className="mobile-scope-trail"');
    expect(appSource).toContain('aria-label="Change deployment, organization, and project"');
    expect(rootCss).toContain(".mobile-scope-popover {");
    expect(rootCss).toContain(".mobile-scope-trail > svg:last-child { margin-left: auto; }");
  });

  it("keeps SQL editing production-capable rather than textarea-based", () => {
    expect(databaseSource).toContain("syntaxHighlighting(ffdbSqlHighlightStyle)");
    expect(databaseSource).toContain("EditorView.cspNonce.of(CODEMIRROR_STYLE_NONCE)");
    expect(gatewaySource).toContain("style-src 'self' 'nonce-ffdb-codemirror'");
    expect(gatewaySource).toContain("style-src-attr 'unsafe-inline'");
    expect(gatewaySource).toContain("script-src 'self'");
    expect(databaseSource).toContain('aria-label="Resize query and results panels"');
    expect(databaseSource).toContain("function EditableTableGrid");
    expect(databaseSource).toContain("client.transaction({ statements })");
  });

  it("keeps narrow database tables compact without sacrificing wide-table scrolling", () => {
    expect(databaseCss).toContain(".ffdb-data-table { width: 100%;");
    expect(databaseCss).toContain("table-layout: auto;");
    expect(databaseCss).toContain(".ffdb-edit-table { width: 100%; min-width: 0; table-layout: auto;");
    expect(databaseCss).toContain("border-right: 1px solid var(--ffdb-data-border);");
    expect(databaseCss).toContain(".ffdb-table-wrap { width: 100%; min-width: 0; max-width: 100%; overflow: auto;");
    expect(rootCss).not.toMatch(/(?:^|\n)th:nth-child\(/u);
  });

  it("keeps the activity detail sheet edge neutral in both themes", () => {
    expect(databaseCss).toContain("box-shadow: -8px 0 24px rgb(0 0 0 / 24%);");
    expect(databaseCss).not.toMatch(/\.ffdb-detail-drawer > section[^}]*box-shadow:[^;]*var\(--foreground\)/su);
  });

  it("uses the same edge-to-edge grid treatment for project resources and quick actions", () => {
    expect(overviewCss).toContain(".ow-project-workspace .ow-action-list { grid-template-columns: repeat(2, minmax(0, 1fr)); }");
    expect(overviewCss).toContain(".ow-project-workspace .ow-action-button:nth-child(odd) { border-right: 1px solid var(--ow-border); }");
    expect(overviewCss).toContain(".ow-project-workspace .ow-action-button:nth-child(n + 3) { border-top: 1px solid var(--ow-border); }");
    expect(overviewCss).toContain(".ow-project-workspace .ow-action-button:nth-child(odd):last-child { grid-column: 1 / -1; border-right: 0; }");
  });

  it("keeps standalone workspace creation dialogs opaque with visible actions", () => {
    expect(overviewCss).toContain(".ow-modal-backdrop {\n  --ow-card:");
    expect(overviewCss).toContain("--ow-dialog: var(--popover, var(--card, var(--surface, #fff)));");
    expect(overviewCss).toContain("background: var(--ow-dialog);");
    expect(overviewCss).toContain("background: color-mix(in oklab, var(--theme-muted, #e9ecef) 38%, var(--ow-dialog));");
    expect(overviewCss).toContain("border-color: color-mix(in oklab, var(--ow-primary) 72%, var(--ow-foreground));");
  });

  it("keeps every semantic table full-width and confines wide schemas to their own scroll region", () => {
    const tableSources = [appSource, overviewSource, databaseSource, observabilitySource, authSource, managedTableSource, polishedTableSource];
    expect(tableSources.reduce((total, source) => total + (source.match(/<table\b/gu)?.length ?? 0), 0)).toBe(14);
    expect(rootCss).toContain(".portal-table-scroll > table {\n  width: 100%;\n  max-width: none;");
    expect(rootCss).not.toMatch(/^(?:table|th|td|tbody\s+tr)(?:\s|\{|:|,)/gmu);
    expect(overviewCss).toContain(".ow-table { width: 100%;");
    expect(authCss).toContain(".auth-users-table,\n.sync-result-table { width: 100%; min-width: 760px;");
    expect(managedTableCss).toContain(".managed-table-scroll table { width: 100%; min-width: 680px;");
    expect(databaseCss).toContain(".ffdb-migration-table { width: 100%;");
    expect(databaseCss).toContain(".ffdb-activity-table { width: 100%;");
    expect(observabilityCss).toContain(".obs-table-scroll table { width: 100%; min-width: 980px;");
    expect([overviewCss, databaseCss, managedTableCss, authCss, observabilityCss].join("\n")).not.toContain("width: max-content; min-width: 100%");
  });

  it("contains migration history overflow inside a keyboard-focusable table region", () => {
    expect(databaseSource).toContain('aria-label="Migration history records" tabIndex={0} ref={historyTableRef} onScroll={updateHistoryTableScroll}');
    expect(databaseSource).toContain('aria-label="Scroll migration history right"');
    expect(databaseSource).toContain('className="ffdb-data-table ffdb-migration-table"');
    expect(databaseCss).toContain(".ffdb-migration-history { overflow: hidden;");
    expect(databaseCss).toContain(".ffdb-migration-workbench { width: 100%; min-width: 0;");
    expect(databaseCss).toContain(".ffdb-migration-panel { width: 100%; min-width: 0;");
    expect(databaseCss).toContain(".ffdb-migration-table-wrap { width: 100%; overflow: auto;");
    expect(databaseCss).toContain(".ffdb-migration-table { width: 100%;");
  });

  it("updates editor chrome and SQL tokens with the selected portal theme", () => {
    expect(databaseSource).toContain('color: "var(--ffdb-syntax-keyword)"');
    expect(databaseSource).toContain('backgroundColor: "var(--ffdb-editor-bg)"');
    expect(databaseCss).toContain('html[data-theme="light"] .ffdb-data-page');
    expect(databaseCss).toContain("--ffdb-editor-bg: #f7f7f8;");
    expect(databaseCss).toContain("--ffdb-editor-bg: #111214;");
  });

  it("treats migrations as a full-height tabbed workbench", () => {
    expect(appSource).toContain('selected === "Migrations" ? "main-content main-content--workbench"');
    expect(databaseSource).toContain('className="ffdb-migration-tabs" role="tablist"');
    expect(databaseSource).toContain('role="tabpanel" aria-labelledby="migration-new-tab"');
    expect(databaseCss).toContain(".ffdb-migrations-page { width: 100%; height: 100%;");
    expect(databaseCss).toContain(".ffdb-migration-workbench { width: 100%; min-width: 0; height: 100%;");
  });

  it("keeps every general-purpose growing table on the managed table contract", () => {
    for (const componentPath of ["./App.tsx", "./Instance.tsx", "./Commerce.tsx"]) {
      const source = readFileSync(sourcePath(componentPath), "utf8");
      expect(source).toContain("<ManagedTable");
    }
    expect(responsiveCss).toContain("managed-table-search");
    expect(responsiveCss).toContain("managed-table-footer");
  });

  it("ships retained performance telemetry as a first-class full-width workspace", () => {
    expect(appSource).toContain('selected === "Observability"');
    expect(observabilitySource).toContain("projectObservability");
    expect(observabilitySource).toContain("instanceObservability");
    expect(observabilityCss).toContain(".obs-table-scroll table { width: 100%; min-width: 980px;");
    expect(observabilitySource).toContain('aria-label="API route metrics"');
    expect(observabilitySource).toContain('aria-label="Query fingerprint metrics"');
    expect(observabilitySource).toContain("identifiers, comments, literals, and parameter values");
    expect(observabilityCss).toContain("height: 100%;");
    expect(observabilityCss).toContain("overflow-y: auto;");
    expect(observabilityCss).toContain("grid-auto-rows: max-content;");
    expect(observabilitySource).toContain("startRefreshCooldown(rateLimitCooldownSeconds)");
    expect(observabilitySource).toContain('role={notice.tone === "error" ? "alert" : "status"}');
    expect(navigationGroups.find((group) => group.label === "Workspace")?.items.some((item) => item.label === "Observability")).toBe(true);
  });
});

describe("responsive layout contract", () => {
  const viewportMatrix = [
    { width: 1440, expectedShell: "desktop" },
    { width: 1024, expectedShell: "desktop" },
    { width: 901, expectedShell: "desktop" },
    { width: 900, expectedShell: "drawer" },
    { width: 761, expectedShell: "drawer" },
    { width: 760, expectedShell: "drawer" },
    { width: 768, expectedShell: "drawer" },
    { width: 390, expectedShell: "drawer" },
  ] as const;

  it.each(viewportMatrix)("maps $width px to the $expectedShell shell", ({ width, expectedShell }) => {
    const drawerBreakpoint = 900;
    expect(width <= drawerBreakpoint ? "drawer" : "desktop").toBe(expectedShell);
    expect(rootCss).toContain(`@media (max-width: ${drawerBreakpoint}px)`);
  });

  it("does not impose a viewport wider than the supported 390 px mobile target", () => {
    const minimum = /html, body, #root \{ min-width: (\d+)px/u.exec(rootCss)?.[1];
    expect(Number(minimum)).toBeLessThanOrEqual(390);
    expect(responsiveCss).not.toMatch(/(?:^|[;{]\s*)width:\s*100vw\b/u);
    expect(rootCss).toMatch(/\.app-shell\s*\{[^}]*overflow:\s*hidden/u);
    expect(rootCss).toMatch(/\.app-shell\s*>\s*\.sidebar\s*\{\s*display:\s*none/u);
    expect(rootCss).toContain(".mobile-navigation-drawer .sidebar");
  });

  it("keeps the shared mobile shell and workbench routes vertically scrollable", () => {
    expect(rootCss).toContain("height: 100dvh;");
    expect(rootCss).toContain("overscroll-behavior-y: auto;");
    expect(rootCss).toContain(".main-content.main-content--workbench { min-height: calc(100vh - 58px); padding: 0; overflow: visible; }");
    expect(observabilityCss).toContain("grid-auto-rows: max-content;");
    expect(observabilityCss).toContain("@media (max-width: 900px)");
    expect(observabilityCss).toContain(".obs-page { height: auto; min-height: calc(100vh - 58px); overflow: visible; }");
    expect(databaseCss).toContain(".ffdb-output-content { min-height: 0; flex: 1; overflow: auto;");
    expect(databaseCss).toContain(".ffdb-migration-table-wrap { width: 100%; overflow: auto;");
    expect(databaseCss).toContain(".ffdb-table-wrap { width: 100%; min-width: 0; max-width: 100%; overflow: auto;");
    expect(rootCss).toContain(".portal-table-scroll {");
    expect(rootCss).toContain("scrollbar-gutter: stable;");
  });

  it("keeps mobile header controls compact without stacking account text", () => {
    expect(appSource).toContain('className="project-tool-label">Account</span>');
    expect(appSource).toContain('className="project-create-label">Create</span>');
    expect(rootCss).toContain('.project-tools > button[aria-label="Open account"] .project-tool-label { display: none; }');
    expect(rootCss).toContain(".mobile-brand .brand-lockup > span { display: none; }");
    expect(rootCss).toContain(".create-button { min-width: 78px; padding-inline: 8px; }");
  });

  it("gives each polished surface a mobile reflow rule and an overflow containment strategy", () => {
    for (const threshold of [420, 520, 540, 600, 900]) {
      expect(responsiveCss).toContain(`@media (max-width: ${threshold}px)`);
    }
    expect(responsiveCss).toMatch(/(?:overflow-x:\s*(?:auto|hidden)|overflow:\s*auto)/u);
    expect(responsiveCss).toContain("min-width: 0");
  });
});

import type { IconName } from "./icons.js";

export type PortalRoute =
  | "Overview"
  | "Projects"
  | "Members"
  | "SQL Editor"
  | "Migrations"
  | "Database"
  | "Policies"
  | "Auth"
  | "Storage"
  | "Sync"
  | "Email"
  | "Activity"
  | "Observability"
  | "Backups"
  | "Usage"
  | "Products"
  | "Orders"
  | "Subscriptions"
  | "Instance"
  | "Instance Billing"
  | "Instance Users"
  | "Updates"
  | "Settings"
  | "Account";

/**
 * The canonical inventory used by routing and QA contract tests. Keeping this
 * explicit makes newly added routes fail coverage until their URL, navigation,
 * description, and rendered panel are all wired.
 */
export const portalRoutes = [
  "Overview",
  "Projects",
  "Members",
  "SQL Editor",
  "Migrations",
  "Database",
  "Policies",
  "Auth",
  "Storage",
  "Sync",
  "Email",
  "Activity",
  "Observability",
  "Backups",
  "Usage",
  "Products",
  "Orders",
  "Subscriptions",
  "Instance",
  "Instance Billing",
  "Instance Users",
  "Updates",
  "Settings",
  "Account",
] as const satisfies readonly PortalRoute[];

export interface PortalNavigationItem {
  readonly label: PortalRoute;
  readonly icon: IconName;
  readonly requiresProject?: boolean;
  readonly administratorOnly?: boolean;
}

export interface PortalNavigationGroup {
  readonly label: "Workspace" | "Build" | "Operate" | "Sell" | "Administration";
  readonly items: readonly PortalNavigationItem[];
}

export const navigationGroups: readonly PortalNavigationGroup[] = [
  {
    label: "Workspace",
    items: [
      { label: "Overview", icon: "home", requiresProject: true },
      { label: "Projects", icon: "database" },
      { label: "Members", icon: "users" },
      { label: "Observability", icon: "chart", requiresProject: true },
    ],
  },
  {
    label: "Build",
    items: [
      { label: "SQL Editor", icon: "code", requiresProject: true },
      { label: "Database", icon: "database", requiresProject: true },
      { label: "Migrations", icon: "sync", requiresProject: true },
      { label: "Policies", icon: "shield", requiresProject: true },
      { label: "Auth", icon: "users", requiresProject: true },
      { label: "Storage", icon: "archive", requiresProject: true },
      { label: "Sync", icon: "sync", requiresProject: true },
      { label: "Email", icon: "mail", requiresProject: true },
    ],
  },
  {
    label: "Operate",
    items: [
      { label: "Activity", icon: "list", requiresProject: true },
      { label: "Backups", icon: "backup", requiresProject: true },
      { label: "Usage", icon: "creditCard" },
    ],
  },
  {
    label: "Sell",
    items: [
      { label: "Products", icon: "shoppingCart", requiresProject: true },
      { label: "Orders", icon: "list", requiresProject: true },
      { label: "Subscriptions", icon: "creditCard", requiresProject: true },
    ],
  },
  {
    label: "Administration",
    items: [
      { label: "Instance", icon: "settings", administratorOnly: true },
      { label: "Instance Billing", icon: "creditCard", administratorOnly: true },
      { label: "Instance Users", icon: "users", administratorOnly: true },
      { label: "Updates", icon: "sync", administratorOnly: true },
      { label: "Settings", icon: "settings" },
    ],
  },
] as const;

const routeSlug: Readonly<Record<PortalRoute, string>> = {
  Overview: "overview",
  Projects: "projects",
  Members: "members",
  "SQL Editor": "sql",
  Migrations: "migrations",
  Database: "database",
  Policies: "policies",
  Auth: "auth",
  Storage: "storage",
  Sync: "sync",
  Email: "email",
  Activity: "activity",
  Observability: "observability",
  Backups: "backups",
  Usage: "usage",
  Products: "products",
  Orders: "orders",
  Subscriptions: "subscriptions",
  Instance: "instance",
  "Instance Billing": "instance/billing",
  "Instance Users": "instance/users",
  Updates: "instance/updates",
  Settings: "settings",
  Account: "account",
};

export function routeFromLocation(pathname: string): PortalRoute | null {
  const normalized = pathname.replace(/^\/app\/?/u, "").replace(/\/$/u, "");
  if (normalized === "") return null;
  const matched = (Object.entries(routeSlug) as readonly (readonly [PortalRoute, string])[])
    .find(([, slug]) => normalized === slug || normalized.endsWith(`/${slug}`));
  return matched?.[0] ?? null;
}

export function pathForRoute(route: PortalRoute, projectId: string, organizationId?: string): string {
  const slug = routeSlug[route];
  if (["Instance", "Instance Billing", "Instance Users", "Updates", "Settings", "Account"].includes(route)) return `/app/${slug}`;
  if (["Projects", "Members", "Usage"].includes(route)) return `/app/organizations/${organizationId ?? "current"}/${slug}`;
  return `/app/projects/${projectId || "current"}/${slug}`;
}

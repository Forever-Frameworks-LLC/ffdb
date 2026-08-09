import { describe, expect, it } from "vitest";

import { navigationGroups, pathForRoute, portalRoutes, routeFromLocation, type PortalRoute } from "./data.js";

describe("portal route model", () => {
  it("round-trips every navigation item through a stable app URL", () => {
    const routes = navigationGroups.flatMap((group) => group.items.map((item) => item.label));
    for (const route of routes) {
      expect(routeFromLocation(pathForRoute(route, "project_123", "org_123"))).toBe(route);
    }
  });

  it("round-trips the complete route inventory, including Account", () => {
    for (const route of portalRoutes) {
      expect(routeFromLocation(pathForRoute(route, "project_123", "org_123"))).toBe(route);
    }
  });

  it("keeps administration, organization, and project routes in distinct scopes", () => {
    expect(pathForRoute("Instance Billing", "project_123", "org_123")).toBe("/app/instance/billing");
    expect(pathForRoute("Members", "project_123", "org_123")).toBe("/app/organizations/org_123/members");
    expect(pathForRoute("Database", "project_123", "org_123")).toBe("/app/projects/project_123/database");
  });

  it("keeps project observability beside the other workspace destinations", () => {
    expect(navigationGroups.find((group) => group.label === "Workspace")?.items.map((item) => item.label))
      .toEqual(["Overview", "Projects", "Members", "Observability"]);
    expect(navigationGroups.find((group) => group.label === "Operate")?.items.map((item) => item.label))
      .toEqual(["Activity", "Backups", "Usage"]);
  });

  it("recognizes account and focused commerce routes", () => {
    const examples: readonly (readonly [string, PortalRoute])[] = [
      ["/app/account", "Account"],
      ["/app/projects/p_1/products", "Products"],
      ["/app/projects/p_1/orders", "Orders"],
      ["/app/projects/p_1/subscriptions", "Subscriptions"],
    ];
    for (const [path, route] of examples) expect(routeFromLocation(path)).toBe(route);
  });
});

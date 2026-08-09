import { beforeEach, describe, expect, it } from "vitest";

import {
  clearPortalProjectKey,
  forgetPortalInstance,
  persistPortalInstance,
  persistPortalProject,
  portalConfiguration,
  portalInstanceNamespace,
  portalInstances,
  portalProjectKey,
  selectPortalInstance,
} from "./config.js";

describe("multi-instance portal configuration", () => {
  beforeEach(() => {
    localStorage.clear();
    sessionStorage.clear();
  });

  it("isolates active projects and credentials by API origin", () => {
    const first = { apiUrl: "http://127.0.0.1:5173", instanceName: "Local" };
    const second = { apiUrl: "https://managed.example.test", instanceName: "Managed" };
    persistPortalInstance(first);
    persistPortalInstance(second);
    persistPortalProject("project_local", "secret-local", "Local Org", "org_local", "Local Project", first.apiUrl);
    persistPortalProject("project_managed", "secret-managed", "Managed Org", "org_managed", "Managed Project", second.apiUrl);

    selectPortalInstance(first);
    const local = portalConfiguration({} as ImportMetaEnv);
    selectPortalInstance(second);
    const managed = portalConfiguration({} as ImportMetaEnv);

    expect(local).toMatchObject({ projectId: "project_local", developerKey: "secret-local", organizationId: "org_local" });
    expect(managed).toMatchObject({ projectId: "project_managed", developerKey: "secret-managed", organizationId: "org_managed" });
    expect(portalInstanceNamespace(first.apiUrl)).not.toBe(portalInstanceNamespace(second.apiUrl));
  });

  it("restores a scoped credential when switching between projects in one instance", () => {
    const apiUrl = "http://127.0.0.1:5173";
    persistPortalProject("project_atlas", "secret-atlas", "Northstar", "org_1", "Atlas", apiUrl);
    persistPortalProject("project_beacon", "secret-beacon", "Northstar", "org_1", "Beacon", apiUrl);

    persistPortalProject("project_atlas", undefined, "Northstar", "org_1", "Atlas", apiUrl);
    const atlas = portalConfiguration({ VITE_FFDB_API_URL: apiUrl } as ImportMetaEnv);

    expect(portalProjectKey(apiUrl, "project_atlas")).toBe("secret-atlas");
    expect(portalProjectKey(apiUrl, "project_beacon")).toBe("secret-beacon");
    expect(atlas).toMatchObject({ projectId: "project_atlas", developerKey: "secret-atlas", projectName: "Atlas" });
  });

  it("lets an explicit build-time API override win over a saved instance", () => {
    selectPortalInstance({ apiUrl: "https://saved.example.test", instanceName: "Saved" });
    const configured = portalConfiguration({ VITE_FFDB_API_URL: "http://127.0.0.1:5173" } as ImportMetaEnv);
    expect(configured.apiUrl).toBe("http://127.0.0.1:5173");
    expect(configured.instanceName).toBe("Local development");
  });

  it("deduplicates saved origins while retaining their friendly names", () => {
    persistPortalInstance({ apiUrl: "https://one.example.test/", instanceName: "One" });
    persistPortalInstance({ apiUrl: "https://one.example.test", instanceName: "Renamed" });
    expect(portalInstances()).toEqual([{ apiUrl: "https://one.example.test", instanceName: "Renamed" }]);
  });

  it("clears only the selected project credential", () => {
    const apiUrl = "https://one.example.test";
    persistPortalProject("project-one", "secret-one", "One", "org-one", "One", apiUrl);
    persistPortalProject("project-two", "secret-two", "One", "org-one", "Two", apiUrl);

    clearPortalProjectKey(apiUrl, "project-one");

    expect(portalProjectKey(apiUrl, "project-one")).toBeUndefined();
    expect(portalProjectKey(apiUrl, "project-two")).toBe("secret-two");
  });

  it("forgets one instance and its browser-local namespace without touching another", () => {
    const first = { apiUrl: "https://one.example.test", instanceName: "One" };
    const second = { apiUrl: "https://two.example.test", instanceName: "Two" };
    persistPortalInstance(first);
    persistPortalInstance(second);
    persistPortalProject("project-one", "secret-one", "One", "org-one", "One", first.apiUrl);
    persistPortalProject("project-two", "secret-two", "Two", "org-two", "Two", second.apiUrl);

    forgetPortalInstance(first.apiUrl);

    expect(portalInstances()).toEqual([second]);
    expect(portalProjectKey(first.apiUrl, "project-one")).toBeUndefined();
    expect(portalProjectKey(second.apiUrl, "project-two")).toBe("secret-two");
  });
});

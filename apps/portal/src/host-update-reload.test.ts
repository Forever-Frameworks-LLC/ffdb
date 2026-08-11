import { describe, expect, it } from "vitest";

import { hostUpdateReloadResult, portalUrlAfterHostUpdate } from "./host-update-reload.js";

describe("portal reloads after host release changes", () => {
  it("preserves the active route and unrelated state in a one-time install URL", () => {
    const href = portalUrlAfterHostUpdate(
      "https://ffdb.example.test/app/instance/updates?panel=history#latest",
      "install",
      "0.3.13",
    );
    const url = new URL(href);

    expect(url.pathname).toBe("/app/instance/updates");
    expect(url.searchParams.get("panel")).toBe("history");
    expect(url.searchParams.get("ffdb-host-update")).toBe("installed");
    expect(url.searchParams.get("ffdb-host-version")).toBe("0.3.13");
    expect(url.hash).toBe("#latest");
  });

  it("restores the success notice and removes only the one-time marker", () => {
    const result = hostUpdateReloadResult(
      "https://ffdb.example.test/app/instance/updates?panel=history&ffdb-host-update=rolled-back&ffdb-host-version=0.3.11#latest",
    );

    expect(result).toEqual({
      cleanPath: "/app/instance/updates?panel=history#latest",
      message: "Rolled back to FFDB 0.3.11",
    });
  });

  it("scrubs an invalid marker without displaying attacker-controlled text", () => {
    const result = hostUpdateReloadResult(
      "https://ffdb.example.test/app/instance/updates?ffdb-host-update=unexpected&ffdb-host-version=%3Cscript%3E",
    );

    expect(result).toEqual({ cleanPath: "/app/instance/updates", message: null });
  });
});

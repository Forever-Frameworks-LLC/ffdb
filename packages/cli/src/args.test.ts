import { describe, expect, it } from "vitest";

import { parseArguments } from "./args.js";

describe("CLI argument parser", () => {
  it("extracts global credentials without consuming command arguments", () => {
    expect(parseArguments(["--url", "https://ffdb.test", "project", "list", "org-1", "--json", "--key", "secret"])).toEqual({
      options: { baseUrl: "https://ffdb.test", developerKey: "secret", json: true },
      command: ["project", "list", "org-1"],
    });
  });

  it("rejects a missing global option value", () => {
    expect(() => parseArguments(["schema", "--project"])).toThrow("--project value is required");
  });
});

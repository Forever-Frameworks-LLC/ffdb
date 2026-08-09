import { describe, expect, it } from "vitest";

import { highlightCode } from "./syntax";

describe("documentation syntax lexer", () => {
  it("recognizes SQL keywords, strings, and comments without changing source", () => {
    const source = "-- scoped query\nSELECT id FROM documents WHERE owner_id = 'user';";
    const tokens = highlightCode(source, "sql");
    expect(tokens.some((token) => token.kind === "comment")).toBe(true);
    expect(tokens.filter((token) => token.kind === "keyword").map((token) => token.value)).toContain("SELECT");
    expect(tokens.some((token) => token.kind === "string" && token.value === "'user'" )).toBe(true);
    expect(tokens.map((token) => token.value).join("")).toBe(source);
  });

  it("recognizes shell variables and systemd properties", () => {
    expect(highlightCode("echo $FFDB_PROJECT_ID", "sh").some((token) => token.kind === "variable")).toBe(true);
    expect(highlightCode("ExecStart=/usr/local/bin/ffdb-api", "systemd")[0]).toEqual({ kind: "property", value: "ExecStart" });
  });
});

import { describe, expect, it } from "vitest";

import { parseAuthAction, scrubbedAuthActionUrl } from "./AuthAction";

describe("public auth action routing", () => {
  it("parses verification and password-reset fragments without putting credentials in the HTTP URL", () => {
    const verification = parseAuthAction("#/auth/verify?project_id=project-1&token=ffdb_action_verify.secret&redirect_to=https%3A%2F%2Fapp.example.test%2Fauth%2Fcomplete%3Fsource%3Demail");
    const reset = parseAuthAction("#/auth/password-reset?project_id=project-2&token=ffdb_action_reset.secret");
    const native = parseAuthAction("#/auth/verify?project_id=project-3&token=ffdb_action_native.secret&redirect_to=ffdb-field-notes%3A%2F%2Fauth%2Fcallback");

    expect(verification).toEqual({ kind: "verify", projectId: "project-1", token: "ffdb_action_verify.secret", redirectTo: "https://app.example.test/auth/complete?source=email" });
    expect(reset).toEqual({ kind: "password-reset", projectId: "project-2", token: "ffdb_action_reset.secret" });
    expect(native).toEqual({ kind: "verify", projectId: "project-3", token: "ffdb_action_native.secret", redirectTo: "ffdb-field-notes://auth/callback" });
    expect(scrubbedAuthActionUrl(verification!, "/", "")).toBe("/#/auth/verify");
    expect(scrubbedAuthActionUrl(reset!, "/", "?source=email")).toBe("/?source=email#/auth/password-reset");
  });

  it("renders malformed auth links as invalid instead of falling through to the marketing page", () => {
    expect(parseAuthAction("#/auth/verify?project_id=project-1")).toEqual({ kind: "invalid" });
    expect(parseAuthAction("#/auth/unknown?project_id=project-1&token=value")).toEqual({ kind: "invalid" });
    expect(parseAuthAction("#capabilities")).toBeNull();
  });

  it("rejects control characters and oversized action values", () => {
    expect(parseAuthAction("#/auth/verify?project_id=project-1&token=value%0Ainjected")).toEqual({ kind: "invalid" });
    expect(parseAuthAction(`#/auth/verify?project_id=project-1&token=${"a".repeat(4_097)}`)).toEqual({ kind: "invalid" });
    expect(parseAuthAction("#/auth/verify?project_id=project-1&token=value&redirect_to=javascript%3Aalert%281%29")).toEqual({ kind: "invalid" });
    expect(parseAuthAction("#/auth/verify?project_id=project-1&token=value&redirect_to=https%3A%2F%2Fuser%3Asecret%40app.example.test")).toEqual({ kind: "invalid" });
    expect(parseAuthAction("#/auth/verify?project_id=project-1&token=value&redirect_to=data%3A%2F%2Fauth%2Fcallback")).toEqual({ kind: "invalid" });
  });
});

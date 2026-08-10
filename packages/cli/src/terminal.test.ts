import { describe, expect, it } from "vitest";

import {
  collectLoginCredentials,
  loginRequirementMessage,
  renderError,
  renderHelp,
  renderHumanResult,
  supportsColor,
} from "./terminal.js";

describe("CLI terminal experience", () => {
  it("renders a readable plain help screen with ASCII branding", () => {
    const output = renderHelp("Usage:\n  ffdb login  Sign in securely", false);
    expect(output).toContain("███████╗███████╗");
    expect(output).toContain("Usage:");
    expect(output).toContain("ffdb login  Sign in securely");
    expect(output).not.toContain("\u001b[");
  });

  it("adds terminal color only when enabled", () => {
    expect(renderHelp("Usage:\n  ffdb login  Sign in", true)).toContain("\u001b[");
    expect(supportsColor({ isTTY: true }, {})).toBe(true);
    expect(supportsColor({ isTTY: true }, { NO_COLOR: "1" })).toBe(false);
    expect(supportsColor({ isTTY: false }, { FORCE_COLOR: "1" })).toBe(true);
  });

  it("explains every missing login requirement and both supported paths", async () => {
    const message = loginRequirementMessage({});
    expect(message).toContain("email and password");
    expect(message).toContain("Interactive:  ffdb login");
    expect(message).toContain("FFDB_PASSWORD");
    await expect(collectLoginCredentials({}, false)).rejects.toThrow(message);
  });

  it("accepts complete automation credentials without prompting", async () => {
    await expect(collectLoginCredentials({ email: "dev@example.com", password: "secret" }, false)).resolves.toEqual({
      email: "dev@example.com",
      password: "secret",
    });
  });

  it("formats human results and errors without leaking JSON ceremony", () => {
    const result = renderHumanResult({ status: "authenticated", email: "dev@example.com", project_id: null }, false);
    expect(result).toContain("✓ Authenticated");
    expect(result).toContain("Email");
    expect(result).toContain("dev@example.com");
    expect(result).toContain("Project id");
    expect(renderError("Login requires a password.\n\n  Interactive: ffdb login", false)).toContain("✖ Login requires a password.");
  });
});

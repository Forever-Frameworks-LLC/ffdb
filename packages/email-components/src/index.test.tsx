import { render } from "@react-email/render";
import { describe, expect, it } from "vitest";

import { PasswordResetEmail, templateManifest } from "./index.js";

describe("email components", () => {
  it("renders safe React Email HTML and declares every default kind", async () => {
    const html = await render(
      <PasswordResetEmail
        projectName={'Atlas <script>alert("x")</script>'}
        actionUrl="https://example.test/reset"
        expiresIn="30 minutes"
      />,
    );
    expect(html).toContain("Reset your password");
    expect(html).not.toContain("<script>alert");
    expect(templateManifest).toHaveLength(5);
  });
});

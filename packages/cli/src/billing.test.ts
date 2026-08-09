import { describe, expect, it } from "vitest";

import { parsePaidBillingTier } from "./billing.js";

describe("billing CLI input", () => {
  it("accepts only paid checkout tiers", () => {
    expect(parsePaidBillingTier("pay_as_you_go")).toBe("pay_as_you_go");
    expect(parsePaidBillingTier("pro")).toBe("pro");
    expect(() => parsePaidBillingTier("free")).toThrow(/pay_as_you_go or pro/u);
  });
});

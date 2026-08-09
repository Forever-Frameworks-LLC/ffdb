import type { PlatformBillingTier } from "@ffdb/client";

export function parsePaidBillingTier(value: string | undefined): Exclude<PlatformBillingTier, "free"> {
  if (value === "pay_as_you_go" || value === "pro") return value;
  throw new Error("billing tier must be pay_as_you_go or pro");
}

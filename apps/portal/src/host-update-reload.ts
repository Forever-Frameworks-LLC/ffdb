import type { HostUpdateOperation } from "@ffdb/client";

const resultParameter = "ffdb-host-update";
const versionParameter = "ffdb-host-version";

type ReleaseChangeOperation = Extract<HostUpdateOperation, "install" | "rollback">;

export interface HostUpdateReloadResult {
  readonly cleanPath: string;
  readonly message: string | null;
}

export function portalUrlAfterHostUpdate(
  href: string,
  operation: ReleaseChangeOperation,
  version: string | null,
): string {
  const url = new URL(href);
  url.searchParams.set(resultParameter, operation === "install" ? "installed" : "rolled-back");
  if (version === null || version.trim() === "") url.searchParams.delete(versionParameter);
  else url.searchParams.set(versionParameter, version.trim());
  return url.href;
}

export function hostUpdateReloadResult(href: string): HostUpdateReloadResult | null {
  const url = new URL(href);
  const result = url.searchParams.get(resultParameter);
  if (result === null) return null;

  const version = safeVersion(url.searchParams.get(versionParameter));
  url.searchParams.delete(resultParameter);
  url.searchParams.delete(versionParameter);

  const message = result === "installed"
    ? `Updated to FFDB ${version ?? "the selected release"}`
    : result === "rolled-back"
      ? `Rolled back to FFDB ${version ?? "the selected release"}`
      : null;

  return {
    cleanPath: `${url.pathname}${url.search}${url.hash}`,
    message,
  };
}

export function reloadPortalAfterHostUpdate(
  operation: ReleaseChangeOperation,
  version: string | null,
): void {
  globalThis.location.replace(portalUrlAfterHostUpdate(globalThis.location.href, operation, version));
}

function safeVersion(value: string | null): string | null {
  if (value === null) return null;
  const normalized = value.trim();
  return /^[0-9A-Za-z][0-9A-Za-z.+-]{0,63}$/u.test(normalized) ? normalized : null;
}

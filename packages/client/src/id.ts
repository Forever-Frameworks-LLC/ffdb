const ID_PREFIX = /^[A-Za-z0-9_-]{0,32}$/u;

/** Generates a cryptographically random UUID suitable for client-created rows. */
export function generateId(prefix = ""): string {
  if (!ID_PREFIX.test(prefix)) {
    throw new TypeError("ID prefix must contain at most 32 letters, numbers, underscores, or hyphens");
  }
  const cryptoApi = globalThis.crypto;
  if (typeof cryptoApi?.randomUUID !== "function") {
    throw new Error("Secure random UUID generation is unavailable in this runtime");
  }
  return `${prefix}${cryptoApi.randomUUID()}`;
}

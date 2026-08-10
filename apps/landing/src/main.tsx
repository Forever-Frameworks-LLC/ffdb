import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import { AuthActionPage, parseAuthAction, scrubbedAuthActionUrl } from "./AuthAction";
import "./styles.css";
import "./auth-action.css";

const root = document.getElementById("root");
if (root === null) throw new Error("Missing application root");

const authAction = parseAuthAction(globalThis.location.hash);
if (authAction !== null) {
  globalThis.history.replaceState({}, "", scrubbedAuthActionUrl(authAction, globalThis.location.pathname, globalThis.location.search));
}

function LandingRouter({ initialAction }: { readonly initialAction: ReturnType<typeof parseAuthAction> }) {
  const [route, setRoute] = useState({ action: initialAction, revision: 0 });

  useEffect(() => {
    const handleHashChange = () => {
      const nextAction = parseAuthAction(globalThis.location.hash);
      if (nextAction !== null) {
        globalThis.history.replaceState({}, "", scrubbedAuthActionUrl(nextAction, globalThis.location.pathname, globalThis.location.search));
      }
      setRoute((current) => ({ action: nextAction, revision: current.revision + 1 }));
    };

    globalThis.addEventListener("hashchange", handleHashChange);
    return () => globalThis.removeEventListener("hashchange", handleHashChange);
  }, []);

  // Verification consumes a one-time credential, so the action page stays
  // outside StrictMode's development-only double-effect cycle. The revision
  // also resets request state when another emailed link opens in the same tab.
  return route.action === null
    ? <StrictMode><App /></StrictMode>
    : <AuthActionPage key={route.revision} action={route.action} apiUrl={import.meta.env.VITE_FFDB_API_URL ?? globalThis.location.origin} />;
}

createRoot(root).render(<LandingRouter initialAction={authAction} />);

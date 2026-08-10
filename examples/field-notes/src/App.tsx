import { useAuth } from "@ffdb/react";

import { AuthScreen } from "./AuthScreen";
import { FieldNotesApp } from "./FieldNotesApp";

export function App() {
  const auth = useAuth();
  if (auth.status === "loading") {
    return <div className="app-loading"><span className="spinner" /> Restoring your FFDB session…</div>;
  }
  if (auth.status !== "authenticated" || auth.session === null) return <AuthScreen />;
  return <FieldNotesApp session={auth.session} />;
}

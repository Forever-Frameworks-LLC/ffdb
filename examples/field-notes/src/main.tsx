import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { AuthProvider, FFDBProvider } from "@ffdb/react";

import { App } from "./App";
import { configurationError, ffdb } from "./ffdb";
import "./styles.css";

const root = createRoot(document.getElementById("root")!);

root.render(
  <StrictMode>
    {ffdb === null ? (
      <main className="configuration-error">
        <div className="brand"><span className="brand-mark">F</span> FFDB Field Notes</div>
        <h1>Connect a project first.</h1>
        <p>{configurationError}</p>
        <code>cp .env.example .env.local</code>
      </main>
    ) : (
      <FFDBProvider client={ffdb}>
        <AuthProvider><App /></AuthProvider>
      </FFDBProvider>
    )}
  </StrictMode>,
);

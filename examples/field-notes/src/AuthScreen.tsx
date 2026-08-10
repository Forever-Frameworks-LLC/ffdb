import { useEffect, useState, type FormEvent } from "react";
import { ArrowRight, Check, Database, Eye, EyeOff, Info, ShieldCheck } from "lucide-react";
import { useAuth, useFFDB } from "@ffdb/react";

import { ffdbProjectId } from "./ffdb";

type Mode = "sign-in" | "register" | "verify" | "reset";

export function AuthScreen() {
  const client = useFFDB();
  const auth = useAuth();
  const actionResult = new URLSearchParams(location.search).get("ffdb_auth");
  const [mode, setMode] = useState<Mode>(() => new URLSearchParams(location.search).has("verification_token") ? "verify" : "sign-in");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [token, setToken] = useState(() => new URLSearchParams(location.search).get("verification_token") ?? "");
  const [showPassword, setShowPassword] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(() => actionResult === "verified" ? "Email verified. Sign in to enter the workspace." : actionResult === "password-reset" ? "Password updated. Sign in with your new password." : null);
  const [connection, setConnection] = useState<"checking" | "connected" | "error">("checking");

  useEffect(() => {
    void client.readiness().then(() => setConnection("connected"), () => setConnection("error"));
  }, [client]);

  useEffect(() => {
    if (actionResult === null) return;
    const clean = new URL(location.href);
    clean.searchParams.delete("ffdb_auth");
    history.replaceState({}, "", clean);
  }, [actionResult]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setMessage(null);
    try {
      if (mode === "register") {
        const result = await client.auth.register({ email, password, redirect_to: authReturnUrl("verified") });
        if (result.verification_required) {
          setMode("verify");
          setMessage("Account created. Open the verification email and you’ll return here automatically.");
        } else {
          await auth.signIn(email, password);
        }
      } else if (mode === "verify") {
        await client.auth.verifyEmail(token.trim());
        setMode("sign-in");
        setMessage("Email verified. Sign in to enter the workspace.");
      } else if (mode === "reset") {
        await client.auth.startPasswordReset(email, { redirectTo: authReturnUrl("password-reset") });
        setMessage("If the account exists, FFDB queued a password-reset email.");
      } else {
        await auth.signIn(email, password);
      }
    } catch (cause) {
      setMessage(cause instanceof Error ? cause.message : "FFDB could not complete the request.");
    } finally {
      setBusy(false);
    }
  }

  const isPasswordMode = mode === "sign-in" || mode === "register";
  const primaryLabel = mode === "register" ? "Create account" : mode === "verify" ? "Verify email" : mode === "reset" ? "Send reset email" : "Enter workspace";

  return (
    <div className="auth-page">
      <header className="auth-header">
        <div className="brand"><span className="brand-mark">F</span> FFDB Field Notes</div>
        <span>Designed for field work. Built for data.</span>
      </header>
      <section className="auth-story">
        <div>
          <h1>Bring your own<br />field notes.</h1>
          <span className="story-rule" />
          <p>Sign in to test authentication, RLS, offline sync, storage, and sessions against your FFDB project.</p>
        </div>
        <div className="schema-sketch" aria-hidden="true">
          <pre>{`01  -- FFDB project schema (excerpt)\n02\n03  create table field_tasks (\n04    id          text primary key,\n05    owner_id    text not null,\n06    title       text not null,\n07    updated_at  integer not null\n08  );\n09\n10  alter table field_tasks enable\n11  row level security;\n12\n13  -- Policies enforce owner scope\n14  -- Secrets remain server-side`}</pre>
          <div className="schema-node"><strong>field_tasks</strong><span>id · text</span><span>owner_id · text</span><span>title · text</span></div>
        </div>
      </section>
      <section className="auth-panel">
        <div className="auth-tabs" role="tablist" aria-label="Authentication mode">
          <button className={mode === "sign-in" ? "active" : ""} onClick={() => { setMode("sign-in"); setMessage(null); }}>Sign in</button>
          <button className={mode === "register" ? "active" : ""} onClick={() => { setMode("register"); setMessage(null); }}>Create account</button>
        </div>
        <form onSubmit={(event) => void submit(event)}>
          {mode === "verify" ? (
            <label>Verification token<input value={token} onChange={(event) => setToken(event.target.value)} autoComplete="one-time-code" required /></label>
          ) : (
            <label>Email<input type="email" value={email} onChange={(event) => setEmail(event.target.value)} autoComplete="email" placeholder="field.researcher@example.com" required /></label>
          )}
          {isPasswordMode && (
            <label>Password<span className="password-field"><input type={showPassword ? "text" : "password"} value={password} onChange={(event) => setPassword(event.target.value)} autoComplete={mode === "register" ? "new-password" : "current-password"} minLength={12} required /><button type="button" onClick={() => setShowPassword((value) => !value)} aria-label={showPassword ? "Hide password" : "Show password"}>{showPassword ? <EyeOff /> : <Eye />}<span>{showPassword ? "Hide" : "Show"}</span></button></span></label>
          )}
          {mode === "sign-in" && <button type="button" className="text-action forgot" onClick={() => setMode("reset")}>Forgot password?</button>}
          {message !== null && <div className="auth-message" role="status"><Info />{message}</div>}
          {auth.error !== null && message === null && <div className="auth-message error" role="alert"><Info />{auth.error.message}</div>}
          <button className="primary wide" disabled={busy}>{busy ? <span className="spinner" /> : null}{primaryLabel}<ArrowRight /></button>
          {mode !== "sign-in" && <button type="button" className="secondary wide" onClick={() => setMode("sign-in")}>Back to sign in</button>}
          {mode === "sign-in" && <button type="button" className="secondary wide" onClick={() => setMode("register")}>Create an account</button>}
        </form>
        <div className="connection-card">
          <Database />
          <div><strong>FFDB project</strong><span>{connection === "checking" ? "Checking…" : connection === "connected" ? "Connected" : "Unavailable"}</span></div>
          {connection === "connected" ? <Check className="connection-ok" /> : <span className={`status-dot ${connection}`} />}
          <small>{ffdbProjectId.slice(0, 18)}{ffdbProjectId.length > 18 ? "…" : ""}</small>
        </div>
        {mode !== "verify" && <button className="verify-callout" onClick={() => setMode("verify")}><Info /><span>Registration requires email verification.<strong>Verify email <ArrowRight /></strong></span></button>}
      </section>
      <footer className="auth-footer"><ShieldCheck /> Developer keys stay server-side.</footer>
    </div>
  );
}

function authReturnUrl(result: "verified" | "password-reset"): string {
  const url = new URL(location.href);
  url.searchParams.delete("verification_token");
  url.searchParams.set("ffdb_auth", result);
  url.hash = "";
  return url.href;
}

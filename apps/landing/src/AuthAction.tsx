import { useCallback, useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";

import { FFDBClient, FFDBError } from "@ffdb/client";

export type AuthAction =
  | { readonly kind: "verify"; readonly projectId: string; readonly token: string; readonly redirectTo?: string }
  | { readonly kind: "password-reset"; readonly projectId: string; readonly token: string; readonly redirectTo?: string }
  | { readonly kind: "invalid" };

type RequestState = "idle" | "working" | "success" | "invalid" | "retryable";

export function parseAuthAction(hash: string): AuthAction | null {
  const fragment = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!fragment.startsWith("/auth/")) return null;
  const [route = "", query = ""] = fragment.split("?", 2);
  if (route !== "/auth/verify" && route !== "/auth/password-reset") return { kind: "invalid" };
  const parameters = new URLSearchParams(query);
  const projectId = parameters.get("project_id")?.trim() ?? "";
  const token = parameters.get("token")?.trim() ?? "";
  const redirectTo = parameters.get("redirect_to") ?? undefined;
  if (!validActionValue(projectId, 128) || !validActionValue(token, 4_096) || !validReturnUrl(redirectTo)) return { kind: "invalid" };
  return {
    kind: route === "/auth/verify" ? "verify" : "password-reset",
    projectId,
    token,
    ...(redirectTo === undefined ? {} : { redirectTo }),
  };
}

export function scrubbedAuthActionUrl(action: AuthAction, pathname: string, search: string): string {
  const route = action.kind === "verify" ? "verify" : action.kind === "password-reset" ? "password-reset" : "error";
  return `${pathname}${search}#/auth/${route}`;
}

function validActionValue(value: string, maximum: number): boolean {
  return value.length > 0 && value.length <= maximum && !/[\u0000-\u001f\u007f]/u.test(value);
}

function validReturnUrl(value: string | undefined): boolean {
  if (value === undefined) return true;
  if (value.length === 0 || value.length > 2_048 || value.trim() !== value || /[\u0000-\u001f\u007f]/u.test(value)) return false;
  try {
    const url = new URL(value);
    return url.hostname !== "" && url.username === "" && url.password === "" && !unsafeAuthRedirectProtocols.has(url.protocol);
  } catch {
    return false;
  }
}

const unsafeAuthRedirectProtocols = new Set([
  "about:", "blob:", "chrome:", "chrome-extension:", "data:", "file:", "filesystem:", "ftp:",
  "intent:", "javascript:", "mailto:", "resource:", "sms:", "tel:", "vbscript:", "view-source:", "ws:", "wss:",
]);

export function AuthActionPage({ action, apiUrl }: { readonly action: AuthAction; readonly apiUrl: string }) {
  return (
    <main className="auth-action-shell">
      <div className="auth-action-grid" aria-hidden="true" />
      <div className="auth-action-glow" aria-hidden="true" />
      <header className="auth-action-header">
        <span className="auth-action-brand">FFDB</span>
        <span><ShieldIcon /> Secure account action</span>
      </header>
      <div className="auth-action-stage">
        {action.kind === "verify" ? <VerificationAction action={action} apiUrl={apiUrl} /> : action.kind === "password-reset" ? <PasswordResetAction action={action} apiUrl={apiUrl} /> : <InvalidAction />}
      </div>
      <footer className="auth-action-footer"><LockIcon /><span>One-time credentials stay in the browser fragment and are removed from the address bar before verification.</span></footer>
    </main>
  );
}

function VerificationAction({ action, apiUrl }: { readonly action: Extract<AuthAction, { kind: "verify" }>; readonly apiUrl: string }) {
  const client = useMemo(() => new FFDBClient({ baseUrl: apiUrl, projectId: action.projectId }), [action.projectId, apiUrl]);
  const [state, setState] = useState<RequestState>("working");
  const [redirectTo, setRedirectTo] = useState<string | null>(null);

  useAppRedirect(state === "success" ? redirectTo : null);

  const verify = useCallback(async (signal?: AbortSignal) => {
    setState("working");
    try {
      const result = await client.auth.verifyEmail(action.token, {
        ...(signal === undefined ? {} : { signal }),
        ...(action.redirectTo === undefined ? {} : { redirectTo: action.redirectTo }),
      });
      setRedirectTo(result.redirect_to);
      setState("success");
    } catch (cause) {
      if (signal?.aborted === true) return;
      setState(isInvalidActionError(cause) ? "invalid" : "retryable");
    }
  }, [action.redirectTo, action.token, client]);

  useEffect(() => {
    const controller = new AbortController();
    void verify(controller.signal);
    return () => controller.abort();
  }, [verify]);

  if (state === "working") return <ActionCard tone="working" icon={<SpinnerIcon />} eyebrow="Email verification" title="Finishing your setup…" detail={action.redirectTo === undefined ? "Securely verifying this one-time link. Keep this tab open for a moment." : `Securely verifying this one-time link. You’ll return to ${returnLabel(action.redirectTo)} automatically.`}><ProgressSteps active={1} /></ActionCard>;
  if (state === "success" && redirectTo !== null) return <RedirectingCard eyebrow="Email verified" title="You’re all set." detail={`Returning you to ${returnLabel(redirectTo)}…`} redirectTo={redirectTo}><ProgressSteps active={2} /></RedirectingCard>;
  if (state === "success") return <ActionCard tone="success" icon={<CheckIcon />} eyebrow="Verification complete" title="Your email is verified." detail="You can safely close this tab and return to the application where you created your account."><ProgressSteps active={2} /><CloseHint /></ActionCard>;
  if (state === "invalid") return <ActionCard tone="error" icon={<AlertIcon />} eyebrow="Link unavailable" title="This verification link can’t be used." detail="It may have expired, already been used, or been replaced by a newer verification email."><p className="auth-action-help">Close this tab, return to the application, and request a fresh verification email.</p></ActionCard>;
  return <ActionCard tone="error" icon={<AlertIcon />} eyebrow="Connection interrupted" title="We couldn’t verify your email yet." detail="The FFDB service could not be reached. Your link may still be valid, so you can safely try again."><button className="auth-action-primary" type="button" onClick={() => void verify()}>Try verification again</button></ActionCard>;
}

function PasswordResetAction({ action, apiUrl }: { readonly action: Extract<AuthAction, { kind: "password-reset" }>; readonly apiUrl: string }) {
  const client = useMemo(() => new FFDBClient({ baseUrl: apiUrl, projectId: action.projectId }), [action.projectId, apiUrl]);
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [state, setState] = useState<RequestState>("idle");
  const [formError, setFormError] = useState<string | null>(null);
  const [redirectTo, setRedirectTo] = useState<string | null>(null);

  useAppRedirect(state === "success" ? redirectTo : null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (password !== confirmation) {
      setFormError("Passwords do not match.");
      return;
    }
    setFormError(null);
    setState("working");
    try {
      const result = await client.auth.completePasswordReset(action.token, password, action.redirectTo === undefined ? {} : { redirectTo: action.redirectTo });
      setRedirectTo(result.redirect_to);
      setState("success");
    } catch (cause) {
      setState(isInvalidActionError(cause) ? "invalid" : "retryable");
    } finally {
      setPassword("");
      setConfirmation("");
    }
  };

  if (state === "success" && redirectTo !== null) return <RedirectingCard eyebrow="Password updated" title="Your new password is ready." detail={`Returning you to ${returnLabel(redirectTo)}…`} redirectTo={redirectTo} />;
  if (state === "success") return <ActionCard tone="success" icon={<CheckIcon />} eyebrow="Password updated" title="Your new password is ready." detail="You can safely close this tab and return to the application to sign in."><CloseHint /></ActionCard>;
  if (state === "invalid") return <ActionCard tone="error" icon={<AlertIcon />} eyebrow="Link unavailable" title="This reset link can’t be used." detail="It may have expired, already been used, or been replaced by a newer password-reset email."><p className="auth-action-help">Close this tab, return to the application, and request a fresh password-reset link.</p></ActionCard>;

  return <ActionCard tone={state === "retryable" ? "error" : "neutral"} icon={<KeyIcon />} eyebrow="Password recovery" title="Choose a new password." detail={state === "retryable" ? "FFDB could not update your password. Check your connection and try again." : "Use at least 12 characters. Your password is sent only to this FFDB deployment over the current secure connection."}>
    <form className="auth-action-form" onSubmit={(event) => void submit(event)}>
      <label><span>New password</span><input autoComplete="new-password" disabled={state === "working"} minLength={12} required type="password" value={password} onChange={(event) => setPassword(event.target.value)} /></label>
      <label><span>Confirm new password</span><input autoComplete="new-password" disabled={state === "working"} minLength={12} required type="password" value={confirmation} onChange={(event) => setConfirmation(event.target.value)} /></label>
      {formError === null ? null : <p className="auth-action-form-error" role="alert">{formError}</p>}
      <button className="auth-action-primary" disabled={state === "working" || password.length < 12 || confirmation.length < 12} type="submit">{state === "working" ? "Updating password…" : state === "retryable" ? "Try again" : "Update password"}</button>
    </form>
  </ActionCard>;
}

function InvalidAction() {
  return <ActionCard tone="error" icon={<AlertIcon />} eyebrow="Invalid account link" title="This link is incomplete." detail="The project identifier, one-time credential, or return destination is invalid. For your security, FFDB did not make an authentication request."><CloseHint /></ActionCard>;
}

function ActionCard({ children, detail, eyebrow, icon, title, tone }: { readonly children?: ReactNode; readonly detail: string; readonly eyebrow: string; readonly icon: ReactNode; readonly title: string; readonly tone: "neutral" | "working" | "success" | "error" }) {
  return <section className={`auth-action-card is-${tone}`} aria-live="polite"><span className="auth-action-icon">{icon}</span><span className="auth-action-eyebrow">{eyebrow}</span><h1>{title}</h1><p className="auth-action-detail">{detail}</p>{children}</section>;
}

function ProgressSteps({ active }: { readonly active: 1 | 2 }) {
  return <ol className="auth-action-progress" aria-label="Verification progress"><li className="is-complete"><span><CheckIcon /></span>Link received</li><li className={active === 2 ? "is-complete" : "is-active"}><span>{active === 2 ? <CheckIcon /> : "2"}</span>Email verified</li></ol>;
}

function RedirectingCard({ children, detail, eyebrow, redirectTo, title }: { readonly children?: ReactNode; readonly detail: string; readonly eyebrow: string; readonly redirectTo: string; readonly title: string }) {
  return <ActionCard tone="success" icon={<CheckIcon />} eyebrow={eyebrow} title={title} detail={detail}>{children}<div className="auth-action-redirect" aria-label="Returning to application"><span /></div><a className="auth-action-return" href={redirectTo}>Return now</a></ActionCard>;
}

function CloseHint() {
  return <p className="auth-action-close-hint">This page is only a secure handoff. It does not lead to the FFDB dashboard or website.</p>;
}

function useAppRedirect(redirectTo: string | null): void {
  useEffect(() => {
    if (redirectTo === null) return;
    const reducedMotion = globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;
    const timeout = globalThis.setTimeout(() => globalThis.location.replace(redirectTo), reducedMotion ? 250 : 1_100);
    return () => globalThis.clearTimeout(timeout);
  }, [redirectTo]);
}

function returnLabel(redirectTo: string): string {
  try {
    const url = new URL(redirectTo);
    return url.protocol === "http:" || url.protocol === "https:" ? url.host : `the ${url.protocol.slice(0, -1)} app`;
  }
  catch { return "your application"; }
}

function isInvalidActionError(cause: unknown): boolean {
  return cause instanceof FFDBError && (cause.status === 400 || cause.code === "auth.invalid_input");
}

function ShieldIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3 20 6v6c0 5-3 8-8 10-5-2-8-5-8-10V6l8-3Z" /><path d="m8.5 12 2.3 2.3 4.8-5" /></svg>; }
function LockIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="5" y="10" width="14" height="11" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>; }
function CheckIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 4 4L19 6" /></svg>; }
function AlertIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3 2.5 20h19L12 3Z" /><path d="M12 9v4m0 3h.01" /></svg>; }
function KeyIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="8" cy="15" r="4" /><path d="m11 12 9-9m-3 3 3 3m-6 0 3 3" /></svg>; }
function SpinnerIcon() { return <svg className="auth-action-spinner" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M12 3a9 9 0 0 1 9 9" /></svg>; }

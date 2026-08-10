import { emitKeypressEvents } from "node:readline";
import { createInterface } from "node:readline/promises";
import type { Readable, Writable } from "node:stream";

export interface PromptInput extends Readable {
  readonly isTTY?: boolean;
  readonly isRaw?: boolean;
  setRawMode?(mode: boolean): this;
}

export interface PromptOutput extends Writable {
  readonly isTTY?: boolean;
}

export interface PromptIO {
  readonly input: PromptInput;
  readonly output: PromptOutput;
}

export interface LoginInput {
  readonly email?: string;
  readonly password?: string;
}

export interface LoginCredentials {
  readonly email: string;
  readonly password: string;
}

const ESCAPE = "\u001b[";
const RESET = `${ESCAPE}0m`;
const ACCENT = `${ESCAPE}38;2;151;194;164m`;
const BRIGHT = `${ESCAPE}97m`;
const MUTED = `${ESCAPE}38;2;139;145;141m`;
const DANGER = `${ESCAPE}38;2;224;166;108m`;
const BOLD = `${ESCAPE}1m`;

const LOGO = String.raw`  ███████╗███████╗██████╗ ██████╗
  ██╔════╝██╔════╝██╔══██╗██╔══██╗
  █████╗  █████╗  ██║  ██║██████╔╝
  ██╔══╝  ██╔══╝  ██║  ██║██╔══██╗
  ██║     ██║     ██████╔╝██████╔╝
  ╚═╝     ╚═╝     ╚═════╝ ╚═════╝`;

export function supportsColor(
  stream: { readonly isTTY?: boolean },
  environment: Readonly<Record<string, string | undefined>> = process.env,
): boolean {
  if (environment.NO_COLOR !== undefined || environment.FORCE_COLOR === "0") return false;
  return stream.isTTY === true || environment.FORCE_COLOR !== undefined;
}

export function renderHelp(source: string, color: boolean): string {
  const logo = paint(LOGO, `${BOLD}${ACCENT}`, color);
  const body = source.trimEnd().split("\n").map((line) => {
    if (/^[A-Z][^:]{1,48}:$/u.test(line)) return paint(line, `${BOLD}${ACCENT}`, color);
    const command = /^(  )(.+?)( {2,})([^ ].*)$/u.exec(line);
    if (command !== null) {
      return `${command[1]}${paint(command[2] ?? "", `${BOLD}${BRIGHT}`, color)}${command[3]}${paint(command[4] ?? "", MUTED, color)}`;
    }
    if (line.startsWith("  ")) return paint(line, BRIGHT, color);
    if (line.startsWith("Tip:")) return paint(line, MUTED, color);
    return line;
  }).join("\n");
  return `\n${logo}\n\n${body}\n`;
}

export function renderError(message: string, color: boolean): string {
  const [first = "Unknown CLI failure", ...rest] = message.split("\n");
  const title = `${paint("✖", DANGER, color)} ${paint(first, BOLD, color)}`;
  return `\n${title}${rest.length === 0 ? "" : `\n${rest.map((line) => paint(line, MUTED, color)).join("\n")}`}\n\n`;
}

export function renderHumanResult(value: unknown, color: boolean): string {
  if (typeof value === "string") return `${value}\n`;
  if (Array.isArray(value)) {
    if (value.length === 0) return `${paint("✓", ACCENT, color)} No results.\n`;
    return `${paint("✓", ACCENT, color)} ${value.length} result${value.length === 1 ? "" : "s"}\n\n${value.map((item, index) => renderListItem(item, index, color)).join("\n")}\n`;
  }
  if (isRecord(value)) {
    const rows = Object.entries(value);
    const width = Math.min(26, Math.max(8, ...rows.map(([key]) => humanize(key).length)));
    const body = rows.map(([key, item]) => {
      const label = humanize(key).padEnd(width);
      return `  ${paint(label, MUTED, color)}  ${formatValue(item, color)}`;
    }).join("\n");
    return `${paint("✓", ACCENT, color)} ${paint(resultTitle(value), BOLD, color)}\n${body === "" ? "" : `\n${body}\n`}`;
  }
  return `${formatValue(value, color)}\n`;
}

export function loginRequirementMessage(input: LoginInput): string {
  const missingEmail = input.email?.trim() === undefined || input.email.trim() === "";
  const missingPassword = input.password === undefined || input.password === "";
  const requirement = missingEmail && missingPassword
    ? "Login requires an email and password."
    : missingEmail
      ? "Login requires an email address."
      : "Login requires a password.";
  return `${requirement}\n\n  Interactive:  ffdb login\n  Automation:   FFDB_PASSWORD='••••••••' ffdb login you@example.com`;
}

export async function collectLoginCredentials(
  input: LoginInput,
  interactive: boolean,
  io: PromptIO = { input: process.stdin, output: process.stderr },
): Promise<LoginCredentials> {
  let email = input.email?.trim() ?? "";
  let password = input.password ?? "";
  if (email !== "" && password !== "") return { email, password };
  if (!interactive || io.input.isTTY !== true || io.output.isTTY !== true) {
    throw new Error(loginRequirementMessage(input));
  }

  if (email === "") email = (await promptText("  Email     › ", io)).trim();
  if (password === "") password = await promptSecret("  Password  › ", io);
  if (email === "" || password === "") throw new Error(loginRequirementMessage({ email, password }));
  return { email, password };
}

export function loginIntro(baseUrl: string, color: boolean): string {
  return `\n${paint("Sign in to FFDB", `${BOLD}${ACCENT}`, color)}\n${paint(`Session endpoint: ${baseUrl}`, MUTED, color)}\n\n`;
}

async function promptText(label: string, io: PromptIO): Promise<string> {
  const prompt = createInterface({ input: io.input, output: io.output });
  try { return await prompt.question(label); }
  finally { prompt.close(); }
}

async function promptSecret(label: string, io: PromptIO): Promise<string> {
  if (io.input.setRawMode === undefined) throw new Error("Secure password input is unavailable in this terminal");
  io.output.write(label);
  emitKeypressEvents(io.input);
  const wasRaw = io.input.isRaw === true;
  const wasPaused = io.input.isPaused();
  io.input.setRawMode(true);
  io.input.resume();

  return await new Promise<string>((resolve, reject) => {
    let secret = "";
    const finish = (cause?: Error) => {
      io.input.removeListener("keypress", onKeypress);
      io.input.setRawMode?.(wasRaw);
      if (wasPaused) io.input.pause();
      io.output.write("\n");
      if (cause === undefined) resolve(secret);
      else reject(cause);
    };
    const onKeypress = (value: string | undefined, key: { readonly ctrl?: boolean; readonly name?: string }) => {
      if (key.ctrl === true && key.name === "c") {
        finish(new Error("Login cancelled"));
        return;
      }
      if (key.name === "return" || key.name === "enter") {
        finish();
        return;
      }
      if (key.name === "backspace") {
        if (secret.length > 0) {
          secret = [...secret].slice(0, -1).join("");
          io.output.write("\b \b");
        }
        return;
      }
      if (value === undefined || key.ctrl === true || key.name === "escape" || /[\u0000-\u001f\u007f]/u.test(value)) return;
      secret += value;
      io.output.write("•".repeat([...value].length));
    };
    io.input.on("keypress", onKeypress);
  });
}

function renderListItem(value: unknown, index: number, color: boolean): string {
  if (!isRecord(value)) return `  ${paint(`${index + 1}.`, MUTED, color)} ${formatValue(value, color)}`;
  const entries = Object.entries(value);
  const lead = entries.find(([key]) => ["name", "display_name", "id", "email", "status"].includes(key));
  const title = lead === undefined ? `Result ${index + 1}` : String(lead[1]);
  const details = entries.filter(([key]) => key !== lead?.[0]).map(([key, item]) => `     ${paint(`${humanize(key)}:`, MUTED, color)} ${formatValue(item, color)}`).join("\n");
  return `  ${paint(`${index + 1}.`, MUTED, color)} ${paint(title, BOLD, color)}${details === "" ? "" : `\n${details}`}`;
}

function resultTitle(value: Readonly<Record<string, unknown>>): string {
  if (typeof value.status === "string") return humanize(value.status);
  if (typeof value.path === "string") return "Complete";
  return "Done";
}

function formatValue(value: unknown, color: boolean): string {
  if (value === null) return paint("—", MUTED, color);
  if (value === true) return paint("yes", ACCENT, color);
  if (value === false) return paint("no", DANGER, color);
  if (typeof value === "number" || typeof value === "bigint") return String(value);
  if (typeof value === "string") return value;
  return JSON.stringify(value, null, 2).split("\n").join("\n    ");
}

function humanize(value: string): string {
  const words = value.replaceAll("_", " ");
  return `${words.charAt(0).toUpperCase()}${words.slice(1)}`;
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function paint(value: string, style: string, enabled: boolean): string {
  return enabled ? `${style}${value}${RESET}` : value;
}

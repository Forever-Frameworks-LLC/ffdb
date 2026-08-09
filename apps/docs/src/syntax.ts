export type SyntaxKind =
  | "plain"
  | "comment"
  | "string"
  | "keyword"
  | "number"
  | "function"
  | "variable"
  | "property"
  | "operator"
  | "punctuation"
  | "boolean"
  | "tag";

export interface SyntaxToken {
  readonly kind: SyntaxKind;
  readonly value: string;
}

const languageAliases: Readonly<Record<string, string>> = {
  bash: "sh",
  shell: "sh",
  typescript: "ts",
  javascript: "ts",
  js: "ts",
  systemd: "ini",
  service: "ini",
  env: "ini",
  yml: "yaml",
};

export const supportedLanguages = new Set(["env", "ini", "json", "nginx", "service", "sh", "sql", "systemd", "ts", "tsx", "yaml"]);

const keywords: Readonly<Record<string, ReadonlySet<string>>> = {
  ts: new Set(["as", "async", "await", "catch", "class", "const", "else", "export", "extends", "finally", "for", "from", "function", "if", "import", "in", "instanceof", "interface", "let", "new", "of", "readonly", "return", "throw", "try", "type", "typeof", "void", "while"]),
  tsx: new Set(["as", "async", "await", "catch", "class", "const", "else", "export", "extends", "finally", "for", "from", "function", "if", "import", "in", "interface", "let", "new", "of", "readonly", "return", "throw", "type"]),
  sql: new Set(["all", "alter", "and", "as", "begin", "by", "check", "commit", "create", "delete", "drop", "enable", "exists", "for", "force", "from", "group", "having", "in", "index", "insert", "into", "join", "limit", "not", "null", "on", "or", "order", "policy", "primary", "references", "returning", "rollback", "row", "select", "set", "table", "to", "transaction", "union", "unique", "update", "using", "values", "where", "with"]),
  sh: new Set(["case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local", "readonly", "set", "then", "until", "while"]),
  yaml: new Set(["apiVersion", "kind"]),
  ini: new Set(["true", "false", "yes", "no"]),
  nginx: new Set(["add_header", "location", "proxy_pass", "proxy_set_header", "return", "server", "try_files"]),
  json: new Set(),
};

const booleans = new Set(["true", "false", "null", "undefined"]);

/** Small deterministic lexer for the languages used in the public docs. */
export function highlightCode(code: string, requestedLanguage: string): readonly SyntaxToken[] {
  const language = languageAliases[requestedLanguage] ?? requestedLanguage;
  const output: SyntaxToken[] = [];
  let cursor = 0;
  while (cursor < code.length) {
    const source = code.slice(cursor);
    const comment = matchComment(source, language, cursor === 0 || code[cursor - 1] === "\n");
    if (comment !== null) {
      push(output, "comment", comment);
      cursor += comment.length;
      continue;
    }
    const string = matchString(source, language);
    if (string !== null) {
      push(output, "string", string);
      cursor += string.length;
      continue;
    }
    const variable = language === "sh" ? /^\$(?:\{[A-Za-z_][A-Za-z0-9_]*\}|[A-Za-z_][A-Za-z0-9_]*)/u.exec(source)?.[0] : undefined;
    if (variable !== undefined) {
      push(output, "variable", variable);
      cursor += variable.length;
      continue;
    }
    const tag = language === "tsx" ? /^<\/?[A-Za-z][A-Za-z0-9.]*/u.exec(source)?.[0] : undefined;
    if (tag !== undefined) {
      push(output, "tag", tag);
      cursor += tag.length;
      continue;
    }
    const number = /^(?:0x[\da-f]+|\d+(?:\.\d+)?)/iu.exec(source)?.[0];
    if (number !== undefined) {
      push(output, "number", number);
      cursor += number.length;
      continue;
    }
    const word = /^[A-Za-z_][A-Za-z0-9_.-]*/u.exec(source)?.[0];
    if (word !== undefined) {
      const normalized = language === "sql" ? word.toLowerCase() : word;
      const rest = source.slice(word.length);
      let kind: SyntaxKind = "plain";
      if (booleans.has(normalized)) kind = "boolean";
      else if (keywords[language]?.has(normalized)) kind = "keyword";
      else if (/^\s*\(/u.test(rest)) kind = "function";
      else if ((language === "yaml" && /^\s*:/u.test(rest)) || (language === "ini" && /^\s*=/u.test(rest))) kind = "property";
      push(output, kind, word);
      cursor += word.length;
      continue;
    }
    const operator = /^(?:=>|===|!==|==|!=|<=|>=|&&|\|\||\+|-|\*|\/|=|<|>)/u.exec(source)?.[0];
    if (operator !== undefined) {
      push(output, "operator", operator);
      cursor += operator.length;
      continue;
    }
    const punctuation = /^[{}[\]();,:]/u.exec(source)?.[0];
    if (punctuation !== undefined) {
      push(output, "punctuation", punctuation);
      cursor += punctuation.length;
      continue;
    }
    push(output, "plain", source[0] ?? "");
    cursor += 1;
  }
  return output;
}

function matchComment(source: string, language: string, lineStart: boolean): string | null {
  if (language === "sql") return /^(?:--[^\n]*|\/\*[\s\S]*?\*\/)/u.exec(source)?.[0] ?? null;
  if (language === "ts" || language === "tsx" || language === "json") {
    return /^(?:\/\/[^\n]*|\/\*[\s\S]*?\*\/)/u.exec(source)?.[0] ?? null;
  }
  if ((language === "sh" || language === "yaml" || language === "nginx") && lineStart) {
    return /^#[^\n]*/u.exec(source)?.[0] ?? null;
  }
  if (language === "ini" && lineStart) return /^[#;][^\n]*/u.exec(source)?.[0] ?? null;
  return null;
}

function matchString(source: string, language: string): string | null {
  const quote = source[0];
  if (quote !== '"' && quote !== "'" && !(quote === "`" && (language === "ts" || language === "tsx"))) return null;
  let escaped = false;
  for (let index = 1; index < source.length; index += 1) {
    const value = source[index];
    if (escaped) escaped = false;
    else if (value === "\\") escaped = true;
    else if (value === quote) return source.slice(0, index + 1);
  }
  return source;
}

function push(tokens: SyntaxToken[], kind: SyntaxKind, value: string): void {
  const previous = tokens[tokens.length - 1];
  if (previous?.kind === kind) tokens[tokens.length - 1] = { kind, value: previous.value + value };
  else tokens.push({ kind, value });
}

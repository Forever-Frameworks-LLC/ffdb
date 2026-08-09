export interface ParsedGlobalOptions {
  readonly baseUrl?: string;
  readonly projectId?: string;
  readonly developerKey?: string;
  readonly configPath?: string;
  readonly json: boolean;
}

export interface ParsedArguments {
  readonly options: ParsedGlobalOptions;
  readonly command: readonly string[];
}

export function parseArguments(argv: readonly string[]): ParsedArguments {
  const command: string[] = [];
  const values: Record<string, string> = {};
  let json = false;
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--json") json = true;
    else if (value === "--url" || value === "--project" || value === "--key" || value === "--config") {
      values[value] = required(argv[index + 1], `${value} value`);
      index += 1;
    } else if (value?.startsWith("--")) {
      command.push(value);
    } else if (value !== undefined) command.push(value);
  }
  return {
    options: {
      ...(values["--url"] === undefined ? {} : { baseUrl: values["--url"] }),
      ...(values["--project"] === undefined ? {} : { projectId: values["--project"] }),
      ...(values["--key"] === undefined ? {} : { developerKey: values["--key"] }),
      ...(values["--config"] === undefined ? {} : { configPath: values["--config"] }),
      json,
    },
    command,
  };
}

export function required(value: string | undefined, label: string): string {
  if (value === undefined || value === "") throw new Error(`${label} is required`);
  return value;
}

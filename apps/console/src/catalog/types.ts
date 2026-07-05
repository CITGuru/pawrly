// Connector catalog types + small YAML builders shared by the database and API
// catalog entries. A `Connector` is one tile in the "Create a New Connector"
// gallery; `buildYaml` turns the connect-form values into a bare SourceDef the
// AddSource RPC accepts.

export type ConnectorCategory = "database" | "api";

/** Auth modes for the segmented control; the first is the default. */
export type AuthMode = "api_token" | "oauth_org" | "oauth_member" | "none";

export interface ConnectorField {
  name: string;
  label: string;
  type?: "text" | "password" | "number";
  placeholder?: string;
  required?: boolean;
  /** Tucked under the collapsible "Advanced" section. */
  advanced?: boolean;
  help?: string;
}

export interface BuildInput {
  /** Source name (SQL schema prefix). */
  name: string;
  description?: string;
  /** Field name → entered value. */
  values: Record<string, string>;
}

export interface Connector {
  id: string;
  label: string;
  category: ConnectorCategory;
  /** Source kind string, for the badge + icon. */
  kind: string;
  /** "Connect to X" subtitle shown on the card and form header. */
  subtitle: string;
  description?: string;
  docsUrl?: string;
  /** Offered auth modes; only `api_token`/`none` are wired in this version. */
  authModes: AuthMode[];
  fields: ConnectorField[];
  /** Build the bare SourceDef YAML the AddSource RPC accepts. */
  buildYaml: (input: BuildInput) => string;
  /** Render with the dashed "special" style (Custom / Domain Whitelist). */
  custom?: boolean;
}

/** Double-quote and escape a value so it is a safe YAML scalar. */
export function yamlString(v: string): string {
  return '"' + v.replace(/\\/g, "\\\\").replace(/"/g, '\\"') + '"';
}

/** Assemble a bare single-source YAML doc. `config` lines are pre-indented one level. */
export function bareSource(opts: {
  name: string;
  kind: string;
  description?: string;
  configLines?: string[];
  extraLines?: string[];
}): string {
  const lines = [`name: ${opts.name}`, `kind: ${opts.kind}`];
  if (opts.description?.trim()) {
    lines.push(`description: ${yamlString(opts.description.trim())}`);
  }
  if (opts.configLines && opts.configLines.length > 0) {
    lines.push("config:");
    for (const l of opts.configLines) lines.push("  " + l);
  }
  if (opts.extraLines) lines.push(...opts.extraLines);
  return lines.join("\n") + "\n";
}

/** `key: "value"` for each field that has a non-empty value, in order. */
export function configFromValues(
  values: Record<string, string>,
  keys: string[],
): string[] {
  const out: string[] = [];
  for (const k of keys) {
    const v = values[k];
    if (v != null && v !== "") out.push(`${k}: ${yamlString(v)}`);
  }
  return out;
}

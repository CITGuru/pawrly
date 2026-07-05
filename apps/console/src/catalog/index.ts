// The connector catalog the gallery + connect form consume: curated specs
// (generated from sources/{http,file}/*.yaml) merged with hand-authored
// database and special API entries.

import { GENERATED_CONNECTORS, type GeneratedConnector } from "./generated";
import { DATABASE_CONNECTORS } from "./databases";
import { API_SPECIAL_CONNECTORS } from "./apis";
import { yamlString, type Connector, type ConnectorCategory } from "./types";

export type { Connector, ConnectorCategory, ConnectorField, AuthMode } from "./types";

/** Rename a curated spec and inline the entered credential into its ${secret:…}. */
function templateSpec(
  g: GeneratedConnector,
  name: string,
  token: string | undefined,
): string {
  let yaml = g.yaml.replace(/^name:.*$/m, `name: ${name}`);
  if (g.secretVar && token) {
    const re = new RegExp(`\\$\\{secret:${g.secretVar}\\}`, "g");
    yaml = yaml.replace(re, yamlString(token));
  }
  return yaml;
}

function fromGenerated(g: GeneratedConnector): Connector {
  const hasSecret = !!g.secretVar;
  const isApi = g.category === "api";
  return {
    id: g.id,
    label: g.label,
    category: g.category,
    kind: g.kind,
    subtitle: isApi ? `Connect to ${g.label} API` : `Connect to ${g.label}`,
    description: g.description,
    docsUrl: g.docsUrl,
    authModes: hasSecret ? ["api_token", "oauth_org", "oauth_member"] : ["none"],
    fields: hasSecret
      ? [
          {
            name: "token",
            label: "API Key",
            type: "password",
            required: true,
            placeholder: "Your API token",
          },
        ]
      : [],
    buildYaml: (input) => templateSpec(g, input.name, input.values.token),
  };
}

const GENERATED = GENERATED_CONNECTORS.map(fromGenerated);

/** Databases tab: hand-authored DBs first, then curated file connectors. */
export const DATABASE_TAB: Connector[] = [
  ...DATABASE_CONNECTORS,
  ...GENERATED.filter((c) => c.category === "database"),
];

/** APIs tab: special entries first, then curated APIs alphabetically. */
export const API_TAB: Connector[] = [
  ...API_SPECIAL_CONNECTORS,
  ...GENERATED.filter((c) => c.category === "api"),
];

export const CONNECTORS: Connector[] = [...DATABASE_TAB, ...API_TAB];

export function connectorsForCategory(cat: ConnectorCategory): Connector[] {
  return cat === "database" ? DATABASE_TAB : API_TAB;
}

const BY_ID = new Map(CONNECTORS.map((c) => [c.id, c]));

export function connectorById(id: string): Connector | undefined {
  return BY_ID.get(id);
}

/**
 * Best-effort match of a live source to a catalog entry: by name first (the
 * default name equals the connector id), else by kind. Used for list icons.
 */
export function connectorForSource(
  name: string,
  kind: string,
): Connector | undefined {
  return BY_ID.get(name) ?? CONNECTORS.find((c) => c.kind === kind);
}

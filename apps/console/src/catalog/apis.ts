// Special API-tab entries that aren't generated from a curated spec: a manual
// HTTP connector and an unauthenticated domain whitelist. Both register a
// `raw_table` so the source is immediately queryable as `FROM <name>`.

import { bareSource, configFromValues, type Connector } from "./types";

export const API_SPECIAL_CONNECTORS: Connector[] = [
  {
    id: "custom_http",
    label: "Custom / Other",
    category: "api",
    kind: "http",
    subtitle: "Configure hosts, headers, and params manually",
    docsUrl:
      "https://github.com/CITGuru/pawrly/blob/main/docs/sources.md#http-backend-http--rest--graphql-apis",
    authModes: ["api_token", "none"],
    custom: true,
    fields: [
      {
        name: "base_url",
        label: "Base URL",
        type: "text",
        required: true,
        placeholder: "https://api.example.com",
      },
      {
        name: "token",
        label: "Bearer token",
        type: "password",
        placeholder: "Optional — sent as Authorization: Bearer",
        help: "Leave empty for an unauthenticated API.",
      },
    ],
    buildYaml: (input) =>
      bareSource({
        name: input.name,
        kind: "http",
        description: input.description,
        configLines: configFromValues(input.values, ["base_url", "token"]),
        extraLines: ["raw_table: true"],
      }),
  },
  {
    id: "domain_whitelist",
    label: "Domain Whitelist",
    category: "api",
    kind: "http",
    subtitle: "Allow access to a domain without authentication",
    custom: true,
    authModes: ["none"],
    fields: [
      {
        name: "base_url",
        label: "Base URL",
        type: "text",
        required: true,
        placeholder: "https://api.example.com",
      },
    ],
    buildYaml: (input) =>
      bareSource({
        name: input.name,
        kind: "http",
        description: input.description,
        configLines: configFromValues(input.values, ["base_url"]),
        extraLines: ["raw_table: true"],
      }),
  },
];

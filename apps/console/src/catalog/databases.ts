// Hand-authored database / lakehouse connectors for the "Databases" tab. Each
// builds a small bare SourceDef from the form values. Credentials are inlined
// (UI-first); a follow-up swaps these for ${secret:…} + a set-secret RPC.

import {
  bareSource,
  configFromValues,
  yamlString,
  type Connector,
  type ConnectorField,
} from "./types";

/** host/port/database/user/password, with an optional DSN that wins if set. */
const SQL_FIELDS: ConnectorField[] = [
  {
    name: "dsn",
    label: "Connection string (DSN)",
    type: "text",
    placeholder: "postgres://user:pass@host:5432/db",
    help: "Provide a full DSN, or fill the fields below.",
  },
  { name: "host", label: "Host", type: "text", placeholder: "db.internal" },
  { name: "port", label: "Port", type: "number", advanced: true },
  { name: "database", label: "Database", type: "text" },
  { name: "user", label: "User", type: "text" },
  { name: "password", label: "Password", type: "password" },
];

/** DSN-or-fields builder used by Postgres / MySQL. */
function buildSql(kind: string) {
  return (input: { name: string; description?: string; values: Record<string, string> }) => {
    const { values } = input;
    const configLines = values.dsn
      ? configFromValues(values, ["dsn"])
      : configFromValues(values, ["host", "port", "database", "user", "password"]);
    return bareSource({ name: input.name, kind, description: input.description, configLines });
  };
}

export const DATABASE_CONNECTORS: Connector[] = [
  {
    id: "postgres",
    label: "Postgres",
    category: "database",
    kind: "postgres",
    subtitle: "Connect to a PostgreSQL database",
    docsUrl: "https://github.com/CITGuru/pawrly/blob/main/docs/sources.md#databases",
    authModes: ["none"],
    fields: SQL_FIELDS,
    buildYaml: buildSql("postgres"),
  },
  {
    id: "mysql",
    label: "MySQL",
    category: "database",
    kind: "mysql",
    subtitle: "Connect to a MySQL database",
    docsUrl: "https://github.com/CITGuru/pawrly/blob/main/docs/sources.md#databases",
    authModes: ["none"],
    fields: SQL_FIELDS,
    buildYaml: buildSql("mysql"),
  },
  {
    id: "snowflake",
    label: "Snowflake",
    category: "database",
    kind: "snowflake",
    subtitle: "Connect to a Snowflake warehouse",
    docsUrl: "https://github.com/CITGuru/pawrly/blob/main/docs/sources.md#databases",
    authModes: ["none"],
    fields: [
      { name: "account", label: "Account", type: "text", required: true, placeholder: "acme.us-east-1" },
      { name: "user", label: "User", type: "text", required: true },
      { name: "password", label: "Password", type: "password", required: true },
      { name: "database", label: "Database", type: "text", advanced: true },
      { name: "schema", label: "Schema", type: "text", advanced: true },
      { name: "warehouse", label: "Warehouse", type: "text", advanced: true },
      { name: "role", label: "Role", type: "text", advanced: true },
    ],
    buildYaml: (input) =>
      bareSource({
        name: input.name,
        kind: "snowflake",
        description: input.description,
        configLines: configFromValues(input.values, [
          "account",
          "user",
          "password",
          "database",
          "schema",
          "warehouse",
          "role",
        ]),
      }),
  },
  {
    id: "duckdb",
    label: "DuckDB",
    category: "database",
    kind: "duckdb",
    subtitle: "Attach a local DuckDB database file",
    authModes: ["none"],
    fields: [
      { name: "path", label: "Database path", type: "text", required: true, placeholder: "./analytics.duckdb" },
    ],
    buildYaml: (input) =>
      bareSource({
        name: input.name,
        kind: "duckdb",
        description: input.description,
        configLines: configFromValues(input.values, ["path"]),
      }),
  },
  {
    id: "sqlite",
    label: "SQLite",
    category: "database",
    kind: "sqlite",
    subtitle: "Attach a local SQLite database file",
    authModes: ["none"],
    fields: [
      { name: "path", label: "Database path", type: "text", required: true, placeholder: "./app.db" },
    ],
    buildYaml: (input) =>
      bareSource({
        name: input.name,
        kind: "sqlite",
        description: input.description,
        configLines: configFromValues(input.values, ["path"]),
      }),
  },
  {
    id: "ducklake",
    label: "DuckLake",
    category: "database",
    kind: "ducklake",
    subtitle: "Connect to a DuckLake catalog",
    authModes: ["none"],
    fields: [
      { name: "catalog", label: "Catalog", type: "text", required: true, placeholder: "./metadata.ducklake" },
      { name: "data_path", label: "Data path", type: "text", required: true, placeholder: "./lake_data" },
    ],
    buildYaml: (input) =>
      bareSource({
        name: input.name,
        kind: "ducklake",
        description: input.description,
        configLines: configFromValues(input.values, ["catalog", "data_path"]),
      }),
  },
  {
    id: "iceberg",
    label: "Iceberg",
    category: "database",
    kind: "iceberg",
    subtitle: "Query an Apache Iceberg table",
    authModes: ["none"],
    fields: [
      { name: "table", label: "Table name", type: "text", required: true, placeholder: "orders" },
      { name: "path", label: "Table path / location", type: "text", required: true, placeholder: "s3://bucket/warehouse/orders" },
    ],
    buildYaml: (input) =>
      bareSource({
        name: input.name,
        kind: "iceberg",
        description: input.description,
        extraLines: [
          "tables:",
          `  - { name: ${input.values.table || "data"}, path: ${yamlString(input.values.path || "")} }`,
        ],
      }),
  },
  {
    id: "delta",
    label: "Delta Lake",
    category: "database",
    kind: "delta",
    subtitle: "Query a Delta Lake table",
    authModes: ["none"],
    fields: [
      { name: "table", label: "Table name", type: "text", required: true, placeholder: "orders" },
      { name: "path", label: "Table path / location", type: "text", required: true, placeholder: "s3://bucket/warehouse/orders" },
    ],
    buildYaml: (input) =>
      bareSource({
        name: input.name,
        kind: "delta",
        description: input.description,
        extraLines: [
          "tables:",
          `  - { name: ${input.values.table || "data"}, path: ${yamlString(input.values.path || "")} }`,
        ],
      }),
  },
];

// The source kinds Pawrly speaks, grouped the way the docs + README frame them.
// Used by the Sources grid and the marquee strip.

export type SourceGroup = {
  kind: string;
  blurb: string;
  items: string[];
};

export const sourceGroups: SourceGroup[] = [
  {
    kind: "APIs",
    blurb:
      "Give Pawrly an endpoint or OpenAPI spec, then read the response as rows and columns.",
    items: ["REST", "GraphQL", "OpenAPI 3.0", "Stripe", "Intercom", "HubSpot"],
  },
  {
    kind: "Files & Object Storage",
    blurb:
      "Read Parquet, CSV, and JSON from disk or a bucket without loading them into a warehouse first.",
    items: ["Parquet", "CSV", "JSON", "S3", "GCS", "Azure"],
  },
  {
    kind: "MCP Servers",
    blurb:
      "Connect to MCP servers like Linear, GitHub, Notion, and internal tools and query their outputs as rows.",
    items: ["Linear", "GitHub", "Notion", "Any MCP server"],
  },
  {
    kind: "Databases",
    blurb:
      "Join operational tables with APIs, files, and warehouse data from the same query.",
    items: ["Postgres", "MySQL", "SQLite", "DuckDB"],
  },
  {
    kind: "Warehouses & Lakehouses",
    blurb:
      "Use existing warehouse and lakehouse data beside live sources, without building a pipeline first.",
    items: ["Snowflake", "Iceberg", "Delta", "DuckLake"],
  },
];

// A flat list for the scrolling trust strip.
export const marqueeSources: string[] = [
  "Stripe",
  "Postgres",
  "Snowflake",
  "S3",
  "Iceberg",
  "GraphQL",
  "OpenAPI",
  "Linear",
  "DuckDB",
  "Parquet",
  "MySQL",
  "Delta",
  "Notion",
  "Intercom",
  "GCS",
  "DuckLake",
];

import { tableFromIPC } from "apache-arrow";

export interface DecodedBatch {
  columns: string[];
  rows: string[][];
}

/**
 * Decode one `QueryResponse.ipc_stream` frame. Each frame is a self-contained
 * Arrow IPC stream (schema + one record batch), so `tableFromIPC` reads it
 * whole. Cells are stringified for display — the SQL runner only ever renders
 * text, never interprets it (see the XSS note in the design doc).
 */
export function decodeBatch(bytes: Uint8Array): DecodedBatch {
  const table = tableFromIPC(bytes);
  const columns = table.schema.fields.map((f) => f.name);
  const rows: string[][] = [];
  for (const row of table) {
    rows.push(columns.map((c) => formatCell(row[c])));
  }
  return { columns, rows };
}

function formatCell(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  if (value instanceof Date) return value.toISOString();
  if (value instanceof Uint8Array) return `0x${bytesToHex(value)}`;
  try {
    return JSON.stringify(value, (_k, v) =>
      typeof v === "bigint" ? v.toString() : v,
    );
  } catch {
    return String(value);
  }
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

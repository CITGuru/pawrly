import { ogImage, OG_SIZE, OG_CONTENT_TYPE } from "@/lib/og";

export const alt = "Pawrly — Query APIs, files, MCP servers, and databases with SQL";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export default function OpengraphImage() {
  return ogImage({
    fontSize: 76,
    title: (
      <>
        Query&nbsp;<span style={{ color: "#f1dcb0" }}>APIs, files,</span>&nbsp;MCP servers, and
        databases with SQL.
      </>
    ),
  });
}

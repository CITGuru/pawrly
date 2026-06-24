import { ogImage, OG_SIZE, OG_CONTENT_TYPE } from "@/lib/og";

export const alt = "Pawrly — Query APIs, files, MCP servers, and databases with SQL";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export default function OpengraphImage() {
  return ogImage({
    fontSize: 70,
    title: "Query APIs, files, MCP servers, and databases with SQL.",
    accent: "APIs, files,",
  });
}

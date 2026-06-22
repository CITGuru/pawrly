import { getSearchIndex } from "@/lib/docs";

// Static JSON search index over the docs (headings + section text). Fetched
// on demand by the client search dialog — self-hosted, no external service.
export const dynamic = "force-static";

export function GET() {
  return Response.json(getSearchIndex());
}

import { ogImage, OG_SIZE, OG_CONTENT_TYPE } from "@/lib/og";
import { featureBySlug } from "@/lib/features";

export const alt = "Pawrly — Materialization";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export default function Image() {
  return ogImage({ eyebrow: "Feature", title: featureBySlug["materialization"].title });
}

import { ogImage, OG_SIZE, OG_CONTENT_TYPE } from "@/lib/og";
import { getPosts } from "@/lib/posts";

export const alt = "Pawrly blog";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export function generateStaticParams() {
  return getPosts().map((p) => ({ slug: p.slug }));
}

export default async function Image({ params }: { params: Promise<{ slug: string }> }) {
  const { slug } = await params;
  const post = getPosts().find((p) => p.slug === slug);
  return ogImage({ eyebrow: "Blog", title: post?.title ?? "Pawrly blog" });
}

import type { MetadataRoute } from "next";
import { getPosts } from "@/lib/posts";
import { features } from "@/lib/features";

const SITE = "https://pawrly.dev";

export default function sitemap(): MetadataRoute.Sitemap {
  const now = new Date();

  const staticRoutes: MetadataRoute.Sitemap = [
    {
      url: `${SITE}/`,
      lastModified: now,
      changeFrequency: "weekly",
      priority: 1.0,
    },
    {
      url: `${SITE}/blog`,
      lastModified: now,
      changeFrequency: "weekly",
      priority: 0.8,
    },
  ];

  const featureRoutes: MetadataRoute.Sitemap = features.map((f) => ({
    url: `${SITE}/features/${f.slug}`,
    lastModified: now,
    changeFrequency: "monthly",
    priority: 0.8,
  }));

  const blogRoutes: MetadataRoute.Sitemap = getPosts().map((p) => ({
    url: `${SITE}/blog/${p.slug}`,
    lastModified: now,
    changeFrequency: "monthly",
    priority: 0.7,
  }));

  return [...staticRoutes, ...featureRoutes, ...blogRoutes];
}

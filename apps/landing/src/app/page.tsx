import { Nav } from "@/components/Nav";
import { Grain } from "@/components/Grain";
import { Hero } from "@/components/sections/Hero";
import { SourceMarquee } from "@/components/sections/SourceMarquee";
import { OneInterface } from "@/components/sections/OneInterface";
import { Sources } from "@/components/sections/Sources";
import { Agents } from "@/components/sections/Agents";
import { Governance } from "@/components/sections/Governance";
import { BlogTeasers } from "@/components/sections/BlogTeasers";
import { Install } from "@/components/sections/Install";
import { Footer } from "@/components/sections/Footer";
import { Horizon } from "@/components/UI";

export default function Home() {
  return (
    <>
      <Grain />
      <Nav />
      <main className="flex w-full flex-col">
        <Hero />
        <SourceMarquee />
        <OneInterface />
        <Horizon className="mx-auto max-w-6xl" />
        <Sources />
        <Agents />
        <Horizon className="mx-auto max-w-6xl" />
        <Governance />
        <BlogTeasers />
        <Install />
      </main>
      <Footer />
    </>
  );
}

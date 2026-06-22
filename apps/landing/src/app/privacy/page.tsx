import type { Metadata } from "next";
import { LegalLayout } from "@/components/LegalLayout";

export const metadata: Metadata = {
  title: "Privacy Policy",
  description:
    "How the pawrly.dev website handles data — and why the Pawrly software keeps your data on your own systems.",
};

export default function PrivacyPage() {
  return (
    <LegalLayout
      title="Privacy Policy"
      updated="June 21, 2026"
      intro="This policy explains how the pawrly.dev website handles information. The short version: the Pawrly software runs on your own machines, and this site collects as little as possible."
    >
      <h2>The Pawrly software stays on your systems</h2>
      <p>
        Pawrly is an open-source tool you install and run yourself. Your queries, source
        credentials, configuration, and results are processed locally on your own machine or
        infrastructure. We operate no servers in that data path and do not receive, store, or have
        access to the data you query with Pawrly.
      </p>

      <h2>What this website collects</h2>
      <p>
        You can browse pawrly.dev without creating an account or providing personal information.
        The site does not run advertising or third-party tracking cookies.
      </p>
      <p>
        The site is hosted on Vercel. Like any web host, Vercel processes standard request metadata
        (such as IP address, user agent, and the pages requested) transiently to deliver the site,
        keep it secure, and operate it reliably. That processing is governed by{" "}
        <a href="https://vercel.com/legal/privacy-policy" target="_blank" rel="noreferrer">
          Vercel&apos;s privacy policy
        </a>
        . If we add privacy-respecting analytics in the future, we will update this page to describe
        it.
      </p>

      <h2>Downloads and the installer</h2>
      <p>
        Installing Pawrly downloads release artifacts from GitHub; those requests are handled by
        GitHub and subject to its privacy practices. The installer does not send analytics or
        identifying information back to us.
      </p>

      <h2>Links to other sites</h2>
      <p>
        Pages here may link to third-party sites — for example GitHub, documentation, or hosting
        providers. Those sites have their own privacy policies, and we are not responsible for their
        practices.
      </p>

      <h2>Children</h2>
      <p>This site is not directed to children and we do not knowingly collect data from them.</p>

      <h2>Changes</h2>
      <p>
        We may update this policy as the project evolves. Material changes will be reflected by the
        &ldquo;Last updated&rdquo; date above.
      </p>

      <h2>Contact</h2>
      <p>
        Questions about this policy can be raised via the{" "}
        <a href="https://github.com/CITGuru/pawrly/issues" target="_blank" rel="noreferrer">
          project&apos;s GitHub issues
        </a>
        .
      </p>
    </LegalLayout>
  );
}

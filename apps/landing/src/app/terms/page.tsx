import type { Metadata } from "next";
import { LegalLayout } from "@/components/LegalLayout";

export const metadata: Metadata = {
  title: "Terms of Use",
  description: "The terms that govern use of the pawrly.dev website and documentation.",
};

export default function TermsPage() {
  return (
    <LegalLayout
      title="Terms of Use"
      updated="June 21, 2026"
      intro="These terms govern your use of the pawrly.dev website and its documentation. Use of the Pawrly software itself is governed by its open-source license."
    >
      <h2>The software is open source</h2>
      <p>
        Pawrly is licensed under the Apache License 2.0. Your rights to use, modify, and distribute
        the software are defined by that{" "}
        <a href="https://github.com/CITGuru/pawrly/blob/main/LICENSE" target="_blank" rel="noreferrer">
          license
        </a>
        , which controls over these website terms for anything concerning the software. These terms
        cover the website and documentation only.
      </p>

      <h2>Acceptable use</h2>
      <p>By using this website, you agree not to:</p>
      <ul>
        <li>attempt to disrupt, attack, or gain unauthorized access to the site or its infrastructure;</li>
        <li>scrape or access the site in a way that places an unreasonable load on it; or</li>
        <li>use the site to violate any applicable law.</li>
      </ul>

      <h2>Intellectual property</h2>
      <p>
        The Pawrly name, logo, and the content of this website are the property of the project and
        its maintainers, except where third-party content is attributed. The open-source code is
        available under its license; these terms do not grant rights to the name or branding beyond
        fair, descriptive reference.
      </p>

      <h2>No warranty</h2>
      <p>
        The website and documentation are provided &ldquo;as is,&rdquo; without warranties of any
        kind, express or implied, including accuracy, fitness for a particular purpose, or
        non-infringement. The software carries the warranty disclaimer stated in its license.
      </p>

      <h2>Limitation of liability</h2>
      <p>
        To the maximum extent permitted by law, the project and its maintainers are not liable for
        any indirect, incidental, or consequential damages arising from your use of the website or
        documentation.
      </p>

      <h2>Third-party links</h2>
      <p>
        This site links to third-party resources we do not control. We are not responsible for their
        content or practices, and a link is not an endorsement.
      </p>

      <h2>Changes</h2>
      <p>
        We may update these terms from time to time. The &ldquo;Last updated&rdquo; date above
        reflects the current version, and continued use of the site means you accept the latest
        terms.
      </p>

      <h2>Governing law</h2>
      <p>
        These terms are governed by the laws of the maintainers&apos; jurisdiction, without regard
        to conflict-of-law rules.
      </p>

      <h2>Contact</h2>
      <p>
        Questions about these terms can be raised via the{" "}
        <a href="https://github.com/CITGuru/pawrly/issues" target="_blank" rel="noreferrer">
          project&apos;s GitHub issues
        </a>
        .
      </p>
    </LegalLayout>
  );
}

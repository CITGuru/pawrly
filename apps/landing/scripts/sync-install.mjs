// Copies the canonical installers (repo-root scripts/*) into this app's public/
// dir so they are served at https://pawrly.dev/<name>. Run automatically via
// predev/prebuild — keep the scripts themselves as the single source of truth
// and never hand-edit the generated copies. No-ops gracefully on any file that
// is absent (e.g. a standalone deploy that didn't bundle the repo root).
import { copyFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const scriptsDir = join(here, "..", "..", "..", "scripts");
const destDir = join(here, "..", "public");

// Repo-root file -> served filename under public/.
const FILES = [
  ["install.sh", "install.sh"],
  ["install.ps1", "install.ps1"],
];

mkdirSync(destDir, { recursive: true });

for (const [srcName, destName] of FILES) {
  const src = join(scriptsDir, srcName);
  if (!existsSync(src)) {
    console.warn(`[sync-install] source not found: ${src} — skipping`);
    continue;
  }
  copyFileSync(src, join(destDir, destName));
  console.log(`[sync-install] copied scripts/${srcName} -> public/${destName}`);
}

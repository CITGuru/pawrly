// Copies canonical repo assets into this app's public/ dir so they are served at
// https://pawrly.dev/<name>. Run automatically via predev/prebuild — keep the
// originals as the single source of truth and never hand-edit the generated
// copies. No-ops gracefully on any file that is absent (e.g. a standalone deploy
// that didn't bundle the repo root).
import { copyFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..", "..");
const destDir = join(here, "..", "public");

// repo-root-relative source -> served filename under public/.
const ASSETS = [
  ["scripts/install.sh", "install.sh"],
  ["scripts/install.ps1", "install.ps1"],
  ["schemas/pawrly.schema.json", "pawrly.schema.json"],
];

mkdirSync(destDir, { recursive: true });

for (const [srcRel, destName] of ASSETS) {
  const src = join(repoRoot, srcRel);
  if (!existsSync(src)) {
    console.warn(`[sync-assets] source not found: ${src} — skipping`);
    continue;
  }
  copyFileSync(src, join(destDir, destName));
  console.log(`[sync-assets] copied ${srcRel} -> public/${destName}`);
}

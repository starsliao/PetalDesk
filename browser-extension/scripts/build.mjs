import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const sourceRoot = join(projectRoot, "src");
const manifestRoot = join(projectRoot, "manifest");
const outputRoot = join(projectRoot, "dist");

const sharedFiles = [
  ["shared/scroll-core.js", "shared/scroll-core.js"],
  ["shared/browser-api.js", "shared/browser-api.js"],
  ["shared/password-templates.js", "shared/password-templates.js"],
  ["content/capture-session.js", "content/capture-session.js"],
  ["background/native-bridge.js", "background/native-bridge.js"],
];

const variants = [
  {
    name: "chromium",
    manifest: "chromium.json",
    files: [["background/chromium-entry.js", "background/chromium-entry.js"]],
  },
  {
    name: "firefox",
    manifest: "firefox.json",
    files: [
      ["content/password-manager.js", "content/password-manager.js"],
      ["background/password-bridge.js", "background/password-bridge.js"],
      ["popup/popup.html", "popup/popup.html"],
      ["popup/popup.js", "popup/popup.js"],
      ["popup/popup.css", "popup/popup.css"],
      ["../assets/icons/petaldesk-16.png", "assets/icons/petaldesk-16.png"],
      ["../assets/icons/petaldesk-32.png", "assets/icons/petaldesk-32.png"],
      ["../assets/icons/petaldesk-48.png", "assets/icons/petaldesk-48.png"],
      ["../assets/icons/petaldesk-96.png", "assets/icons/petaldesk-96.png"],
      ["../assets/icons/petaldesk-128.png", "assets/icons/petaldesk-128.png"],
      ["../assets/icons/lucide/LICENSE", "assets/icons/lucide/LICENSE"],
      ["../assets/icons/lucide/README.md", "assets/icons/lucide/README.md"],
      ["../assets/icons/lucide/user-round.svg", "assets/icons/lucide/user-round.svg"],
      ["../assets/icons/lucide/key-round.svg", "assets/icons/lucide/key-round.svg"],
      ["../assets/icons/lucide/shield-check.svg", "assets/icons/lucide/shield-check.svg"],
      ["../assets/icons/lucide/trash-2.svg", "assets/icons/lucide/trash-2.svg"],
    ],
  },
];

async function copyFileToVariant(variantRoot, sourceRelative, targetRelative) {
  const target = join(variantRoot, targetRelative);
  await mkdir(dirname(target), { recursive: true });
  await cp(join(sourceRoot, sourceRelative), target);
}

await rm(outputRoot, { recursive: true, force: true });

for (const variant of variants) {
  const variantRoot = join(outputRoot, variant.name);
  await mkdir(variantRoot, { recursive: true });

  const manifestText = await readFile(join(manifestRoot, variant.manifest), "utf8");
  const manifest = JSON.parse(manifestText);
  await writeFile(
    join(variantRoot, "manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
    "utf8",
  );

  for (const [sourceRelative, targetRelative] of [...sharedFiles, ...variant.files]) {
    await copyFileToVariant(variantRoot, sourceRelative, targetRelative);
  }
}

console.log(`Built Chromium and Firefox extensions in ${outputRoot}`);

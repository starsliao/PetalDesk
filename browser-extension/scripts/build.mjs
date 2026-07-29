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
    files: [],
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

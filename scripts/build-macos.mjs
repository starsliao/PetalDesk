import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

function resolveBuildTimestamp(environment = process.env, now = Date.now()) {
  const explicit = environment.PETALDESK_BUILD_TIMESTAMP?.trim()
    || environment.SOURCE_DATE_EPOCH?.trim()
    || String(Math.floor(now / 1000));
  if (!/^[1-9]\d*$/.test(explicit)) {
    throw new Error("PETALDESK_BUILD_TIMESTAMP/SOURCE_DATE_EPOCH must be a positive Unix timestamp in seconds");
  }
  return explicit;
}

export { resolveBuildTimestamp };

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const timestamp = resolveBuildTimestamp();
  const command = process.platform === "win32" ? "pnpm.cmd" : "pnpm";
  const result = spawnSync(
    command,
    ["exec", "tauri", "build", "--target", "universal-apple-darwin", "--bundles", "app,dmg"],
    {
      env: { ...process.env, PETALDESK_BUILD_TIMESTAMP: timestamp },
      stdio: "inherit",
    },
  );
  if (result.error) throw result.error;
  process.exit(result.status ?? 1);
}

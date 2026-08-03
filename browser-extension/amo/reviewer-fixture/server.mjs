import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const host = "127.0.0.1";
const port = 43127;
const pages = new Map(await Promise.all([
  ["/login", "login.html"],
  ["/signed-in", "login.html"],
  ["/two-step", "two-step.html"],
  ["/password-change", "password-change.html"],
  ["/failed-login", "failed-login.html"],
  ["/ambiguous", "ambiguous.html"],
].map(async ([route, file]) => [route, await readFile(new URL(`./${file}`, import.meta.url))])));

const server = createServer((request, response) => {
  const path = new URL(request.url || "/", `http://${host}:${port}`).pathname;
  const html = pages.get(path === "/" ? "/login" : path);
  if (request.method !== "GET" || !html) {
    response.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
    response.end("Not found");
    return;
  }
  response.writeHead(200, {
    "cache-control": "no-store",
    "content-length": html.length,
    "content-type": "text/html; charset=utf-8",
    "x-content-type-options": "nosniff",
  });
  response.end(html);
});

server.listen(port, host, () => {
  process.stdout.write(`PetalDesk reviewer fixtures: http://${host}:${port}/login\n`);
});

process.on("SIGINT", () => server.close());

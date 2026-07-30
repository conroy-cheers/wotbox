import http from "node:http";

const port = Number(process.env.QBITTORRENT_MOCK_PORT ?? 18001);
const torrents = [];

const server = http.createServer((request, response) => {
  const url = new URL(request.url ?? "/", `http://${request.headers.host}`);
  response.setHeader("content-type", "application/json");
  if (url.pathname === "/api/v2/app/version") {
    response.setHeader("content-type", "text/plain");
    response.end("v5.0.0-test");
    return;
  }
  if (url.pathname === "/api/v2/sync/maindata") {
    response.end(JSON.stringify({
      server_state: { free_space_on_disk: 1_000_000_000_000 }
    }));
    return;
  }
  if (url.pathname === "/api/v2/torrents/info") {
    response.end(JSON.stringify(torrents));
    return;
  }
  if (url.pathname === "/api/v2/torrents/add" && request.method === "POST") {
    request.resume();
    request.on("end", () => response.end(JSON.stringify({
      success_count: 1,
      failure_count: 0,
      pending_count: 0,
      added_torrent_ids: ["fixture"]
    })));
    return;
  }
  response.statusCode = 404;
  response.end(JSON.stringify({ error: "not_found", path: url.pathname }));
});

server.listen(port, "127.0.0.1", () => {
  process.stdout.write(`qBittorrent fixture listening on ${port}\n`);
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => server.close(() => process.exit(0)));
}

// A dev server the SMOKE fixture boots: prints the Local: line, serves one healthy route and
// one that renders a crash banner.
const http = require("node:http");
const port = Number(process.env.PORT || 47391);
const page = (body) => `<!doctype html><html><body><main>${body}</main></body></html>`;
const server = http.createServer((req, res) => {
  if (req.url === "/") {
    res.writeHead(200, { "content-type": "text/html" });
    res.end(page("<h1>Receipts</h1><p>Every receipt on this page is logged to a topic on the local network.</p>"));
    return;
  }
  if (req.url === "/favicon.ico") {
    res.writeHead(204);
    res.end();
    return;
  }
  if (req.url === "/broken") {
    res.writeHead(200, { "content-type": "text/html" });
    res.end(page("<h1>Application error: a client-side exception has occurred</h1><p>See the console for details.</p>"));
    return;
  }
  res.writeHead(404, { "content-type": "text/html" });
  res.end(page("<h1>Not found</h1><p>There is nothing at this address on this server.</p>"));
});
server.listen(port, "127.0.0.1", () => {
  console.log(`  Local: http://127.0.0.1:${port}`);
});

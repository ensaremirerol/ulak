// Dev helper: fake GitHub REST API for hub e2e runs (loopback only, never real tokens).
// usage: node web/scripts/fake-github.mjs [port] [logfile]   (hub: ULAK_GITHUB_API=http://127.0.0.1:<port>)
// Accepts only `Authorization: Bearer fake-token-123`; logs every request's method, url and headers.
import http from 'node:http';
import fs from 'node:fs';
const port = Number(process.argv[2] ?? 19090), logf = process.argv[3] ?? 'github-requests.log';
const repo = (n, d) => ({ full_name: n, name: n.split('/')[1], private: false, description: d, default_branch: 'main', pushed_at: '2026-01-01T00:00:00Z', owner: { login: 'tester' }, permissions: { push: true } });
const page1 = [repo('tester/hello', 'hello world'), repo('tester/other', 'other repo')];
const page2 = [repo('tester/third', 'third repo')];
http.createServer((req, res) => {
  fs.appendFileSync(logf, JSON.stringify({ m: req.method, url: req.url, headers: req.headers }) + '\n');
  const send = (c, b, h = {}) => { res.writeHead(c, { 'content-type': 'application/json', ...h }); res.end(JSON.stringify(b)); };
  if (req.headers.authorization !== 'Bearer fake-token-123') return send(401, { message: 'Bad credentials' });
  const u = new URL(req.url, 'http://x');
  if (u.pathname === '/user') return send(200, { login: 'tester' });
  if (u.pathname === '/user/repos') {
    const p = Number(u.searchParams.get('page') ?? 1);
    return p === 1 ? send(200, page1, { link: `<http://127.0.0.1:${port}/user/repos?page=2>; rel="next"` }) : send(200, page2);
  }
  if (u.pathname === '/repos/tester/hello') return send(200, repo('tester/hello', 'hello world'));
  if (u.pathname === '/repos/tester/hello/branches') return send(200, [{ name: 'main' }, { name: 'dev' }]);
  send(404, { message: 'Not Found' });
}).listen(port, '127.0.0.1');

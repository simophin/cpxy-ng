// Browser regression checks with Node >=22 and an installed Chrome; no npm dependencies.
// CHROME_BIN=/path/to/chrome node --test mobile-engine/test/traffic-ui.mjs
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(check, tries = 100) {
  for (let i = 0; i < tries; i++) { if (await check()) return; await delay(50); }
  throw new Error("Browser condition timed out");
}

test("offline traffic page: history, follow, bounds, restart, retry, hidden tabs, mobile", { timeout: 40000 }, async () => {
  const profile = await mkdtemp(join(tmpdir(), "cpxy-ui-"));
  const state = { session: "one", end: 600, oldest: 0, fail: false };
  const requests = [];
  const errors = [];
  const source = await readFile(new URL("../src/traffic_api.rs", import.meta.url), "utf8");
  const csp = source.match(/"(default-src [^"]+)"/)[1];
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost");
    requests.push(url.pathname);
    res.setHeader("Content-Security-Policy", csp);
    if (url.pathname.startsWith("/api/")) {
      if (state.fail) { res.writeHead(503); res.end(); return; }
      res.setHeader("Content-Type", "application/json");
      if (url.pathname === "/api/traffic") {
        res.end(JSON.stringify({ session: state.session, running: true, sent: 123, received: 456 }));
      } else {
        const limit = Number(url.searchParams.get("limit") || 200);
        const before = Math.min(Number(url.searchParams.get("before") ?? state.end), state.end);
        const start = url.searchParams.has("since") ? Math.max(Number(url.searchParams.get("since")), state.oldest) : Math.max(state.oldest, before - limit);
        const end = url.searchParams.has("since") ? Math.min(start + limit, state.end) : before;
        const records = Array.from({ length: Math.max(0, end - start) }, (_, i) => ({
          seq: start + i,
          event: { host: "<script>window.injected=true</script>", ip: "93.184.216.34", port: 443, outbound: "proxy", delay_millis: 42, time_millis: Date.now(), error: null },
        }));
        res.end(JSON.stringify({ session: state.session, oldest_seq: state.oldest, records }));
      }
      return;
    }
    const files = { "/": ["index.html", "text/html"], "/style.css": ["style.css", "text/css"], "/app.js": ["app.js", "text/javascript"] };
    if (!files[url.pathname]) { res.writeHead(404); res.end(); return; }
    const [file, type] = files[url.pathname];
    res.setHeader("Content-Type", type);
    res.end(await readFile(new URL(`../web/${file}`, import.meta.url)));
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const chrome = process.env.CHROME_BIN || (process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "google-chrome");
  const child = spawn(chrome, ["--headless", "--no-sandbox", "--no-first-run", "--disable-background-networking", "--disable-extensions", "--disable-sync", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
  let stderr = "";
  child.stderr.on("data", data => { stderr = (stderr + data).slice(-4000); });
  child.on("error", error => errors.push(error.message));
  let socket;
  try {
    let port;
    // A cold CI runner can take well over 5s to start Chrome
    await until(async () => {
      if (errors.length) throw new Error(errors.join("\n"));
      if (child.exitCode !== null || child.signalCode !== null) throw new Error(`Chrome exited (${child.exitCode ?? child.signalCode})`);
      try { port = (await readFile(join(profile, "DevToolsActivePort"), "utf8")).split("\n")[0]; return true; } catch { return false; }
    }, 600).catch(error => { throw new Error(`${error.message}\nChrome stderr:\n${stderr}`, { cause: error }); });
    const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    socket = new WebSocket(targets.find(target => target.type === "page").webSocketDebuggerUrl);
    await new Promise(resolve => socket.addEventListener("open", resolve, { once: true }));
    let id = 0;
    const callbacks = new Map();
    socket.addEventListener("message", ({ data }) => {
      const message = JSON.parse(data);
      if (message.method === "Runtime.exceptionThrown") errors.push(message.params.exceptionDetails.text);
      if (message.method === "Log.entryAdded" && message.params.entry.level === "error") errors.push(message.params.entry.text);
      if (message.id) {
        const callback = callbacks.get(message.id);
        callbacks.delete(message.id);
        message.error ? callback.reject(message.error) : callback.resolve(message.result);
      }
    });
    const send = (method, params = {}) => new Promise((resolve, reject) => {
      const number = ++id;
      callbacks.set(number, { resolve, reject });
      socket.send(JSON.stringify({ id: number, method, params }));
    });
    const evaluate = async expression => {
      const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
      assert.equal(result.exceptionDetails, undefined);
      return result.result.value;
    };
    const waitFor = async expression => {
      try { await until(() => evaluate(expression)); }
      catch (error) { throw new Error(`${expression}: ${await evaluate("JSON.stringify({busy, count:records.length, first:records[0]?.seq, follow:ui.follow.checked, status:ui.status.textContent})")}`, { cause: error }); }
    };
    const click = id => until(() => evaluate(`!busy && !document.getElementById(${JSON.stringify(id)}).disabled ? (document.getElementById(${JSON.stringify(id)}).click(), true) : false`));
    const count = "document.querySelectorAll('li').length";
    const first = "document.querySelector('li')?.dataset.seq";
    await send("Runtime.enable");
    await send("Log.enable");
    await send("Emulation.setDeviceMetricsOverride", { width: 1100, height: 850, deviceScaleFactor: 1, mobile: false });
    await send("Page.navigate", { url: origin });
    await waitFor(`${count} === 200 && !busy`);
    assert.equal(await evaluate("window.injected === undefined && document.querySelectorAll('li script').length === 0"), true);
    assert.equal(await evaluate("performance.getEntriesByType('resource').every(r => r.name.startsWith(location.origin))"), true);
    assert.equal(await evaluate("document.getElementById('follow').checked"), true);
    await evaluate("document.querySelector('li').dataset.stable = 'true'");
    await delay(1100);
    assert.equal(await evaluate("document.querySelector('li').dataset.stable"), "true");
    // Loading older pauses following and retains the current rows.
    await click("older");
    await waitFor(`${count} === 400 && !busy`);
    assert.equal(await evaluate(first), "200");
    assert.equal(await evaluate("document.getElementById('follow').checked"), false);
    state.end = 1600;
    await delay(1200);
    assert.equal(await evaluate(count), 400);
    await click("latest");
    await waitFor(`${first} === '1400' && !busy`);
    state.end = 2600;
    await waitFor(`${count} === 1200 && !busy`);
    state.end = 3600;
    await waitFor(`${count} === 2000 && !busy`);
    // Evicted cursors must reload a coherent latest page.
    state.oldest = 3800; state.end = 4000;
    await waitFor(`${first} === '3800' && ${count} === 200 && !busy`);
    state.session = "two"; state.oldest = 0; state.end = 3;
    await waitFor(`${first} === '0' && ${count} === 3 && !busy`);
    assert.match(await evaluate("document.getElementById('message').textContent"), /restarted/);
    state.fail = true;
    await waitFor("document.getElementById('status').textContent.includes('unavailable') && !busy");
    state.fail = false;
    await waitFor("document.getElementById('status').textContent === 'Connected to router' && !busy");
    // Simulate the visibility event; hidden tabs must stop all API requests.
    await evaluate("Object.defineProperty(document, 'hidden', { configurable: true, value: true }); document.dispatchEvent(new Event('visibilitychange'))");
    const pausedAt = requests.length;
    await delay(1300);
    assert.equal(requests.length, pausedAt);
    await evaluate("delete document.hidden; document.dispatchEvent(new Event('visibilitychange'))");
    await waitFor("!busy");
    await send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
    assert.equal(await evaluate("document.documentElement.scrollWidth <= innerWidth"), true);
    // Failed fetches intentionally produce Chrome network-error log entries.
    assert.deepEqual(errors.filter(error => !error.includes("503")), []);
    if (process.env.CPXY_UI_SCREENSHOT) {
      const screenshot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
      await writeFile(process.env.CPXY_UI_SCREENSHOT, Buffer.from(screenshot.data, "base64"));
    }
  } finally {
    socket?.close();
    const exited = new Promise(resolve => child.once("exit", resolve));
    child.kill();
    if (child.exitCode === null) await Promise.race([exited, delay(2000)]);
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    await rm(profile, { recursive: true, force: true, maxRetries: 3 });
  }
});

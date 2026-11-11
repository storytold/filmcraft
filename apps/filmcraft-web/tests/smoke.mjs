// Browser smoke test of the FilmCraft web app over the Chrome DevTools Protocol.
// No npm dependencies (Node ≥ 22: global WebSocket and fetch).
//
//   cargo xtask web --serve 8765 &            # build + serve target/web/dist
//   node apps/filmcraft-web/tests/smoke.mjs --url http://127.0.0.1:8765/ --media clip.mp4 --out /tmp/fc-web
//
// Steps: load (timing), demo project, play 2 s (frames shown/dropped), import the media through
// `filmcraft.importUrl`, put it on a new sequence, export H.264 (download), screenshots of each
// step; then every mode and a tiny window. `--media` must be reachable from the page (copy it next
// to index.html). Prints a JSON report; exits non-zero on failure, including any Rust panic or
// uncaught exception in the console.
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, readdirSync, statSync, mkdtempSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { checkImportBins } from "./import-bins.mjs";
import { checkImportCollision } from "./import-collision.mjs";

const arg = (k, d) => {
  const i = process.argv.indexOf(`--${k}`);
  return i > 0 ? process.argv[i + 1] : d;
};
const url = arg("url", "http://127.0.0.1:8765/");
const out = arg("out", join(tmpdir(), "filmcraft-web-smoke"));
const media = arg("media", "web-test.mp4");
const chrome = arg("chrome", process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "google-chrome");
const port = Number(arg("port", "9333"));
const headless = !process.argv.includes("--headed");
mkdirSync(out, { recursive: true });
const downloads = join(out, "downloads");
mkdirSync(downloads, { recursive: true });

const profile = mkdtempSync(join(tmpdir(), "fc-chrome-"));
const proc = spawn(chrome, [
  ...(headless ? ["--headless=new"] : []),
  `--remote-debugging-port=${port}`,
  `--user-data-dir=${profile}`,
  "--no-first-run",
  "--no-default-browser-check",
  "--enable-unsafe-webgpu",
  "--autoplay-policy=no-user-gesture-required",
  "--window-size=1600,1000",
  "about:blank",
], { stdio: "ignore" });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let ws;
let nextId = 1;
const waiting = new Map();
const logs = [];

async function connect() {
  for (let i = 0; i < 100; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      const page = list.find((t) => t.type === "page");
      if (page) {
        ws = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
        ws.onmessage = (m) => {
          const msg = JSON.parse(m.data);
          if (msg.id && waiting.has(msg.id)) {
            waiting.get(msg.id)(msg);
            waiting.delete(msg.id);
          } else if (msg.method === "Runtime.consoleAPICalled") {
            logs.push(msg.params.args.map((a) => a.value ?? a.description ?? "").join(" "));
          } else if (msg.method === "Runtime.exceptionThrown") {
            logs.push("EXCEPTION " + JSON.stringify(msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text));
          }
        };
        return;
      }
    } catch {}
    await sleep(100);
  }
  throw new Error("Chrome did not start");
}

function send(method, params = {}) {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((res, rej) => waiting.set(id, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result))));
}

async function js(expr, timeout = 120000) {
  const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true, timeout });
  if (r.exceptionDetails) throw new Error(`${expr}: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
  return r.result.value;
}

async function shot(name) {
  const r = await send("Page.captureScreenshot", { format: "png" });
  const p = join(out, `${name}.png`);
  writeFileSync(p, Buffer.from(r.data, "base64"));
  return p;
}

async function until(expr, ms = 60000, step = 100) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (await js(expr)) return Date.now() - t0;
    await sleep(step);
  }
  throw new Error(`timeout waiting for ${expr}`);
}

const report = { url, steps: {} };
try {
  await connect();
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: downloads }).catch(() => {});
  await send("Emulation.setDeviceMetricsOverride", { width: 1600, height: 1000, deviceScaleFactor: 1, mobile: false });
  const t0 = Date.now();
  await send("Page.navigate", { url });
  await until("!!(window.filmcraftLoad && (window.filmcraftLoad.readyMs || window.filmcraftLoad.error))", 120000);
  report.load = await js("window.filmcraftLoad");
  report.load.wallMs = Date.now() - t0;
  if (report.load.error) throw new Error(report.load.error);
  report.resources = await js("performance.getEntriesByType('resource').filter(e => /wasm|\\.js/.test(e.name)).map(e => ({name: e.name.split('/').pop(), kB: Math.round(e.transferSize / 1024), ms: Math.round(e.duration)}))");
  report.info = await js("filmcraft.info()");
  // let the UI settle (fonts, first frames)
  await sleep(1500);
  report.steps.demo = { screenshot: await shot("01-demo"), inspect: await js("filmcraft.inspect().then(i => ({window: i.window, activeSequence: i.activeSequence, fps: i.fps}))") };

  // play 2 seconds
  await js("filmcraft.request('ui.playback', {action: 'play'})");
  await sleep(2000);
  const pb = await js("filmcraft.inspect().then(i => ({playhead: i.playhead, playback: i.playback, fps: i.fps}))");
  report.steps.play = { ...pb, screenshot: await shot("02-playing") };
  await js("filmcraft.request('ui.playback', {action: 'stop'})");

  // import generated media
  const imp = await js(`filmcraft.importUrl(${JSON.stringify(media)})`);
  report.steps.import = imp;
  if (!imp.items || imp.items.length === 0) throw new Error("import failed: " + JSON.stringify(imp));
  const item = imp.items[0];
  // a sequence from the clip, then look at its first frames
  report.steps.sequence = await js(`filmcraft.execute("file.newSequence", {fromItem: ${item}, name: "Web import"})`).catch((e) => ({ error: String(e) }));
  await sleep(1500);
  await js("filmcraft.request('ui.playback', {action: 'play'})");
  await sleep(1500);
  report.steps.importPlay = { ...(await js("filmcraft.inspect().then(i => ({playhead: i.playhead, playback: i.playback}))")), screenshot: await shot("03-imported") };
  await js("filmcraft.request('ui.playback', {action: 'stop'})");

  // export H.264 (stepped job; the file is offered as a download)
  const ex = await js(`filmcraft.execute("file.exportMedia", {format: "h264", path: "/exports/web-export.mp4", bitrateKbps: 4000})`);
  const te = Date.now();
  await until(`filmcraft.execute("jobs.list").then(js => js.some(j => j.id === ${ex.job} && j.finished))`, 300000, 250);
  report.steps.export = { job: ex.job, ms: Date.now() - te, jobs: await js("filmcraft.execute('jobs.list')"), files: await js("filmcraft.files()") };
  await sleep(1000);
  report.steps.export.downloads = readdirSync(downloads).filter((f) => !f.endsWith(".crdownload")).map((f) => ({ f, bytes: statSync(join(downloads, f)).size }));
  report.steps.export.screenshot = await shot("04-exported");

  // every top-level mode and a tiny window: these used to panic (temp_dir in Export mode, a dock
  // clamp below 40 points) and freeze the canvas
  for (const mode of ["import", "export", "edit"]) {
    await js(`filmcraft.request('ui.set', {mode: ${JSON.stringify(mode)}})`);
    await sleep(500);
  }
  await send("Emulation.setDeviceMetricsOverride", { width: 120, height: 80, deviceScaleFactor: 1, mobile: false });
  await sleep(500);
  await send("Emulation.setDeviceMetricsOverride", { width: 1600, height: 1000, deviceScaleFactor: 2, mobile: false });
  await sleep(500);
  await send("Emulation.setDeviceMetricsOverride", { width: 1600, height: 1000, deviceScaleFactor: 1, mobile: false });
  report.steps.modes = { screenshot: await shot("05-modes") };
  report.steps.importBins = await checkImportBins({evaluate: js, media});
  if (!report.steps.importBins.ok) throw new Error("import bin regression: " + JSON.stringify(report.steps.importBins.gates));
  report.steps.importCollision = await checkImportCollision({
    evaluate: js,
    url,
    navigate: async (nextUrl) => {
      await send("Page.navigate", { url: nextUrl });
      await until("!!(window.filmcraftLoad && (window.filmcraftLoad.readyMs || window.filmcraftLoad.error))", 120000);
      const load = await js("window.filmcraftLoad");
      if (load.error) throw new Error(load.error);
    },
  });
  if (!report.steps.importCollision.ok) throw new Error("import collision regression: " + JSON.stringify(report.steps.importCollision.gates));
  // still alive? (a panicked app never answers)
  await Promise.race([js("filmcraft.inspect().then(() => true)"), sleep(5000).then(() => { throw new Error("app stopped answering"); })]);
  const fatal = logs.filter((l) => /panicked at|RuntimeError|EXCEPTION/.test(l));
  if (fatal.length) throw new Error("panic / uncaught exception:\n" + fatal.join("\n").slice(0, 4000));
  if (await js("window.filmcraftLoad.fatal || null")) throw new Error("fatal overlay shown");
  report.ok = true;
} catch (e) {
  report.ok = false;
  report.error = String(e && e.stack || e);
  try { report.failScreenshot = await shot("99-failure"); } catch {}
}
report.console = logs.slice(-60);
writeFileSync(join(out, "report.json"), JSON.stringify(report, null, 2));
console.log(JSON.stringify(report, null, 2));
try { ws && ws.close(); } catch {}
proc.kill();
process.exit(report.ok ? 0 : 1);

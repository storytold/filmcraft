// Browser jank benchmark for the FilmCraft web app (Playwright + headless Chromium).
// Adapted from apps/filmcraft-web/tests/smoke.mjs (Phase-0 frame recorder), with the
// __fcLast skew fixed: the first rAF delta after recording starts is discarded, so it never
// spans the idle gap before `recording` flipped on (nor the script-start → first-rAF gap).
//
//   node bench.mjs --url http://127.0.0.1:18731/ --media clip1080.mp4 --label fork \
//        --runs 3 --seconds 20 [--cpu 4] --out results/fork.json
import { chromium } from "playwright-core";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

const arg = (k, d) => { const i = process.argv.indexOf(`--${k}`); return i > 0 ? process.argv[i + 1] : d; };
const url = arg("url"), media = arg("media", "clip1080.mp4"), label = arg("label", "app");
const runs = Number(arg("runs", "3")), seconds = Number(arg("seconds", "20")), cpu = Number(arg("cpu", "1"));
const out = arg("out", `results/${label}.json`);

const perfScript = `
window.__fcPerf = { frames: [], recording: false, longtasks: [] };
try {
  new PerformanceObserver((l) => {
    for (const e of l.getEntries()) if (window.__fcPerf.recording) window.__fcPerf.longtasks.push(e.duration);
  }).observe({ type: "longtask", buffered: false });
} catch {}
let __fcLast = null, __fcWasRec = false;
requestAnimationFrame(function __fcTick(t) {
  const rec = window.__fcPerf.recording;
  // only a delta between two ticks that were both inside the recording window
  if (rec && __fcWasRec && __fcLast !== null) window.__fcPerf.frames.push(t - __fcLast);
  __fcLast = t; __fcWasRec = rec;
  requestAnimationFrame(__fcTick);
});`;

const q = (s, p) => s.length ? s[Math.min(s.length - 1, Math.floor(p * s.length))] : 0;
const r1 = (x) => Math.round(x * 10) / 10;
function summarize(frames, longtasks, wallS, shown, dropped) {
  const f = [...frames].sort((a, b) => a - b);
  return {
    frames: f.length, rafFps: r1(f.length / wallS),
    p50: r1(q(f, .5)), p95: r1(q(f, .95)), p99: r1(q(f, .99)), max: r1(f.at(-1) ?? 0),
    over16_7pct: r1(100 * f.filter((d) => d > 16.7).length / (f.length || 1)),
    over33pct: r1(100 * f.filter((d) => d > 33.4).length / (f.length || 1)),
    longtasks: longtasks.length, longtaskMs: Math.round(longtasks.reduce((a, b) => a + b, 0)),
    shown, dropped, decodedFps: r1(shown / wallS),
  };
}

const browser = await chromium.launch({ headless: true, args: ["--autoplay-policy=no-user-gesture-required", "--enable-unsafe-webgpu"] });
const results = [];
for (let run = 0; run < runs; run++) {
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1 });
  await ctx.addInitScript(perfScript);
  const page = await ctx.newPage();
  const errs = [];
  page.on("pageerror", (e) => errs.push(String(e)));
  page.on("console", (m) => { if (/panicked at|RuntimeError/.test(m.text())) errs.push(m.text()); });
  await page.goto(url);
  await page.waitForFunction(() => window.filmcraftLoad && (window.filmcraftLoad.readyMs || window.filmcraftLoad.error), null, { timeout: 180000 });
  const load = await page.evaluate(() => window.filmcraftLoad);
  if (load.error) throw new Error(load.error);
  const imp = await page.evaluate((m) => filmcraft.importUrl(m), media);
  if (!imp.items?.length) throw new Error("import failed " + JSON.stringify(imp));
  await page.evaluate((it) => filmcraft.execute("file.newSequence", { fromItem: it, name: "Jank" }), imp.items[0]);
  await page.waitForTimeout(3000);
  const cdp = await ctx.newCDPSession(page);
  if (cpu > 1) await cdp.send("Emulation.setCPUThrottlingRate", { rate: cpu });
  const pb0 = await page.evaluate(() => filmcraft.request("perf.stats", {}).then((s) => s.playback));
  await page.evaluate(() => { window.__fcPerf.frames.length = 0; window.__fcPerf.longtasks.length = 0; window.__fcPerf.recording = true; });
  await page.evaluate(() => filmcraft.request("ui.playback", { action: "play" }));
  const t0 = Date.now();
  await page.waitForTimeout(seconds * 1000);
  const raw = await page.evaluate(() => { window.__fcPerf.recording = false; return { f: window.__fcPerf.frames, l: window.__fcPerf.longtasks }; });
  const wallS = (Date.now() - t0) / 1000;
  const st = await page.evaluate(() => filmcraft.request("perf.stats", {}));
  await page.evaluate(() => filmcraft.request("ui.playback", { action: "stop" }));
  if (cpu > 1) await cdp.send("Emulation.setCPUThrottlingRate", { rate: 1 });
  const s = summarize(raw.f, raw.l, wallS, st.playback.shown - pb0.shown, st.playback.dropped - pb0.dropped);
  s.playheadEnd = (await page.evaluate(() => filmcraft.inspect())).playhead;
  s.uiFps = st.ui?.fps; s.errors = errs.slice(0, 5);
  console.error(label, `cpu=${cpu}x run ${run + 1}:`, JSON.stringify(s));
  results.push(s);
  await ctx.close();
}
await browser.close();
const med = (k) => { const v = results.map((r) => r[k]).sort((a, b) => a - b); return v[Math.floor(v.length / 2)]; };
const keys = ["rafFps", "p50", "p95", "p99", "max", "over16_7pct", "over33pct", "longtasks", "longtaskMs", "decodedFps", "dropped"];
const median = Object.fromEntries(keys.map((k) => [k, med(k)]));
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, JSON.stringify({ label, url, cpu, seconds, runs: results, median, chromium: browser.version?.() }, null, 2));
console.log(JSON.stringify({ label, cpu, median }));

import { chromium } from 'playwright';
const [,, url, label, out] = process.argv;
const browser = await chromium.launch({ headless: process.env.HEADED ? false : true, executablePath: '/home/box/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome', args: ['--window-position=0,0','--window-size=1280,900','--enable-unsafe-webgpu','--autoplay-policy=no-user-gesture-required','--use-gl=angle','--use-angle=swiftshader','--ignore-gpu-blocklist'] });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, recordVideo: { dir: out, size: { width: 1280, height: 800 } } });
await ctx.addInitScript((label) => {
  const f = []; let last = 0; window.__fc = { f, rec: false };
  requestAnimationFrame(function t(n) { if (last && window.__fc.rec) f.push(n - last); last = n; requestAnimationFrame(t); });
  addEventListener('DOMContentLoaded', () => {
    const d = document.createElement('div');
    d.style.cssText = 'position:fixed;right:8px;top:44px;z-index:99999;background:rgba(0,0,0,.75);color:#0f0;font:bold 16px monospace;padding:6px 10px;border-radius:6px;pointer-events:none;white-space:pre';
    document.body.appendChild(d);
    setInterval(() => { const w = f.slice(-60); if (!w.length) { d.textContent = label + '\nwaiting'; return; }
      const avg = w.reduce((a, b) => a + b, 0) / w.length; const s = [...f].sort((a,b)=>a-b);
      d.textContent = `${label}\n${(1000/avg).toFixed(1)} fps\n${avg.toFixed(1)} ms/frame\np95 ${s[Math.floor(s.length*.95)]?.toFixed(1)} ms`; }, 500);
  });
}, label);
const page = await ctx.newPage();
page.on('pageerror', e => console.error('pageerror', e.message));
const t0 = Date.now();
await page.goto(url);
await page.waitForFunction(() => window.filmcraftLoad && (window.filmcraftLoad.readyMs || window.filmcraftLoad.error), null, { timeout: 180000 });
console.error('load', JSON.stringify(await page.evaluate(() => window.filmcraftLoad)));
const imp = await page.evaluate(() => filmcraft.importUrl('clip.mp4'));
console.error('import', JSON.stringify(imp).slice(0, 300));
await page.evaluate((it) => filmcraft.execute('file.newSequence', { fromItem: it, name: 'bench' }), imp.items[0]);
await page.waitForTimeout(3000);
const tStart = (Date.now() - t0) / 1000;
await page.evaluate(() => { window.__fc.rec = true; return filmcraft.request('ui.playback', { action: 'play' }); });
await page.waitForTimeout(Number(process.env.DUR||30000)); await page.screenshot({path: out + '/shot.png'});
const res = await page.evaluate(async () => { const f = [...window.__fc.f].sort((a,b)=>a-b); const i = await filmcraft.inspect(); const st = await filmcraft.request('perf.stats', {}).catch(() => null);
  return { n: f.length, avg: f.reduce((a,b)=>a+b,0)/f.length, p50: f[f.length>>1], p95: f[Math.floor(f.length*.95)], playhead: i.playhead, stats: st }; });
console.log(JSON.stringify({ label, tStart, ...res }));
await page.evaluate(() => filmcraft.request('ui.playback', { action: 'stop' }));
await ctx.close(); await browser.close();

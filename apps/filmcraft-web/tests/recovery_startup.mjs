// Reproduce recovery-startup font readiness and preservation in a headless browser.
// Build and serve the web app first, then run: node apps/filmcraft-web/tests/recovery_startup.mjs
// Node 22+ supplies fetch and WebSocket; no npm packages or visible browser window are needed.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const url = process.argv[2] ?? "http://127.0.0.1:8765/?empty&webgl";
const chrome = process.argv[3] ?? (process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "google-chrome");
const port = 9347;
const profile = mkdtempSync(join(tmpdir(), "filmcraft-recovery-"));
const downloads = join(profile, "downloads");
mkdirSync(downloads);
const child = spawn(chrome, [
  "--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  "--no-first-run", "--no-default-browser-check", "--window-size=1260,806", "about:blank",
], { stdio: "ignore" });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let socket;
let nextId = 0;
const pending = new Map();

async function connect() {
  for (let tries = 0; tries < 100; tries++) {
    try {
      const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      const page = pages.find((entry) => entry.type === "page");
      if (page) {
        socket = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
        socket.onmessage = ({ data }) => {
          const message = JSON.parse(data);
          if (!message.id || !pending.has(message.id)) return;
          pending.get(message.id)(message);
          pending.delete(message.id);
        };
        return;
      }
    } catch {}
    await sleep(100);
  }
  throw new Error("headless Chrome did not start");
}

function send(method, params = {}) {
  const id = ++nextId;
  socket.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve, reject) => {
    pending.set(id, (message) => message.error ? reject(new Error(`${method}: ${message.error.message}`)) : resolve(message.result));
  });
}

async function js(expression) {
  const result = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
  return result.result.value;
}

async function until(check, timeoutMs = 15000) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await check()) return;
    await sleep(100);
  }
  throw new Error("browser recovery test timed out");
}

try {
  await connect();
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: downloads });
  await send("Page.navigate", { url });
  await until(async () => await js("!!window.filmcraftLoad?.readyMs"));
  assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);

  await js(`(async () => {
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("recovery", {create:true});
    for (const [name, data] of [
      ["snapshot.fcproj", "invalid-fcproj-test"],
      ["meta.json", JSON.stringify({dirty:true,name:"Unsafe dimensions",revision:7,time:1})],
    ]) {
      const file = await dir.getFileHandle(name, {create:true});
      const writer = await file.createWritable();
      await writer.write(data);
      await writer.close();
    }
  })()`);

  const previousOrigin = await js("performance.timeOrigin");
  await send("Page.reload", { ignoreCache: true });
  await until(async () => await js(`performance.timeOrigin !== ${previousOrigin} && !!window.filmcraftLoad?.readyMs`));
  await sleep(800);
  assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null, "recovery overlay must not render before its fonts exist");
  await js("window.filmcraft.inspect()");
  let elements = await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})");
  assert.deepEqual(elements.map((element) => element.id), ["recovery.download"]);

  await js("window.filmcraft.execute('file.openDemoProject',{})");
  await js("window.filmcraft.execute('file.newSequence',{name:'Held unsaved edit'})");
  await sleep(6500);
  const opfsText = (name) => `
    (async () => {
      const dir=await (await navigator.storage.getDirectory()).getDirectoryHandle('recovery');
      return (await (await dir.getFileHandle(${JSON.stringify(name)})).getFile()).text();
    })()`;
  assert.equal(await js(opfsText("snapshot.fcproj")), "invalid-fcproj-test", "an unsaved edit must not replace the held recovery file");
  assert.equal(await js(opfsText("meta.json")), JSON.stringify({dirty:true,name:"Unsafe dimensions",revision:7,time:1}), "the held dirty marker must remain intact");

  await js("window.filmcraft.request('ui.click',{id:'recovery.download'})");
  await until(async () => (await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).some((element) => element.id === "recovery.continue"));
  await until(async () => readdirSync(downloads).some((name) => name.endsWith(".fcproj")));
  const saved = readdirSync(downloads).find((name) => name.endsWith(".fcproj"));
  assert.equal(readFileSync(join(downloads, saved), "utf8"), "invalid-fcproj-test");

  await js("window.filmcraft.request('ui.click',{id:'recovery.continue'})");
  await until(async () => (await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).length === 0);
  await until(async () => await js(opfsText("snapshot.fcproj")) !== "invalid-fcproj-test", 15000);
  assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);
  console.log("recovery startup, held snapshot, download, and resumed autosave: PASS");
} catch (error) {
  console.error(error);
  process.exitCode = 1;
} finally {
  socket?.close();
  child.kill();
  await sleep(300);
  rmSync(profile, { recursive: true, force: true });
}

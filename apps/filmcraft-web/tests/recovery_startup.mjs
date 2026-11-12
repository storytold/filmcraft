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
const profile = mkdtempSync(join(tmpdir(), "filmcraft-recovery-"));
const downloads = join(profile, "downloads");
mkdirSync(downloads);
const child = spawn(chrome, [
  "--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
  "--no-first-run", "--no-default-browser-check", "--window-size=1260,806", "about:blank",
], { stdio: "ignore" });

let launchError;
child.on("error", (error) => { launchError = error; });
const exited = new Promise((resolve) => {
  child.once("error", resolve);
  child.once("exit", resolve);
});
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let socket;
let nextId = 0;
const pending = new Map();

async function connect() {
  for (let tries = 0; tries < 100; tries++) {
    if (launchError) throw launchError;
    if (child.exitCode !== null || child.signalCode !== null) throw new Error("headless Chrome exited before connecting");
    try {
      const [port, endpoint] = readFileSync(join(profile, "DevToolsActivePort"), "utf8").trim().split("\n");
      assert.match(port, /^[0-9]+$/);
      assert.ok(endpoint?.startsWith("/devtools/browser/"));
      const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
      assert.equal(new URL(version.webSocketDebuggerUrl).pathname, endpoint);
      const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      const page = pages.find((entry) => entry.type === "page" && entry.url === "about:blank");
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
    const timeout = setTimeout(() => { pending.delete(id); reject(new Error(`${method}: browser request timed out`)); }, 30000);
    pending.set(id, (message) => {
      clearTimeout(timeout);
      message.error ? reject(new Error(`${method}: ${message.error.message}`)) : resolve(message.result);
    });
  });
}

async function js(expression) {
  const result = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
  return result.result.value;
}

async function until(check, timeoutMs = 15000, message = "browser recovery test timed out") {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await check()) return;
    await sleep(100);
  }
  throw new Error(message);
}

async function testCustomRunner() {
  await send("Page.navigate", { url });
  await until(async () => await js("!!window.customModuleReady"));
  await js(`(async () => {
    const root = await navigator.storage.getDirectory();
    for (const [dir, name, data] of [
      ["recovery", "snapshot.fcproj", "held-before-custom"],
      ["recovery", "meta.json", JSON.stringify({dirty:true,name:"Previous work",revision:7})],
      ["media", "take.wav", "original media"],
    ]) {
      const folder = await root.getDirectoryHandle(dir, {create:true});
      const writer = await (await folder.getFileHandle(name, {create:true})).createWritable();
      await writer.write(data);
      await writer.close();
    }
  })()`);
  assert.equal(await js("(() => { try { return window.probeExclusiveWrites(); } catch (error) { return String(error); } })()"), true);
  console.log("exclusive writes reject reserved and occupied paths while preserving new-path writes: PASS");
  await js("window.startCustomRunner()");
  await until(async () => await js("!!window.filmcraftLoad.readyMs"));
  await js("window.filmcraft.execute('file.newSequence',{name:'Custom runner edit'})");
  const imported = await js(`window.filmcraft.importFiles([new File([
    new Uint8Array([82,73,70,70,38,0,0,0,87,65,86,69,102,109,116,32,16,0,0,0,1,0,1,0,128,187,0,0,0,119,1,0,2,0,16,0,100,97,116,97,2,0,0,0,0,0])
  ],"take.wav",{type:"audio/wav"})])`);
  assert.deepEqual(imported.errors, []);
  assert.deepEqual(imported.paths, ["/session-files/take.wav"]);
  await sleep(6500);
  const persisted = await js(`(async () => {
    const root = await navigator.storage.getDirectory();
    const read = async (dir, name) => (await (await (await root.getDirectoryHandle(dir)).getFileHandle(name)).getFile()).text();
    return {snapshot:await read("recovery","snapshot.fcproj"),media:await read("media","take.wav"),meta:await read("recovery","meta.json")};
  })()`);
  assert.equal(persisted.snapshot, "held-before-custom", "custom constructor must not autosave before recovery initialization");
  assert.equal(persisted.media, "original media", "custom imports must not persist before media names are protected");
  assert.equal(persisted.meta, JSON.stringify({dirty:true,name:"Previous work",revision:7}));
  console.log("public custom-runner constructor preserves existing snapshot, metadata and media: PASS");
}

async function scrollRecoveryActionIntoView(id) {
  await until(async () => {
    const elements = await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})");
    const action = elements.find((element) => element.id === id);
    if (!action) return false;
    const [x, y, width, height] = action.rect;
    if (x >= 0 && x + width <= 520 && y >= 0 && y + height <= 620) return true;
    const wheelX = Math.max(40, Math.min(480, x + width / 2));
    await send("Input.dispatchMouseEvent", {type:"mouseMoved",x:wheelX,y:310});
    await send("Input.dispatchMouseEvent", {
      type: "mouseWheel", x: wheelX, y: 310,
      deltaX: 0, deltaY: 10000,
    });
    return false;
  }, 10000, "recovery actions must remain reachable after scrolling a long diagnostic");
}

async function testRecoveryWriteFailures() {
  const read = (name) => `(async () => {
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("recovery");
    return (await (await dir.getFileHandle(${JSON.stringify(name)})).getFile()).text();
  })()`;
  for (const target of ["meta.json", "snapshot.fcproj"]) {
    for (const stage of ["createWritable", "write", "close"]) {
      const before = await js(read("snapshot.fcproj"));
      const beforeMeta = await js(read("meta.json"));
      await js(`(() => {
        const original = FileSystemFileHandle.prototype.createWritable;
        window.recoveryWriteFailures = 0;
        window.recoveryWriteAttempts = [];
        window.restoreRecoveryWriter = () => { FileSystemFileHandle.prototype.createWritable = original; };
        FileSystemFileHandle.prototype.createWritable = async function(...args) {
          window.recoveryWriteAttempts.push(this.name);
          if (this.name !== ${JSON.stringify(target)}) return original.apply(this, args);
          if (${JSON.stringify(stage)} === "createWritable") {
            window.recoveryWriteFailures++;
            throw new Error("injected recovery createWritable failure");
          }
          const writer = await original.apply(this, args);
          writer[${JSON.stringify(stage)}] = async function() {
            await writer.abort();
            window.recoveryWriteFailures++;
            throw new Error("injected recovery stream failure");
          };
          return writer;
        };
      })()`);
      const sequence = `Retry ${target} ${stage}`;
      try {
        await js(`window.filmcraft.execute("file.newSequence",{name:${JSON.stringify(sequence)}})`);
        await until(async () => await js("window.recoveryWriteFailures > 0"), 15000, "the actual recovery writer must encounter the injected failure");
        assert.equal(await js(read("snapshot.fcproj")), before, "failed recovery writes must preserve the previous snapshot");
        if (target === "meta.json") {
          assert.equal(await js(read("meta.json")), beforeMeta, "failed dirty metadata writes must preserve previous metadata");
          assert.equal(await js('window.recoveryWriteAttempts.includes("snapshot.fcproj")'), false,
            "snapshot replacement must not start before dirty metadata succeeds");
        } else {
          assert.equal(JSON.parse(await js(read("meta.json"))).dirty, true);
        }
      } finally {
        await js("window.restoreRecoveryWriter()");
      }
      await until(async () => (await js(read("snapshot.fcproj"))).includes(sequence), 15000,
        "recovery must retry the same revision after the writer recovers");
      console.log(`recovery ${target} ${stage} failure preserves bytes and retries: PASS`);
    }
  }
}

try {
  await connect();
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: downloads });
  if (process.argv[4] === "custom") {
    await testCustomRunner();
  } else {
    await send("Page.navigate", { url });
    await until(async () => await js("!!window.filmcraftLoad?.readyMs"));
    assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);

    // A crash after the first dirty-meta write can leave no snapshot to preserve.
    await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("recovery", {create:true});
      try { await dir.removeEntry("snapshot.fcproj"); } catch (error) {
        if (error.name !== "NotFoundError") throw error;
      }
      const file = await dir.getFileHandle("meta.json", {create:true});
      const writer = await file.createWritable();
      await writer.write(JSON.stringify({dirty:true,name:"Interrupted first snapshot",revision:7,time:1}));
      await writer.close();
    })()`);
    const orphanOrigin = await js("performance.timeOrigin");
    await send("Page.reload", { ignoreCache: true });
    await until(async () => await js(`performance.timeOrigin !== ${orphanOrigin} && !!window.filmcraftLoad?.readyMs`));
    await sleep(800);
    assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);
    await js("window.filmcraft.inspect()");
    assert.deepEqual(await js("window.filmcraft.request(\'ui.elements\',{prefix:\'recovery.\'})"), []);
    await js("window.filmcraft.execute(\'file.newSequence\',{name:\'Autosave after missing snapshot\'})");
    await until(async () => await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("recovery");
      try { return (await (await dir.getFileHandle("snapshot.fcproj")).getFile()).size > 0; }
      catch (error) { if (error.name === "NotFoundError") return false; throw error; }
    })()`), 15000, "metadata-only startup must resume autosave");
    console.log("metadata-only startup resumes autosave: PASS");
    const validSnapshot = await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("recovery");
      return (await (await dir.getFileHandle("snapshot.fcproj")).getFile()).text();
    })()`);
    assert.ok(JSON.parse(validSnapshot).project, "resumed autosave must write a serialized project");
    const invalidSnapshot = '{"project":';
    const openError = await js(`window.filmcraft.openProject(new File([${JSON.stringify(invalidSnapshot)}], "truncated-project.fcproj"))
      .then(() => null, (error) => String(error))`);
    assert.match(openError, /EOF|parse|JSON|invalid|project/i,
      "the truncated fixture must be rejected before testing recovery preservation");

    await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("recovery", {create:true});
      for (const [name, data] of [
        ["snapshot.fcproj", ${JSON.stringify(invalidSnapshot)}],
        ["meta.json", JSON.stringify({dirty:true,name:"Unreadable project",revision:7,time:1})],
      ]) {
        const file = await dir.getFileHandle(name, {create:true});
        const writer = await file.createWritable();
        await writer.write(data);
        await writer.close();
      }
    })()`);

    const hostSetup = await send("Page.addScriptToEvaluateOnNewDocument", { source: `
      new MutationObserver((_, observer) => {
        const canvas = document.querySelector('canvas');
        if (!canvas) return;
        canvas.setAttribute('data-host', 'page');
        canvas.addEventListener('filmcraft:ready', () => { window.testHostReady = true; });
        observer.disconnect();
      }).observe(document, {childList:true, subtree:true});
    ` });
    await send("Page.reload", { ignoreCache: true });
    await until(async () => await js("!!window.testHostReady && !!window.filmcraftLoad?.readyMs"));
    assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);
    assert.equal(await js("window.filmcraft.info().restoredMedia"), 0);
    assert.deepEqual(await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})"), []);
    await js("window.filmcraft.execute('file.newSequence',{name:'Host-owned unsaved edit'})");
    const hostImport = await js(`window.filmcraft.importFiles([new File([
      new Uint8Array([82,73,70,70,38,0,0,0,87,65,86,69,102,109,116,32,16,0,0,0,1,0,1,0,128,187,0,0,0,119,1,0,2,0,16,0,100,97,116,97,2,0,0,0,0,0])
    ],"host-import.wav",{type:"audio/wav"})])`);
    assert.deepEqual(hostImport.errors, []);
    assert.deepEqual(hostImport.paths, ["/session-files/host-import.wav"]);
    await sleep(6500);
    assert.equal(await js(`(async () => {
      try {
        const media = await (await navigator.storage.getDirectory()).getDirectoryHandle("media");
        await media.getFileHandle("host-import.wav");
        return true;
      } catch (error) { if (error.name === "NotFoundError") return false; throw error; }
    })()`), false, "host imports must not write standalone recovery media");
    for (const [name, expected] of [
      ['snapshot.fcproj', invalidSnapshot],
      ['meta.json', JSON.stringify({dirty:true,name:'Unreadable project',revision:7,time:1})],
    ]) {
      assert.equal(await js(`(async () => {
        const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle('recovery');
        return (await (await dir.getFileHandle(${JSON.stringify(name)})).getFile()).text();
      })()`), expected, 'host mode must leave browser recovery untouched');
    }
    await send("Page.removeScriptToEvaluateOnNewDocument", { identifier: hostSetup.identifier });
    console.log("host mode bypasses recovery and preserves its snapshot during edits: PASS");

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
    assert.equal(await js(opfsText("snapshot.fcproj")), invalidSnapshot, "an unsaved edit must not replace the held recovery file");
    assert.equal(await js(opfsText("meta.json")), JSON.stringify({dirty:true,name:"Unreadable project",revision:7,time:1}), "the held dirty marker must remain intact");

    await js(`(() => {
      const create = URL.createObjectURL.bind(URL);
      const revoke = URL.revokeObjectURL.bind(URL);
      window.downloadUrls = new Set();
      window.downloadUrlCreates = 0;
      URL.createObjectURL = (blob) => {
        const url = create(blob);
        window.downloadUrls.add(url);
        window.downloadUrlCreates++;
        return url;
      };
      URL.revokeObjectURL = (url) => { window.downloadUrls.delete(url); return revoke(url); };
    })()`);
    await send("Emulation.setDeviceMetricsOverride", {width:520,height:620,deviceScaleFactor:1,mobile:false});
    const holdWindow = async () => (await js("window.filmcraft.request('ui.elements',{prefix:'browserRecovery.holdWindow'})"))[0];
    const shortDiagnosticHeight = (await holdWindow()).rect[3];
    let longDiagnosticHeight = 0;
    for (const failure of ["anchor", "style", "append"]) {
      await js(`(() => {
        const create = Document.prototype.createElement;
        const style = CSSStyleDeclaration.prototype.setProperty;
        const append = Node.prototype.appendChild;
        window.injectedDownloadFailures = 0;
        Document.prototype.createElement = function(name, ...args) {
          if (${JSON.stringify(failure)} === "anchor" && name === "a") {
            window.injectedDownloadFailures++;
            throw new Error("injected anchor creation failure ".repeat(200));
          }
          return create.call(this, name, ...args);
        };
        CSSStyleDeclaration.prototype.setProperty = function(name, value, ...args) {
          if (${JSON.stringify(failure)} === "style" && name === "display" && value === "none") {
            window.injectedDownloadFailures++;
            throw "injected anchor styling failure";
          }
          return style.call(this, name, value, ...args);
        };
        Node.prototype.appendChild = function(node) {
          if (${JSON.stringify(failure)} === "append" && node.tagName === "A") {
            window.injectedDownloadFailures++;
            throw "injected anchor append failure";
          }
          return append.call(this, node);
        };
        window.restoreDownloadSetup = () => {
          Document.prototype.createElement = create;
          CSSStyleDeclaration.prototype.setProperty = style;
          Node.prototype.appendChild = append;
        };
      })()`);
      const createsBefore = await js("window.downloadUrlCreates");
      try {
        await scrollRecoveryActionIntoView("recovery.download");
        await js("window.filmcraft.request('ui.click',{id:'recovery.download'})");
        assert.equal(await js("window.injectedDownloadFailures"), 1, `${failure} must reach the injected failure`);
        if (failure === "anchor") {
          await until(async () => (await holdWindow()).rect[3] > shortDiagnosticHeight + 50,
            10000, "a long diagnostic must expand its content-fitting recovery window");
          longDiagnosticHeight = (await holdWindow()).rect[3];
          const [x, y, width, height] = (await holdWindow()).rect;
          assert.ok(x >= 0 && x + width <= 520 && y >= 0 && y + height <= 620,
            "the expanded recovery window must stay inside the viewport");
          await scrollRecoveryActionIntoView("recovery.download");
        } else {
          await until(async () => (await holdWindow()).rect[3] < longDiagnosticHeight - 50,
            10000, "a short diagnostic must shrink the same recovery window again");
        }
        assert.equal(await js("window.downloadUrls.size"), 0, `${failure} must not leak an object URL`);
        assert.equal(await js("window.downloadUrlCreates"), createsBefore + (failure === "append" ? 1 : 0));
        assert.deepEqual((await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).map((element) => element.id), ["recovery.download"]);
        assert.equal(await js(opfsText("snapshot.fcproj")), invalidSnapshot);
      } finally {
        await js("window.restoreDownloadSetup()");
      }
    }
    await send("Emulation.setDeviceMetricsOverride", {width:1260,height:806,deviceScaleFactor:1,mobile:false});
    console.log("failed recovery download setup preserves bytes, cleans URLs, and fits short-long-short diagnostics: PASS");
    await js("window.filmcraft.request('ui.click',{id:'recovery.download'})");
    await until(async () => (await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).some((element) => element.id === "recovery.continue"));
    await until(async () => readdirSync(downloads).some((name) => name.endsWith(".fcproj")));
    const saved = readdirSync(downloads).find((name) => name.endsWith(".fcproj"));
    assert.equal(readFileSync(join(downloads, saved), "utf8"), invalidSnapshot);

    await js("window.filmcraft.request('ui.click',{id:'recovery.continue'})");
    await until(async () => (await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).length === 0);
    await until(async () => await js(opfsText("snapshot.fcproj")) !== invalidSnapshot, 15000);
    assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);
    console.log("recovery startup, held snapshot, download, and resumed autosave: PASS");
    await testRecoveryWriteFailures();

    for (const name of ["", "   ", "folder/name", "folder\\name", "bad\nname", "x".repeat(513)]) {
      for (const file of readdirSync(downloads)) rmSync(join(downloads, file));
      await js(`(async () => {
        const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("recovery", {create:true});
        for (const [fileName, data] of [
          ["snapshot.fcproj", "legacy-recovery-test"],
          ["meta.json", JSON.stringify({dirty:true,name:${JSON.stringify(name)},revision:7})],
        ]) {
          const writer = await (await dir.getFileHandle(fileName, {create:true})).createWritable();
          await writer.write(data);
          await writer.close();
        }
      })()`);
      const oldOrigin = await js("performance.timeOrigin");
      await send("Page.reload", { ignoreCache: true });
      await until(async () => await js(`performance.timeOrigin !== ${oldOrigin} && !!window.filmcraftLoad?.readyMs`));
      await until(async () => (await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).some((element) => element.id === "recovery.download"));
      await js("window.filmcraft.request('ui.click',{id:'recovery.download'})");
      await until(async () => readdirSync(downloads).includes("Recovered.fcproj"));
      assert.equal(readFileSync(join(downloads, "Recovered.fcproj"), "utf8"), "legacy-recovery-test");
      assert.equal(await js(opfsText("snapshot.fcproj")), "legacy-recovery-test");
      assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);
    }
    console.log("legacy recovery names are sanitized on read and download: PASS");

    await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("media", {create:true});
      const writer = await (await dir.getFileHandle("take.wav", {create:true})).createWritable();
      await writer.write("original recovery media");
      await writer.close();
    })()`);
    const freshUrl = new URL(url);
    freshUrl.searchParams.set("fresh", "");
    const freshOrigin = await js("performance.timeOrigin");
    await send("Page.navigate", { url: freshUrl.href });
    await until(async () => await js(`performance.timeOrigin !== ${freshOrigin} && !!window.filmcraftLoad?.readyMs`));
    assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);
    assert.equal(await js("window.filmcraft.info().restoredMedia"), 0);
    assert.ok(!(await js("window.filmcraft.files()")).some((file) => file.path === "/files/take.wav"),
      "fresh startup must reserve media without exposing it");
    const imported = await js(`(async () => {
      const bytes = new Uint8Array(46);
      const view = new DataView(bytes.buffer);
      const text = (offset, value) => [...value].forEach((c, i) => bytes[offset + i] = c.charCodeAt(0));
      text(0, "RIFF"); view.setUint32(4, 38, true); text(8, "WAVEfmt ");
      view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
      view.setUint32(24, 48000, true); view.setUint32(28, 96000, true);
      view.setUint16(32, 2, true); view.setUint16(34, 16, true); text(36, "data");
      view.setUint32(40, 2, true);
      return window.filmcraft.importFiles([new File([bytes], "take.wav", {type:"audio/wav"})]);
    })()`);
    assert.deepEqual(imported.errors, []);
    assert.deepEqual(imported.paths, ["/files/take (2).wav"]);
    await until(async () => await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("media");
      try { return (await (await dir.getFileHandle("take (2).wav")).getFile()).size === 46; }
      catch (error) { if (error.name === "NotFoundError") return false; throw error; }
    })()`));
    assert.equal(await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("media");
      return (await (await dir.getFileHandle("take.wav")).getFile()).text();
    })()`), "original recovery media");
    assert.equal(await js(opfsText("snapshot.fcproj")), "legacy-recovery-test");
    console.log("fresh startup reserves recovery media names and preserves original bytes: PASS");

    await assert.rejects(() => js("window.filmcraft.execute(\'file.saveCopy\',{path:\'/opfs/media/take.wav\'})"), /reserved/);
    const beforeRejectedSave = await js("window.filmcraft.execute('project.inspect',{})");
    await assert.rejects(() => js("window.filmcraft.execute('file.saveAs',{path:'/opfs/recovery/snapshot.fcproj'})"), /reserved/);
    assert.deepEqual(await js("window.filmcraft.execute('project.inspect',{})"), beforeRejectedSave,
      "a rejected Save As must preserve the current project identity and dirty state");
    const waveBytes = await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("media");
      const file = await (await dir.getFileHandle("take (2).wav")).getFile();
      return Array.from(new Uint8Array(await file.arrayBuffer()));
    })()`);
    assert.deepEqual(waveBytes, [82,73,70,70,38,0,0,0,87,65,86,69,102,109,116,32,16,0,0,0,1,0,1,0,128,187,0,0,0,119,1,0,2,0,16,0,100,97,116,97,2,0,0,0,0,0]);
    const unreadableSetup = await send("Page.addScriptToEvaluateOnNewDocument", { source: `
      const getFile = FileSystemFileHandle.prototype.getFile;
      window.injectedMediaReads = 0;
      FileSystemFileHandle.prototype.getFile = function(...args) {
        if (this.name === "take.wav") {
          window.injectedMediaReads++;
          return Promise.reject(new Error("injected recovery media read failure"));
        }
        return getFile.apply(this, args);
      };
    ` });
    try {
      const unreadableOrigin = await js("performance.timeOrigin");
      await send("Page.navigate", { url });
      await until(async () => await js(`performance.timeOrigin !== ${unreadableOrigin} && !!window.filmcraftLoad?.readyMs`));
      assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);
      assert.ok(await js("window.injectedMediaReads > 0"));
      assert.ok(!(await js("window.filmcraft.files()")).some((file) => file.path === "/files/take.wav"));
      const retriedImport = await js(`window.filmcraft.importFiles([
        new File([new Uint8Array(${JSON.stringify(waveBytes)})], "take.wav", {type:"audio/wav"})
      ])`);
      assert.deepEqual(retriedImport.errors, []);
      assert.deepEqual(retriedImport.paths, ["/files/take (3).wav"]);
      await until(async () => await js(`(async () => {
        const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("media");
        try { return (await (await dir.getFileHandle("take (3).wav")).getFile()).size === 46; }
        catch (error) { if (error.name === "NotFoundError") return false; throw error; }
      })()`));
      await js("delete window.injectedMediaReads");
      assert.equal(await js(opfsText("snapshot.fcproj")), "legacy-recovery-test");
    } finally {
      await send("Page.removeScriptToEvaluateOnNewDocument", { identifier: unreadableSetup.identifier });
    }
    const verifyOrigin = await js("performance.timeOrigin");
    await send("Page.navigate", { url: freshUrl.href });
    await until(async () => await js(`performance.timeOrigin !== ${verifyOrigin} && !!window.filmcraftLoad?.readyMs`));
    assert.equal(await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("media");
      return (await (await dir.getFileHandle("take.wav")).getFile()).text();
    })()`), "original recovery media");
    console.log("unreadable recovery media retain their names and bytes during new imports: PASS");

    await send("Emulation.setDeviceMetricsOverride", {width:520,height:620,deviceScaleFactor:1,mobile:false});
    const heldMeta = await js(opfsText("meta.json"));
    const retainedWaveBytes = [...waveBytes];
    retainedWaveBytes[retainedWaveBytes.length - 1] = 1;
    await js(`(async () => {
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("media");
      const writer = await (await dir.getFileHandle("take.wav")).createWritable();
      await writer.write(new Uint8Array(${JSON.stringify(retainedWaveBytes)}));
      await writer.close();
    })()`);
    const degradedCopies = [];
    const failedEnumeration = await send("Page.addScriptToEvaluateOnNewDocument", { source: `
      const values = FileSystemDirectoryHandle.prototype.values;
      window.injectedEnumerationFailures = 0;
      FileSystemDirectoryHandle.prototype.values = function(...args) {
        if (this.name === "media") {
          window.injectedEnumerationFailures++;
          throw new DOMException("injected media enumeration failure ".repeat(200), "NotAllowedError");
        }
        return values.apply(this, args);
      };
    ` });
    try {
      for (const failureUrl of [freshUrl.href, url]) {
        const failedOrigin = await js("performance.timeOrigin");
        await send("Page.navigate", { url: failureUrl });
        await until(async () => await js(`performance.timeOrigin !== ${failedOrigin} && !!window.filmcraftLoad?.readyMs`));
        assert.equal(await js("window.filmcraftLoad.fatal ?? null"), null);
        assert.ok(await js("window.injectedEnumerationFailures > 0"));
        assert.ok((await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).some((element) => element.id === "recovery.download"),
          "storage failures must still expose the existing recovery download");
        await scrollRecoveryActionIntoView("recovery.download");
        rmSync(join(downloads, "Recovered.fcproj"), {force:true});
        await js("window.filmcraft.request('ui.click',{id:'recovery.download'})");
        await until(() => readdirSync(downloads).includes("Recovered.fcproj"));
        assert.equal(readFileSync(join(downloads, "Recovered.fcproj"), "utf8"), "legacy-recovery-test");
        await scrollRecoveryActionIntoView("recovery.continue");
        await js("window.filmcraft.request('ui.click',{id:'recovery.continue'})");
        await until(async () => (await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).length === 0);
        await js("window.filmcraft.execute('file.newSequence',{name:'Editing after storage failure'})");
        const manualImport = await js(`window.filmcraft.importFiles([
          new File([new Uint8Array(${JSON.stringify(waveBytes)})], "take.wav", {type:"audio/wav"})
        ])`);
        assert.deepEqual(manualImport.errors, []);
        assert.ok(manualImport.paths[0].startsWith("/session-files/"),
          "unverified import names must be disjoint from retained media paths");
        assert.deepEqual(await js(`(async () => Array.from(await window.filmcraft.readFile(${JSON.stringify(manualImport.paths[0])})))()`), waveBytes);
        const browser = await js("window.filmcraft.execute('mediaBrowser.list',{})");
        assert.equal(browser.dir, "/session-files");
        assert.ok(browser.entries.some((entry) => entry.path === manualImport.paths[0]));
        await sleep(6500);
        assert.equal(await js(opfsText("snapshot.fcproj")), "legacy-recovery-test");
        assert.equal(await js(opfsText("meta.json")), heldMeta);
        assert.deepEqual(await js(`(async () => {
          const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("media");
          return Array.from(new Uint8Array(await (await (await dir.getFileHandle("take.wav")).getFile()).arrayBuffer()));
        })()`), retainedWaveBytes);
        rmSync(join(downloads, "manual.fcproj"), {force:true});
        await js("window.filmcraft.execute('file.saveCopy',{path:'/exports/manual.fcproj'})");
        await until(() => readdirSync(downloads).includes("manual.fcproj"));
        const manualText = readFileSync(join(downloads, "manual.fcproj"), "utf8");
        degradedCopies.push({text:manualText, path:manualImport.paths[0]});
        const manual = JSON.parse(manualText);
        assert.ok(Object.values(manual.project.items).some((item) => item.name === "Editing after storage failure"));
      }
    } finally {
      await send("Page.removeScriptToEvaluateOnNewDocument", { identifier: failedEnumeration.identifier });
    }
    await send("Emulation.setDeviceMetricsOverride", {width:1260,height:806,deviceScaleFactor:1,mobile:false});
    const reopenOrigin = await js("performance.timeOrigin");
    await send("Page.navigate", {url});
    await until(async () => await js(`performance.timeOrigin !== ${reopenOrigin} && !!window.filmcraftLoad?.readyMs`));
    assert.deepEqual(await js("(async () => Array.from(await window.filmcraft.readFile('/files/take.wav')))()"), retainedWaveBytes);
    for (const copy of degradedCopies) {
      const opened = await js(`window.filmcraft.openProject(new File([${JSON.stringify(copy.text)}],"manual-reopened.fcproj"))`);
      assert.equal(opened.missingMedia, 1);
      const missing = await js("window.filmcraft.execute('media.findMissing',{})");
      assert.ok(missing.missing.some((item) => item.path === copy.path));
      const status = await js("window.filmcraft.execute('media.status',{})");
      assert.equal(status.find((item) => item.path === copy.path)?.status, "missing",
        "a manual save must never bind a session import to older retained media");
    }
    console.log("normal and fresh storage failures preserve reachable actions, isolate imports across save/reopen, pause persistence and permit manual save: PASS");

    const entryLimitSetup = await send("Page.addScriptToEvaluateOnNewDocument", { source: `
      const values = FileSystemDirectoryHandle.prototype.values;
      window.mediaLimitCalls = 0;
      FileSystemDirectoryHandle.prototype.values = function(...args) {
        if (this.name !== "media") return values.apply(this, args);
        let file;
        return {next:async () => {
          window.mediaLimitCalls++;
          file ??= await this.getFileHandle("take.wav");
          return {done:false,value:file};
        }};
      };
    ` });
    try {
      const limitOrigin = await js("performance.timeOrigin");
      await send("Page.navigate", { url: freshUrl.href });
      await until(async () => await js(`performance.timeOrigin !== ${limitOrigin} && !!window.filmcraftLoad?.readyMs`), 45000);
      assert.equal(await js("window.mediaLimitCalls"), 100001, "an endless media iterator must stop at the documented directory bound");
      await send("Emulation.setDeviceMetricsOverride", {width:520,height:620,deviceScaleFactor:1,mobile:false});
      await js("window.filmcraft.request('ui.menu.invoke',{id:'app.language.german'})");
      await js("window.filmcraft.execute('prefs.set',{key:'appearance.uiScale',value:'150'})");
      await sleep(500);
      const recovery = await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})");
      assert.ok(recovery.some((element) => element.id === "recovery.download"));
      assert.ok(recovery.some((element) => element.label.includes("Wiederherstellungsdatei")));
      rmSync(join(downloads, "Recovered.fcproj"), {force:true});
      await js("window.filmcraft.request('ui.click',{id:'recovery.download'})");
      await until(() => readdirSync(downloads).includes("Recovered.fcproj"));
      assert.equal(readFileSync(join(downloads, "Recovered.fcproj"), "utf8"), "legacy-recovery-test");
      await js("window.filmcraft.request('ui.click',{id:'recovery.continue'})");
      await until(async () => (await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})")).length === 0);
      const limitWarning = await js("window.filmcraft.request('ui.elements',{prefix:'browserRecovery.'})");
      assert.ok(limitWarning.some((element) => element.id === "browserRecovery.unavailable"
        && element.label.includes("Die Browser-Wiederherstellung enthält mehr als 100000 Medieneinträge.")
        && !element.label.includes("{limit}")));
      await js("window.filmcraft.execute('file.newSequence',{name:'Editing after directory limit'})");
      await sleep(6500);
      assert.equal(await js(opfsText("snapshot.fcproj")), "legacy-recovery-test");
      assert.equal(await js(opfsText("meta.json")), heldMeta);
    } finally {
      await send("Page.removeScriptToEvaluateOnNewDocument", { identifier: entryLimitSetup.identifier });
    }
    console.log("endless media enumeration is bounded, localized and preserves the downloadable snapshot: PASS");

    const unavailableStorage = await send("Page.addScriptToEvaluateOnNewDocument", {source: `
      delete StorageManager.prototype.getDirectory;
    `});
    try {
      const previousOrigin = await js("performance.timeOrigin");
      await send("Page.navigate", {url});
      await until(async () => await js(`performance.timeOrigin !== ${previousOrigin} && !!window.filmcraftLoad?.readyMs`));
      assert.equal(await js("window.filmcraft.info().opfs"), false);
      await until(async () => (await js("window.filmcraft.request('ui.elements',{prefix:'browserRecovery.'})"))
        .some((element) => element.id === "browserRecovery.unavailable" && element.label.includes("OPFS not available")),
        5000, "unsupported browser storage must show a persistent recovery warning");
      await js("window.filmcraft.execute('file.newSequence',{name:'Manual save without OPFS'})");
      rmSync(join(downloads, "without-opfs.fcproj"), {force:true});
      await js("window.filmcraft.execute('file.saveCopy',{path:'/exports/without-opfs.fcproj'})");
      await until(() => readdirSync(downloads).includes("without-opfs.fcproj"));
      const saved = JSON.parse(readFileSync(join(downloads, "without-opfs.fcproj"), "utf8"));
      assert.ok(Object.values(saved.project.items).some((item) => item.name === "Manual save without OPFS"));
      assert.ok((await js("window.filmcraft.request('ui.elements',{prefix:'browserRecovery.'})"))
        .some((element) => element.id === "browserRecovery.unavailable"));
      const hostWithoutStorage = await send("Page.addScriptToEvaluateOnNewDocument", {source: `
        new MutationObserver((_, observer) => {
          const canvas = document.querySelector("canvas");
          if (!canvas) return;
          canvas.setAttribute("data-host", "page");
          canvas.addEventListener("filmcraft:ready", () => { window.testHostReady = true; });
          observer.disconnect();
        }).observe(document, {childList:true,subtree:true});
      `});
      try {
        await send("Page.navigate", {url});
        await until(async () => await js("!!window.testHostReady && !!window.filmcraftLoad?.readyMs"));
        assert.equal(await js("window.filmcraft.info().opfs"), false);
        await js("window.filmcraft.execute('file.newSequence',{name:'Host without browser recovery'})");
        assert.ok((await js("window.filmcraft.request('ui.elements',{})")).length > 0);
        assert.deepEqual(await js("window.filmcraft.request('ui.elements',{prefix:'browserRecovery.'})"), []);
        assert.deepEqual(await js("window.filmcraft.request('ui.elements',{prefix:'recovery.'})"), []);
      } finally {
        await send("Page.removeScriptToEvaluateOnNewDocument", {identifier:hostWithoutStorage.identifier});
      }
    } finally {
      await send("Page.removeScriptToEvaluateOnNewDocument", {identifier:unavailableStorage.identifier});
    }
    console.log("unsupported storage reports unavailable recovery while editing and manual save remain usable: PASS");
  }
} catch (error) {
  console.error(error);
  try {
    console.error(await js("JSON.stringify({load:window.filmcraftLoad,text:document.body.innerText})"));
  } catch (diagnostic) {
    console.error(diagnostic);
  }
  process.exitCode = 1;
} finally {
  socket?.close();
  let forceExit;
  if (child.exitCode === null && child.signalCode === null && !launchError) {
    child.kill();
    forceExit = setTimeout(() => child.kill("SIGKILL"), 5000);
  }
  await exited;
  clearTimeout(forceExit);
  rmSync(profile, { recursive: true, force: true });
}

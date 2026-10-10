// Web import destinations: active bin tab, browser drop + navigation during pending reads.
// Synthetic browser drop event exercises the production handler; physical OS drag/picker UI is outside this test.
export async function checkImportBins({ evaluate, media, timeoutMs = 120000 }) {
  const report = {gates: {}, cases: []};
  await evaluate('filmcraft.execute("file.newProject", {name: "Import bins"})');
  const parent = (await evaluate('filmcraft.execute("file.newBin", {name: "Footage"})')).bin;
  const child = (await evaluate(`filmcraft.execute("file.newBin", {name: "Selects", parent: ${parent}})`)).bin;
  await evaluate(`(async () => {window.__importBinFixture = new Uint8Array(await (await fetch(${JSON.stringify(media)})).arrayBuffer());})()`);
  const setBin = bin => evaluate(`filmcraft.request("ui.set", {projectPanel: {bin: ${JSON.stringify(bin)}, activeTab: null}})`);
  const importExpression = name => `filmcraft.importFiles([new File([window.__importBinFixture], ${JSON.stringify(name)}, {type: "video/mp4"})])`;
  const locate = async name => {
    const deadline = Date.now() + timeoutMs;
    const pollMs = 100;
    for (;;) {
      const project = await evaluate('filmcraft.execute("project.inspect")');
      const find = bin => {
        for (const entry of bin.children) {
          if (entry.item !== undefined && entry.name === name) return bin.id;
          if (entry.children) {const found = find(entry); if (found !== undefined) return found;}
        }
      };
      const found = find(project.root);
      if (found !== undefined) return found;
      if (Date.now() >= deadline) throw new Error(`Import never appeared: ${name}`);
      await new Promise(resolve => setTimeout(resolve, pollMs));
    }
  };
  const check = async (key, expected, name) => {
    const actual = await locate(name);
    report.cases.push({key, expected, actual, name}); report.gates[key] = actual === expected;
  };
  await evaluate(`filmcraft.request("ui.set", {projectPanel: {bin: ${child}, activeTab: 0, tabs: [{bin: ${parent}}]}})`);
  await evaluate(importExpression("tab.mp4")); await check("activeTab", parent, "tab.mp4");
  await setBin(child);
  await evaluate(`(() => {
    const transfer = new DataTransfer();
    transfer.items.add(new File([window.__importBinFixture], "drop.mp4", {type: "video/mp4"}));
    window.dispatchEvent(new DragEvent("drop", {dataTransfer: transfer, bubbles: true, cancelable: true}));
  })()`);
  await check("browserDrop", child, "drop.mp4");
  await setBin(child);
  await evaluate(`(() => {
    const original = Blob.prototype.arrayBuffer;
    let release;
    const pending = new Promise(resolve => {release = resolve;});
    const gate = window.__importBinGate = {hits: 0, release, original};
    Blob.prototype.arrayBuffer = function () {gate.hits++; return pending.then(() => original.call(this));};
    window.__pendingBinImport = ${importExpression("pending.mp4")};
  })()`);
  try {
    const deadline = Date.now() + timeoutMs;
    const pollMs = 100;
    for (;;) {
      if (await evaluate("window.__importBinGate.hits > 0")) break;
      if (Date.now() >= deadline) throw new Error("Read-delay probe did not intercept any Blob reads");
      await new Promise(resolve => setTimeout(resolve, pollMs));
    }
    report.delayedReadHits = await evaluate("window.__importBinGate.hits");
    await setBin(parent);
    await evaluate("window.__importBinGate.release(); window.__pendingBinImport");
    await check("destinationCapturedBeforeRead", child, "pending.mp4");
  } finally {
    await evaluate("Blob.prototype.arrayBuffer = window.__importBinGate.original; window.__importBinGate.release(); delete window.__importBinGate; delete window.__pendingBinImport; delete window.__importBinFixture");
  }
  report.ok = Object.values(report.gates).every(Boolean);
  return report;
}

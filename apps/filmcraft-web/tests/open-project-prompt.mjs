// Web Open Project prompt: a project dropped on the page with unsaved changes asks first.
// Synthetic browser drop events and a stubbed File System Access picker exercise the production handlers; physical OS drag/picker UI is outside this test.

function silence() {
  // RIFF/WAVE PCM16 mono, 0.1 s of silence at 48 kHz: 44-byte header + 2 bytes/sample.
  const samples = 4800;
  const b = Buffer.alloc(44 + samples * 2);
  b.write("RIFF", 0); b.writeUInt32LE(b.length - 8, 4); b.write("WAVEfmt ", 8);
  b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22);
  b.writeUInt32LE(48000, 24); b.writeUInt32LE(96000, 28);
  b.writeUInt16LE(2, 32); b.writeUInt16LE(16, 34); b.write("data", 36); b.writeUInt32LE(samples * 2, 40);
  return b;
}

export async function checkOpenProjectPrompt({ evaluate, timeoutMs = 60000 }) {
  const report = {gates: {}};
  const pollMs = 100;
  const until = async (what, read) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (await read()) return;
      if (Date.now() >= deadline) throw new Error(`Timed out waiting for ${what}`);
      await new Promise(resolve => setTimeout(resolve, pollMs));
    }
  };
  const execute = (id, params = {}) => evaluate(`filmcraft.execute(${JSON.stringify(id)}, ${JSON.stringify(params)})`);
  const inspect = () => execute("project.inspect");
  const prompted = async () => (await evaluate('filmcraft.request("ui.elements", {prefix: "openProject."})')).some(e => e.id === "openProject.cancel");
  const hasFile = async path => (await evaluate("filmcraft.files()")).some(f => f.path === path);
  const hasChild = async name => (await inspect()).root.children.some(c => c.name === name);
  const openedAt = path => until(`the project at ${path}`, async () => (await inspect()).path === path);
  const answer = async id => {
    await until("the Save Project prompt", prompted);
    await evaluate(`filmcraft.request("ui.click", {id: ${JSON.stringify(id)}})`);
  };
  const drop = files => evaluate(`(() => {
    const transfer = new DataTransfer();
    for (const [name, source] of ${JSON.stringify(files)}) transfer.items.add(new File([window.__promptFixtures[source]], name));
    window.dispatchEvent(new DragEvent("drop", {dataTransfer: transfer, bubbles: true, cancelable: true}));
  })()`);
  const change = async () => {
    await execute("file.newBin", {name: "Unsaved"});
    report.gates.changeMakesItDirty = (report.gates.changeMakesItDirty ?? true) && (await inspect()).dirty;
  };

  const current = "/projects/Current.fcproj";
  // `ui.elements` waits for the timeline to settle, which takes a sequence to draw.
  await execute("file.newProject", {name: "Other"});
  await execute("file.newSequence", {name: "Other sequence"});
  await execute("file.save", {path: "/prompt-test/Other.fcproj"});
  await evaluate(`(async () => {window.__promptFixtures = {
    other: await filmcraft.readFile("/prompt-test/Other.fcproj"),
    wav: Uint8Array.from(atob(${JSON.stringify(silence().toString("base64"))}), c => c.charCodeAt(0)),
    corrupt: new TextEncoder().encode("not a project"),
  };})()`);
  await execute("file.newProject", {name: "Current"});
  await execute("file.newSequence", {name: "Current sequence"});
  await execute("file.save", {path: current});

  // Nothing unsaved: the dropped project opens without a prompt.
  await drop([["Other.fcproj", "other"]]);
  await openedAt("/projects/Other.fcproj");
  report.gates.opensWithoutPromptWhenSaved = !(await prompted());

  // A project that fails to open leaves the file table as it was, and the next drop still runs.
  await execute("file.open", {path: current});
  await drop([["Corrupt.fcproj", "corrupt"]]);
  await drop([["Current.fcproj", "corrupt"]]);
  await drop([["Other.fcproj", "other"]]);
  await openedAt("/projects/Other.fcproj");
  report.gates.failedOpenRemovesCopy = !(await hasFile("/projects/Corrupt.fcproj"));
  await execute("file.open", {path: current});
  report.gates.failedOpenKeepsReplacedFile = (await inspect()).name === "Current";

  // The same with unsaved changes: Don't Save keeps the open project and its changes.
  await execute("file.newBin", {name: "Failed open"});
  await drop([["Corrupt.fcproj", "corrupt"]]);
  await answer("openProject.dontSave");
  await until("the failed copy's removal", async () => !(await hasFile("/projects/Corrupt.fcproj")));
  const failed = await inspect();
  report.gates.failedOpenKeepsProject = failed.path === current && failed.dirty && (await hasChild("Failed open"));
  await execute("file.save", {path: current});

  // Cancel keeps the open project and its changes, and the dropped file's copy is removed.
  await execute("file.open", {path: current});
  await change();
  await drop([["Dropped.fcproj", "other"]]);
  await until("the Save Project prompt", prompted);
  const shown = await inspect();
  report.gates.promptLeavesProjectOpen = shown.path === current && shown.dirty;
  await answer("openProject.cancel");
  await until("the canceled copy's removal", async () => !(await hasFile("/projects/Dropped.fcproj")));
  const canceled = await inspect();
  report.gates.cancelKeepsProject = canceled.path === current && canceled.dirty && !(await prompted());

  // Don't Save opens the dropped project and leaves the file as it was.
  await drop([["Dropped.fcproj", "other"]]);
  await answer("openProject.dontSave");
  await openedAt("/projects/Dropped.fcproj");
  report.gates.dontSaveOpens = (await inspect()).name === "Other";
  await execute("file.open", {path: current});
  report.gates.dontSaveDoesNotWrite = !(await hasChild("Unsaved"));

  // Save writes the open project first. A dropped file named like it does not replace the save.
  await change();
  await drop([["Current.fcproj", "other"]]);
  await answer("openProject.save");
  await openedAt("/projects/opened/Current.fcproj");
  report.gates.saveOpensDroppedFile = (await inspect()).name === "Other";
  await execute("file.open", {path: current});
  report.gates.saveWritesOpenProject = await hasChild("Unsaved");

  // Save on a project never saved writes it under its name, then opens the dropped file.
  await execute("file.newProject", {name: "Fresh"});
  await execute("file.newSequence", {name: "Fresh sequence"});
  await change();
  await drop([["Never saved.fcproj", "other"]]);
  await answer("openProject.save");
  await openedAt("/projects/Never saved.fcproj");
  await execute("file.open", {path: "/projects/Fresh.fcproj"});
  report.gates.saveWritesNeverSavedProject = await hasChild("Unsaved");
  await execute("file.open", {path: current});

  // File ▸ Open asks too. The picker's answer is a real file handle, since headless Chromium shows no dialog.
  await evaluate(`(async () => {
    const dir = await navigator.storage.getDirectory();
    const handle = await dir.getFileHandle("picked-project.fcproj", {create: true});
    const writable = await handle.createWritable();
    await writable.write(window.__promptFixtures.other);
    await writable.close();
    window.__promptFixtures.picker = window.showOpenFilePicker;
    window.showOpenFilePicker = async () => [handle];
  })()`);
  await execute("file.newBin", {name: "Picked change"});
  await evaluate('filmcraft.request("ui.menu.invoke", {id: "file.open"})');
  await answer("openProject.dontSave");
  await openedAt("/projects/picked-project.fcproj");
  report.gates.filePickerAsks = (await inspect()).name === "Other";
  await evaluate(`(async () => {
    window.showOpenFilePicker = window.__promptFixtures.picker;
    await (await navigator.storage.getDirectory()).removeEntry("picked-project.fcproj");
  })()`);
  await execute("file.open", {path: current});

  // A second drop waits for the first prompt's answer.
  await execute("file.newBin", {name: "Second change"});
  await drop([["First.fcproj", "other"]]);
  await until("the Save Project prompt", prompted);
  await drop([["Second.fcproj", "other"]]);
  await answer("openProject.cancel");
  await until("the first drop's removal", async () => !(await hasFile("/projects/First.fcproj")));
  await answer("openProject.dontSave");
  await openedAt("/projects/Second.fcproj");
  report.gates.secondDropWaits = (await inspect()).name === "Other";

  // Media dropped with a project waits for the answer, and goes into the project that is open then.
  await execute("file.open", {path: current});
  await execute("file.newBin", {name: "Third change"});
  await drop([["Third.fcproj", "other"], ["with-project.wav", "wav"]]);
  await until("the Save Project prompt", prompted);
  report.gates.mediaWaitsForAnswer = !(await hasChild("with-project.wav"));
  await answer("openProject.cancel");
  await until("the media import", () => hasChild("with-project.wav"));
  report.gates.mediaGoesIntoOpenProject = (await inspect()).path === current;

  // Media dropped on its own while the prompt is shown waits too, and goes into the project opened after the answer.
  await drop([["Fourth.fcproj", "other"]]);
  await until("the Save Project prompt", prompted);
  await drop([["after-prompt.wav", "wav"]]);
  await answer("openProject.dontSave");
  await openedAt("/projects/Fourth.fcproj");
  await until("the later drop's import", () => hasChild("after-prompt.wav"));
  report.gates.laterMediaWaitsForAnswer = (await inspect()).path === "/projects/Fourth.fcproj";

  await evaluate("delete window.__promptFixtures");
  report.ok = Object.values(report.gates).every(Boolean);
  return report;
}

// Original example code. MIT OR Apache-2.0.
"use strict";
const status = document.getElementById("status");
const addButton = document.getElementById("add");
const refreshButton = document.getElementById("refresh");
const nameInput = document.getElementById("name");
const commentInput = document.getElementById("comment");
let snapshot = null;
let refreshing = false;
let refreshAgain = false;
let editing = false;
let refreshTimer;

function render() {
  const sequence = snapshot && snapshot.sequence;
  document.getElementById("sequence-name").textContent = sequence ? sequence.name : "Open a sequence in FilmCraft";
  document.getElementById("playhead").textContent = sequence
    ? `Frame ${sequence.playheadFrame} · ${sequence.frameRate.num}/${sequence.frameRate.den} fps`
    : "No active sequence";
  addButton.disabled = !sequence || editing;
  const markers = sequence ? sequence.markers : [];
  document.getElementById("count").textContent = sequence && sequence.markersTruncated
    ? `${markers.length} of ${sequence.totalMarkers}`
    : String(markers.length);
  document.getElementById("empty").hidden = markers.length > 0;
  const list = document.getElementById("markers");
  list.replaceChildren();
  for (const marker of markers) {
    const item = document.createElement("li");
    const heading = document.createElement("div");
    heading.className = "marker-title";
    const title = document.createElement("strong");
    title.textContent = marker.name || "Untitled marker";
    const seekButton = document.createElement("button");
    seekButton.type = "button";
    seekButton.textContent = `Frame ${marker.frame}`;
    seekButton.setAttribute("aria-label", `Go to ${marker.name || "marker"} at frame ${marker.frame}`);
    // Capture this document and sequence, so a late click cannot target a newly opened project.
    const documentId = snapshot.documentId;
    const sequenceId = sequence.id;
    seekButton.addEventListener("click", async () => {
      seekButton.disabled = true;
      try {
        await filmcraft.invoke("playhead.seek", { documentId, sequenceId, frame: marker.frame });
        await refresh();
        status.textContent = `Moved to frame ${marker.frame}.`;
      } catch (error) {
        status.textContent = error.message;
        await refresh(false);
      } finally {
        seekButton.disabled = false;
      }
    });
    heading.append(title, seekButton);
    item.append(heading);
    if (marker.comment) {
      const comment = document.createElement("p");
      comment.className = "marker-comment";
      comment.textContent = marker.comment;
      item.append(comment);
    }
    list.append(item);
  }
}

async function refresh(showStatus = true) {
  if (refreshing) {
    refreshAgain = true;
    return;
  }
  refreshing = true;
  refreshButton.disabled = true;
  try {
    snapshot = await filmcraft.invoke("document.snapshot");
    render();
    if (showStatus) status.textContent = snapshot && snapshot.sequence ? "Sequence is up to date." : "Open a sequence to add notes.";
  } catch (error) {
    snapshot = null;
    render();
    status.textContent = error.message;
  } finally {
    refreshing = false;
    refreshButton.disabled = false;
    if (refreshAgain) {
      refreshAgain = false;
      scheduleRefresh();
    }
  }
}

function scheduleRefresh() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => refresh(false), 100);
}

document.getElementById("note-form").addEventListener("submit", async event => {
  event.preventDefault();
  if (editing || !snapshot || !snapshot.sequence) return;
  const context = snapshot;
  const name = nameInput.value.trim();
  if (!name) return;
  editing = true;
  addButton.disabled = true;
  status.textContent = "Adding marker…";
  try {
    await filmcraft.invoke("markers.add", {
      documentId: context.documentId,
      sequenceId: context.sequence.id,
      revision: context.revision,
      frame: context.sequence.playheadFrame,
      name,
      comment: commentInput.value,
    });
    nameInput.value = "";
    commentInput.value = "";
    await refresh(false);
    status.textContent = "Marker added. You can undo it in FilmCraft.";
  } catch (error) {
    // Keep the draft and refresh. Never automatically retry an edit after an ambiguous timeout.
    status.textContent = error.message;
    await refresh(false);
  } finally {
    editing = false;
    render();
  }
});
refreshButton.addEventListener("click", () => refresh());
filmcraft.on("document.changed", scheduleRefresh);
refresh();

// Original example code. MIT OR Apache-2.0.
"use strict";
const status = document.getElementById("status");
const refreshButton = document.getElementById("refresh");
function showSize() {
  document.getElementById("window-size").textContent = `${window.innerWidth} × ${window.innerHeight}`;
}
async function refresh() {
  refreshButton.disabled = true;
  status.textContent = "Reading host information…";
  try {
    const info = await filmcraft.invoke("host.info");
    document.getElementById("app-version").textContent = info.appVersion;
    document.getElementById("api-version").textContent = `v${info.apiVersion}`;
    filmcraft.applyTheme(info.theme);
    const list = document.getElementById("capabilities");
    list.replaceChildren();
    for (const capability of info.capabilities || []) {
      const item = document.createElement("li");
      item.textContent = capability;
      list.append(item);
    }
    status.textContent = "Connected to FilmCraft.";
  } catch (error) {
    status.textContent = error.message;
  } finally {
    refreshButton.disabled = false;
  }
}
refreshButton.addEventListener("click", refresh);
window.addEventListener("resize", showSize);
showSize();
refresh();

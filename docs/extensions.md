# Experimental web extensions

Web extensions are optional desktop integrations with their own HTML, CSS and JavaScript interfaces.
FilmCraft installs the package and opens its interface in a FilmCraft-managed window. An extension
can choose its layout and workflow; FilmCraft supplies a small, permission-checked editor API.

This is an experimental API, not a stable plugin ABI. The initial feature targets macOS and Windows
and is disabled in default builds. It does not load Adobe UXP packages or native effect plugins.
Runtime and distribution decisions are proposed in [ADR 0002](adr/0002-web-extensions.md).
macOS uses its system WebKit runtime. Windows requires an available WebView2 runtime; this feature
does not bundle a WebView2 installer.

## Use an extension

In a build with `web-extensions` enabled:

1. Open **Window ▸ Extensions**.
2. Choose **Install from File…** and select a `.fcplugin` file. Read its requested permissions and
   network destinations, then choose **Install Extension**.
3. Select the installed extension and choose **Open**.
4. Clear **Enabled** to close the extension and prevent it opening, or choose **Remove** to uninstall it.

The extension window belongs to FilmCraft. Users do not install or launch another application, run a
terminal command, or start a local server. Closing its window leaves the editor open. The first
version uses separate managed windows; docking into the editor workspace is future work.

The manager allows 32 installed extensions and four open windows. Packages are checked against the
SHA-256 digest recorded at installation; a changed package must be removed and reinstalled so its
permissions can be reviewed again. This detects a changed package, not the identity of its author.
Installing an update currently also requires removing the previous version first.
Invalid or changed installed packages remain visible as **Unavailable extension**, with the error
and a removal action. Removing a package leaves its runtime data directory under `Extensions/Runtime`
in place; it is not a general browser-data cleanup operation.

Packages can be distributed directly as files. There is no marketplace, package signing service,
automatic updater, or public registry in this version. An installed package is executable third-party
JavaScript: install only packages you trust. Permissions describe the editor bridge and allowed web
origins, not an operating-system sandbox or a certification of the publisher.

## Package format

A `.fcplugin` file is a ZIP archive with `manifest.json` at the root, alongside the extension's files:

```text
manifest.json
index.html
style.css
app.js
```

Example manifest:

```json
{
  "id": "org.example.marker-notes",
  "name": "Marker Notes",
  "version": "0.1.0",
  "apiVersion": 1,
  "entry": "index.html",
  "permissions": ["project.read", "playhead.write", "markers.write"],
  "networkOrigins": []
}
```

`id` is the package's stable identifier. `name` is shown in the manager and window title. `version`
identifies the package release. `apiVersion` must be `1`. `entry` is a relative HTML path inside the
package. `permissions` and `networkOrigins` default to empty arrays; unknown manifest fields and
unknown permissions are rejected.

Use local, relative URLs for styles and scripts. For example:

```html
<link rel="stylesheet" href="style.css">
<script src="app.js" defer></script>
```

Do not include the SDK script in the package: FilmCraft injects it before extension scripts execute.
Inline JavaScript, inline event handlers, `eval`, remote scripts, frames, workers and embedded objects
are not part of the supported execution model. Package files are served from the installed package;
the manifest does not grant access to arbitrary local files.

For extension authors, the examples can be packaged with a ZIP utility. Archive the **contents** of
each example directory, with no extra enclosing directory. For example, from the repository root:

```sh
cd examples/extensions/host-info
zip -X /tmp/host-info.fcplugin manifest.json index.html style.css app.js
```

Build a development editor with:

```sh
cargo run --release -p filmcraft --features web-extensions
```

These author commands are not part of the installation flow for an end user.

## JavaScript API

FilmCraft injects `window.filmcraft`:

```js
const info = await filmcraft.invoke("host.info");
const unsubscribe = filmcraft.on("document.changed", () => refreshDocument());
// When the subscriber is no longer needed:
unsubscribe();
```

`invoke(method, params = {})` returns a Promise. Failures reject with an `Error` whose `code` and
`message` describe the failure. The SDK allows at most 16 outstanding requests, keeps each JSON
request below 256 KiB of UTF-8 and times out after 15 seconds. The host bounds incoming and outgoing
frames at 256 KiB including the newline delimiter, queues at 32 frames per direction, and dispatches
at most eight calls per extension per editor frame. A timeout does not prove that a mutation
did not happen: refresh the document before deciding whether to retry. Do not automatically retry
editing requests.

`on(event, callback)` returns an unsubscribe function. A throwing callback does not prevent other
subscribers from receiving an event. Closing the page rejects outstanding requests and clears
subscribers.

| Method | Permission | Parameters | Result |
|---|---|---|---|
| `host.info` | None | `{}` | `{apiVersion, appVersion, capabilities, permissions, theme}` |
| `document.snapshot` | `project.read` | `{}` | Document context and active sequence, described below |
| `playhead.seek` | `playhead.write` | `{documentId, sequenceId, frame}` | Completion; refresh the snapshot for the current position |
| `markers.add` | `markers.write` | `{documentId, sequenceId, revision, frame, name, comment?}` | `{markerId, revision}`; the marker is an undoable editor change |

`capabilities` lists the methods available under the package's permissions. The host still checks
permissions on every call. This bridge deliberately does not expose arbitrary `engine.execute`, command-line
execution, native libraries, the generic automation server, or general filesystem access.

A document snapshot has this shape:

```js
{
  documentId,
  revision,
  name,
  sequence: {
    id,
    name,
    durationFrames,
    playheadFrame,
    frameRate: { num, den },
    markers: [{ id, frame, name, comment, color }],
    totalMarkers,
    markersTruncated
  } // or null when there is no active sequence
}
```

Document IDs, sequence IDs, marker IDs and revisions are decimal strings; treat them as opaque
values and pass them back unchanged. Frame positions and durations are safe JavaScript integers,
and the rate is rational (`num / den`). Do not convert frame values to
floating-point seconds for an edit. Snapshot data excludes media filesystem paths.

The first version returns at most 128 markers within a 192 KiB JSON budget; heavily escaped text
can mean fewer markers fit. Marker names are limited to 128 characters and comments to 256
characters in the snapshot. `totalMarkers` reports the full count and `markersTruncated` reports
whether more exist. Pagination is future work; do not present a bounded snapshot as a complete
project inventory. New marker names accept up to 512 UTF-8 bytes and comments up to 2048 UTF-8 bytes;
longer values will therefore appear truncated in a snapshot. The example uses smaller input limits
so its own notes fit the snapshot display.

Editing calls identify their target document and sequence so a delayed request cannot silently act
on a newly opened project. `markers.add` also includes the revision observed by the caller. If the
context is stale, refresh and let the user make the editing decision again. The Marker Notes example
keeps the unfinished note on an error instead of retrying it.

## Theme and layout

The SDK applies `host.info.theme` and `theme.changed` events to these CSS custom properties:

| Property | Purpose |
|---|---|
| `--filmcraft-bg` | Window background |
| `--filmcraft-text` | Primary text |
| `--filmcraft-muted` | Secondary text |
| `--filmcraft-accent` | Accent and focus indicators |
| `--filmcraft-border` | Dividers and control borders |

The theme object uses the keys `bg`, `text`, `muted`, `accent`, `border`, each containing a CSS color.
Authors can also call `filmcraft.applyTheme(theme)`. Write a responsive layout and give CSS variables
fallback values, for example `color: var(--filmcraft-text, #eeeeee)`. FilmCraft owns the window's
lifecycle and theme tokens; the extension owns its contents, typography and layout. An editor
workspace rearrangement does not require a plugin to search for controls by screen coordinates.
An incompatible API change still requires migration; this is not a guarantee of automatic future
compatibility.

`document.changed` signals that the context may have changed. Extensions with `project.read` receive
a snapshot; others receive only `{documentId, revision}`, without project or sequence names.
Read `document.snapshot` to obtain the current data when permitted. Coalesce
frequent notifications instead of starting one request per event. There is no guarantee of one
notification for every intermediate playback frame.

## HTTPS services

An extension may use standard `fetch` to contact declared HTTPS origins when it requests `network`:

```json
{
  "permissions": ["network"],
  "networkOrigins": ["https://api.example.com"]
}
```

This is an excerpt to add to a complete manifest, not a complete manifest itself. Network origins
are explicit origins, not wildcards. The window's Content Security Policy limits connections to the
declared origins and the extension's own packaged resources. Local `fetch("data.json")`
works without a network permission. Browser CORS rules still apply: the service must accept the extension's origin
(`filmcraft-extension://localhost` on macOS and `http://filmcraft-extension.localhost` on Windows).
Redirects and other requested origins must also satisfy the policy. Do not embed service secrets in
a package. There is no credential-vault API in this first version.

The current editor API does **not** export a sequence, supply media bytes, upload media, or import a
service's output. Network access alone does not implement an end-to-end media service workflow.
Export/import jobs, authentication and scoped file handles require a separate API design and tests.

## Examples and validation

- [Host Info](../examples/extensions/host-info/) uses a responsive card layout, displays host
  capabilities and follows the editor theme. It asks for no project access.
- [Marker Notes](../examples/extensions/marker-notes/) uses a note composer and marker list. It reads
  the active sequence, seeks to a selected marker and adds an undoable marker at the observed
  playhead position.

Both examples are original code under MIT OR Apache-2.0, use system fonts and contain no image or
media assets. Their different interfaces demonstrate that extensions are not constrained to a
shared form or one service-specific panel.

The SDK regression suite uses Node's built-in test runner, with no npm dependencies:

```sh
node --test tests/extensions/sdk.test.cjs
```

Verify both default and `web-extensions` builds. In addition to the repository's `cargo xtask ci`
gates, exercise package rejection, permission denial, stale document/revision rejection, marker
undo/redo, window close/reopen and a helper failure. Run the actual enabled editor, install each
example through the manager, inspect the rendered windows and resize them. The headless egui
harness alone does not validate a native webview window. Record the platforms actually tested;
an experimental macOS/Windows target is not evidence that both were tested.

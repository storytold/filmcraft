# Connect an AI to FilmCraft on GoogleBook

FilmCraft runs a Streamable HTTP MCP server inside the APK. Open a project and sequence, then select
**MCP Config** in the toolbar. Keep FilmCraft open while your client is using its timeline.

1. Choose **Start on this device** if the client can reach FilmCraft's Android loopback. For a
   client on another device, choose **Connect over a trusted network**, confirm FilmCraft's private
   IPv4 address, and select **Start network server**. Both devices need to be able to reach each
   other on the same trusted private network. This mode uses HTTP.
2. Select your client. In Antigravity, open **MCP Servers → Manage MCP Servers → View raw config**.
   Choose **Full JSON** for an empty config or **FilmCraft entry only** to add or replace just
   the `filmcraft` entry inside an existing `mcpServers` object. Use commas between entries.
   Use the same JSON options with Claude Code's `.mcp.json`. For Codex, add the TOML block to
   `~/.codex/config.toml`.
3. Use the copy button in FilmCraft, then paste, save, and reload your client's MCP servers.
   The clipboard contains the live bearer token. The on-screen preview hides it until you
   choose **Reveal private token**. Keep this config private.

Other local clients that support Streamable HTTP can use the same `/mcp` URL and `Authorization`
header. By default, the config expires when you stop the server, switch projects, or close FilmCraft.

Choose **Create saved local pairing** to keep the local URL and token across restarts. Paste that
config once, then use **Start saved local pairing** when you reopen FilmCraft. On a project switch,
select **Allow access to this project** before your client can inspect or propose edits.
**Revoke saved pairing** invalidates the copied config. Saved pairing uses `127.0.0.1:38761`.

**Check connection** tests the server and token locally. After pasting your configuration, ask the
client to call `filmcraft_inspect`. **MCP client initialized** confirms the client completed the
handshake, and **Current project approved** indicates whether it can inspect and propose edits.
FilmCraft shows when a request used a missing or invalid token.

## Try an edit

Ask your AI “Inspect my FilmCraft timeline and propose trimming the last 12 frames of the first
video clip. Wait for my review.” The client should discover `filmcraft_inspect`,
`filmcraft_propose`, and `filmcraft_review_status`. It can propose splits, trims, clip moves, titles,
captions, supported effects and transitions, audio gain adjustments, and deletes or ripple deletes
of an explicit clip. A ripple delete can shift later clips, captions, and markers. Review the named
clips and effects on the timeline before applying. Times are timeline seconds.
The AI uses the revision object from inspection when submitting a proposal.

In FilmCraft, select **Review AI edit**. Each step shows its estimated range after the earlier steps.
**Jump to step** and **Compare selected step** use the same moment. The review panel can play a
short comparison with the before or after audio.
**Compare all at playhead** previews the whole batch at the current frame. Select **Apply edit**
or **Dismiss**.
Undo reverts the applied batch in one step.

## If the client cannot reach FilmCraft

Some Android environments isolate app networking from the rest of the device. If Android's loopback
is isolated, start the local server in FilmCraft. With the GoogleBook visible in `adb devices`, run
the following on the computer running the AI client. Substitute the port shown in **MCP Config**.

```sh
adb forward tcp:12345 tcp:12345
```

Use the same copied config after forwarding. This forwards the embedded server and requires no
companion process. If the network option fails on a guest Wi-Fi network, check whether that network
allows devices to reach one another.

ChatGPT and Claude on the web require a reachable, authenticated HTTPS connector. Use a supported
tunnel or HTTPS gateway with suitable account authentication, keep FilmCraft's bearer token private,
and forward requests to the embedded `/mcp` endpoint. A gateway must use the embedded server's address
as its upstream `Host` header. This APK provides the local endpoint. The hosted connector and its
account settings are configured separately.

- [Antigravity MCP settings](https://antigravity.google/docs/mcp)
- [Codex MCP settings](https://developers.openai.com/codex/mcp/)
- [Claude Code MCP settings](https://code.claude.com/docs/en/mcp)
- [ChatGPT custom MCP connectors](https://help.openai.com/en/articles/12584461-connecting-custom-mcp-servers-in-chatgpt)
- [Claude web custom connectors](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp)

The Python companion files in this directory are for an earlier APK with the legacy control bridge.

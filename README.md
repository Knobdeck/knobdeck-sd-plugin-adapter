# KnobDeck SD Plugin Adapter

Runs plugins made for Stream Deck inside [KnobDeck](https://github.com/Knobdeck). Install it from KnobDeck's
**Adapters** screen, then add plugins from the adapter's own page (**Adapters → Open**).

- You bring the plugins (`.streamDeckPlugin` files). Nothing from any plugin author or from Elgato is included.
- Built only from the open Stream Deck SDK and its public docs (MIT, `elgatosf/streamdeck`). Not affiliated with
  or endorsed by Elgato / Corsair.
- Plugins run as ordinary programs on your computer: only add ones you trust.
- Many plugins won't work (native Windows/macOS code, touch-strip layouts, deep links). Plugins written in Node
  that only call web APIs or local servers are the best bet.

## How it works

```
KnobDeck --stdio--> bridge-shim (one copy per imported plugin) --loopback--> daemon --WebSocket--> Stream Deck plugin (Node)
```

Importing a plugin generates a small KnobDeck plugin (a manifest, an actions list and the shim) in KnobDeck's plugins
folder. The daemon runs the real plugin and translates events: key presses become `keyDown`/`keyUp`, dial turns
`dialRotate`, and `setTitle` / `setState` / `setFeedback` come back as tile updates. Settings live in the adapter
(`knobdeck-sd-plugin-adapter where`), never in KnobDeck. Actions with a settings page get a **Configure** button that
opens the plugin's Property Inspector in your browser. Design notes: the KnobDeck plans folder (`STREAMDECK_ADAPTER_PLAN.md`).

## Commands

KnobDeck calls these from the Adapters screen (see `adapter.json`); you can too:

| Command | |
|---|---|
| `daemon` | run the daemon in this terminal |
| `stop` / `status` | stop the daemon / exit 0 when it runs |
| `open` | open the adapter's page (starts the daemon if needed) |
| `import <file\|folder>` | import a `.streamDeckPlugin` or a `.sdPlugin` folder |
| `list`, `remove <uuid> [--settings]` | |
| `uninstall [--purge]` | stop and remove every imported plugin (`--purge`: also settings and data) |

After importing or removing, restart KnobDeck so its plugin list refreshes. Node must be on PATH (or set
`KNOBDECK_BRIDGE_NODE`). `KNOBDECK_CONFIG_DIR` points at KnobDeck's config folder when it isn't the default.

## Build

```sh
cargo build --release     # target/release/knobdeck-sd-plugin-adapter and bridge-shim
cargo test
```

Every push to `master` builds all platforms; a new `version` in `bridge/Cargo.toml` publishes release `v<version>`
(zips per platform with `adapter.json`), and the KnobDeck registry lists it for the app.

`examples/hello-sd-plugin.sdPlugin` is a dependency-free test plugin (counter key, on/off key, dial with feedback).

MIT licence.

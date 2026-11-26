// A minimal Stream Deck plugin with no dependencies (Node 22+ has a global WebSocket).
const a = process.argv.slice(2);
const arg = (n) => a[a.indexOf(n) + 1];
const ws = new WebSocket(`ws://127.0.0.1:${arg("-port")}`);
const send = (o) => ws.send(JSON.stringify(o));
const instances = new Map(); // context -> {action, settings}
const log = (m) => send({ event: "logMessage", payload: { message: String(m) } });

ws.onopen = () => {
  send({ event: arg("-registerEvent"), uuid: arg("-pluginUUID") });
  send({ event: "getGlobalSettings", context: arg("-pluginUUID") });
};
ws.onmessage = (m) => {
  const e = JSON.parse(m.data);
  const ctx = e.context;
  switch (e.event) {
    case "willAppear":
      instances.set(ctx, { action: e.action, settings: e.payload.settings });
      if (e.action.endsWith(".count")) send({ event: "setTitle", context: ctx, payload: { title: String(e.payload.settings.count ?? 0) } });
      if (e.action.endsWith(".switch")) send({ event: "setTitle", context: ctx, payload: { title: e.payload.settings.name ?? "" } });
      break;
    case "willDisappear":
      instances.delete(ctx);
      break;
    case "didReceiveSettings": {
      const i = instances.get(ctx);
      if (i) i.settings = e.payload.settings;
      if (e.action.endsWith(".switch")) send({ event: "setTitle", context: ctx, payload: { title: e.payload.settings.name ?? "" } });
      break;
    }
    case "keyDown":
      if (e.action.endsWith(".count")) {
        const s = { ...e.payload.settings, count: (e.payload.settings.count ?? 0) + 1 };
        send({ event: "setSettings", context: ctx, payload: s });
        send({ event: "setTitle", context: ctx, payload: { title: String(s.count) } });
      }
      log(`keyDown ${e.action} state=${e.payload.state}`);
      break;
    case "keyUp":
      log(`keyUp ${e.action}`);
      break;
    case "dialRotate": {
      // Relative ticks in; the plugin owns the number and reports it back as feedback.
      const i = instances.get(ctx);
      i.level = Math.max(0, Math.min(100, (i.level ?? 50) + e.payload.ticks));
      send({ event: "setFeedback", context: ctx, payload: { title: `Level ${i.level}`, indicator: i.level } });
      break;
    }
    case "sendToPlugin":
      log(`from PI: ${JSON.stringify(e.payload)}`);
      break;
  }
};

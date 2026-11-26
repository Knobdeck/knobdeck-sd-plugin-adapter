//! The daemon: one local socket for the shims, one loopback WebSocket/HTTP port for the
//! Stream Deck plugins and their Property Inspector pages.

use bridge_core::manifest::{ActionMap, BridgeMeta};
use bridge_core::settings::{key_for, Store};
use bridge_core::{manifest, paths, random_hex};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tungstenite::{Message, WebSocket};

const DEVICE: &str = "knobdeck-bridge-device";
const COLUMNS: usize = 5;
const ROWS: usize = 3;
const DIALS: usize = 4;

pub fn log(msg: impl AsRef<str>) {
    let line = format!("{}", msg.as_ref());
    eprintln!("{line}");
    let _ = std::fs::create_dir_all(paths::logs_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(paths::logs_dir().join("daemon.log")) {
        let _ = writeln!(f, "{line}");
    }
}

struct Inst {
    action_id: String,
    inputs: Value,
    map: ActionMap,
    state: usize,
    pos: (usize, usize),
    last_value: Option<f64>,
}

struct PiConn {
    inspector: String,
    key: String,
    tx: Sender<String>,
}

#[derive(Default)]
struct St {
    instances: HashMap<String, Inst>,
    plugin_tx: Option<Sender<String>>,
    pis: Vec<PiConn>,
}

pub struct Rt {
    meta: BridgeMeta,
    store: Store,
    host: Mutex<Option<TcpStream>>,
    st: Mutex<St>,
    child: Mutex<Option<Child>>,
    stopping: AtomicBool,
}

#[derive(serde::Deserialize)]
struct HInst {
    instance_id: String,
    action_id: String,
    #[serde(default)]
    inputs: Value,
}

struct PendingPi {
    sd_uuid: String,
    key: String,
    action_uuid: String,
}

pub struct Daemon {
    port: u16,
    token: String,
    regs: Mutex<HashMap<String, (Arc<Rt>, String)>>,
    pending_pi: Mutex<HashMap<String, PendingPi>>,
    rts: Mutex<HashMap<String, Arc<Rt>>>,
}

fn device_info() -> Value {
    json!({"name": "KnobDeck", "size": {"columns": COLUMNS, "rows": ROWS}, "type": 0})
}

impl Rt {
    fn host_send(&self, v: Value) {
        if let Some(s) = self.host.lock().unwrap().as_mut() {
            let _ = writeln!(s, "{v}");
        }
    }

    fn plugin_send(st: &St, v: Value) {
        if let Some(tx) = &st.plugin_tx {
            let _ = tx.send(v.to_string());
        }
    }

    fn info(&self) -> Value {
        json!({
            "application": {
                "font": "sans-serif", "language": "en",
                // Linux reports "windows": the registration schema only knows mac and windows.
                "platform": if cfg!(target_os = "macos") { "mac" } else { "windows" },
                "platformVersion": "10", "version": "7.1.0"
            },
            "colors": {
                "buttonMouseOverBackgroundColor": "#464646", "buttonPressedBackgroundColor": "#303030",
                "buttonPressedBorderColor": "#646464", "buttonPressedTextColor": "#969696", "highlightColor": "#0078ff"
            },
            "devicePixelRatio": 1,
            "devices": [{"id": DEVICE, "name": "KnobDeck", "size": {"columns": COLUMNS, "rows": ROWS}, "type": 0}],
            "plugin": {"uuid": self.meta.sd_uuid, "version": self.meta.version},
        })
    }

    fn payload(&self, i: &Inst, settings: &Value) -> Value {
        json!({
            "controller": i.map.controller,
            "coordinates": {"column": i.pos.1, "row": i.pos.0},
            "isInMultiAction": false,
            "resources": {},
            "settings": settings,
            "state": i.state,
        })
    }

    fn event(&self, event: &str, ctx: &str, i: &Inst, settings: &Value) -> Value {
        json!({"event": event, "action": i.map.uuid, "context": ctx, "device": DEVICE, "payload": self.payload(i, settings)})
    }

    fn settings_of(&self, ctx: &str, i: &Inst) -> Value {
        self.store.get(&key_for(&i.inputs, ctx))
    }

    // ── host → plugin ──

    fn on_sync(&self, instances: &[HInst]) {
        let mut st = self.st.lock().unwrap();
        let mut seen = Vec::new();
        for n in instances {
            let Some(map) = self.meta.actions.iter().find(|a| a.id == n.action_id) else { continue };
            seen.push(n.instance_id.clone());
            let key = key_for(&n.inputs, &n.instance_id);
            let old = st.instances.get(&n.instance_id).map(|i| key_for(&i.inputs, &n.instance_id));
            // A token newly entered: start from what the unconfigured instance had saved.
            if old.as_deref() != Some(&key) && !self.store.exists(&key) {
                let own = format!("i-{}", n.instance_id);
                if self.store.exists(&own) {
                    let _ = self.store.set(&key, &self.store.get(&own));
                }
            }
            match st.instances.get_mut(&n.instance_id) {
                Some(i) => {
                    i.inputs = n.inputs.clone();
                    i.action_id = n.action_id.clone();
                    if old.as_deref() != Some(&key) {
                        let ev = self.event("didReceiveSettings", &n.instance_id, i, &self.store.get(&key));
                        Self::plugin_send(&st, ev);
                    }
                }
                None => {
                    let used = st.instances.values().filter(|i| i.map.controller == map.controller).count();
                    let (cols, rows) = if map.controller == "Encoder" { (DIALS, 1) } else { (COLUMNS, ROWS) };
                    let pos = ((used / cols) % rows, used % cols);
                    let i = Inst {
                        action_id: n.action_id.clone(),
                        inputs: n.inputs.clone(),
                        map: map.clone(),
                        state: 0,
                        pos,
                        last_value: None,
                    };
                    let ev = self.event("willAppear", &n.instance_id, &i, &self.store.get(&key));
                    st.instances.insert(n.instance_id.clone(), i);
                    Self::plugin_send(&st, ev);
                }
            }
        }
        let gone: Vec<String> = st.instances.keys().filter(|k| !seen.contains(k)).cloned().collect();
        for g in gone {
            if let Some(i) = st.instances.remove(&g) {
                let ev = self.event("willDisappear", &g, &i, &self.settings_of(&g, &i));
                Self::plugin_send(&st, ev);
            }
        }
    }

    fn on_invoke(&self, id: &str, instance_id: &str, trigger: &str, inputs: &Value) {
        self.host_send(json!({"type": "ack", "id": id}));
        let mut st = self.st.lock().unwrap();
        let connected = st.plugin_tx.is_some();
        if !connected {
            drop(st);
            self.host_send(json!({"type": "error", "instance_id": instance_id, "id": id, "message": "The plugin is not running (see the bridge log)"}));
            self.host_send(json!({"type": "done", "id": id}));
            return;
        }
        let Some(mut i) = st.instances.remove(instance_id) else {
            drop(st);
            self.host_send(json!({"type": "done", "id": id}));
            return;
        };
        i.inputs = inputs.clone();
        let settings = self.settings_of(instance_id, &i);
        let mut flip = false;
        match (i.map.kind.as_str(), trigger) {
            ("slider", "adjust") => {
                let ticks = if let Some(d) = inputs["delta"].as_f64() {
                    d.round()
                } else if let Some(v) = inputs["value"].as_f64() {
                    let t = i.last_value.map(|l| v - l).unwrap_or(0.0).round();
                    i.last_value = Some(v);
                    t
                } else {
                    0.0
                };
                let mut ev = self.event("dialRotate", instance_id, &i, &settings);
                ev["payload"]["ticks"] = ticks.into();
                ev["payload"]["pressed"] = false.into();
                Self::plugin_send(&st, ev);
            }
            ("slider", _) => {}
            (_, "press") => {
                for e in ["keyDown", "keyUp"] {
                    Self::plugin_send(&st, self.event(e, instance_id, &i, &settings));
                }
                flip = i.map.kind == "toggle";
            }
            (_, "hold_start") => Self::plugin_send(&st, self.event("keyDown", instance_id, &i, &settings)),
            (_, "hold_end") => {
                Self::plugin_send(&st, self.event("keyUp", instance_id, &i, &settings));
                flip = i.map.kind == "toggle";
            }
            _ => {}
        }
        if flip && i.map.states > 1 {
            i.state = (i.state + 1) % i.map.states;
            self.host_send(json!({"type": "state", "instance_id": instance_id, "state": i.state.to_string()}));
        }
        st.instances.insert(instance_id.to_string(), i);
        drop(st);
        // "done" means delivered: Stream Deck has no completion signal.
        self.host_send(json!({"type": "done", "id": id}));
    }

    fn on_request(&self, d: &Arc<Daemon>, id: &str, method: &str, params: &Value) {
        let reply = |r: Result<Value, String>| match r {
            Ok(v) => self.host_send(json!({"type": "result", "id": id, "ok": true, "value": v})),
            Err(e) => self.host_send(json!({"type": "result", "id": id, "ok": false, "error": e})),
        };
        let qid = params["query_id"].as_str().unwrap_or("");
        let (true, Some(uuid)) = (method == "query", qid.strip_prefix("configure:")) else {
            return reply(Err(format!("unknown request {method}")));
        };
        let Some(map) = self.meta.actions.iter().find(|a| a.uuid == uuid && a.pi.is_some()) else {
            return reply(Err("this action has no settings page".into()));
        };
        let token = match params["inputs"]["sd_ref"].as_str().filter(|t| t.starts_with("sdr_")) {
            Some(t) => t.to_string(),
            None => format!("sdr_{}", random_hex(8)),
        };
        if !self.store.exists(&token) {
            let _ = self.store.set(&token, &json!({}));
        }
        let inspector = random_hex(8);
        d.pending_pi.lock().unwrap().insert(
            inspector.clone(),
            PendingPi { sd_uuid: self.meta.sd_uuid.clone(), key: token.clone(), action_uuid: uuid.to_string() },
        );
        let url = format!(
            "http://127.0.0.1:{}/pi/{}/{}?i={inspector}",
            d.port,
            bridge_core::slug(&self.meta.sd_uuid),
            map.pi.clone().unwrap()
        );
        open_url(&url);
        // The settings are saved as the user goes, so the token is the whole answer: no need to
        // block the host until the page closes.
        reply(Ok(Value::String(token)));
    }

    fn stop(self: &Arc<Self>, d: &Arc<Daemon>) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        {
            let mut st = self.st.lock().unwrap();
            let ids: Vec<String> = st.instances.keys().cloned().collect();
            for id in ids {
                let i = &st.instances[&id];
                Self::plugin_send(&st, self.event("willDisappear", &id, i, &self.settings_of(&id, i)));
            }
            st.instances.clear();
        }
        let rt = self.clone();
        let d = d.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            rt.st.lock().unwrap().plugin_tx = None;
            if let Some(c) = rt.child.lock().unwrap().as_mut() {
                let _ = c.kill();
                let _ = c.wait();
            }
            let mut rts = d.rts.lock().unwrap();
            if rts.get(&rt.meta.sd_uuid).map_or(false, |r| Arc::ptr_eq(r, &rt)) {
                rts.remove(&rt.meta.sd_uuid);
            }
        });
    }

    // ── plugin → host ──

    fn on_plugin_msg(&self, v: Value) {
        let ev = v["event"].as_str().unwrap_or("");
        let ctx = v["context"].as_str().unwrap_or("").to_string();
        let p = &v["payload"];
        match ev {
            "setTitle" => self.host_send(json!({"type": "state", "instance_id": ctx, "label": p["title"].as_str().unwrap_or("")})),
            "setImage" => {
                let icon = p["image"].as_str().map(Value::from).unwrap_or(Value::Null);
                self.host_send(json!({"type": "state", "instance_id": ctx, "icon": icon}));
            }
            "setState" => {
                if let Some(n) = p["state"].as_u64() {
                    if let Some(i) = self.st.lock().unwrap().instances.get_mut(&ctx) {
                        i.state = n as usize;
                    }
                    self.host_send(json!({"type": "state", "instance_id": ctx, "state": n.to_string()}));
                }
            }
            "showAlert" => self.host_send(json!({"type": "error", "instance_id": ctx, "message": "The plugin reported a problem"})),
            "showOk" => {}
            "setSettings" | "getSettings" => {
                let mut st = self.st.lock().unwrap();
                let Some(i) = st.instances.get(&ctx) else { return };
                let key = key_for(&i.inputs, &ctx);
                if ev == "setSettings" {
                    let _ = self.store.set(&key, p);
                }
                let mut out = self.event("didReceiveSettings", &ctx, i, &self.store.get(&key));
                if let Some(id) = v.get("id") {
                    out["id"] = id.clone();
                }
                Self::plugin_send(&st, out);
                if ev == "setSettings" {
                    // The same settings key may be open in a Property Inspector.
                    let msg = json!({"event": "didReceiveSettings", "action": i.map.uuid, "context": ctx, "device": DEVICE,
                        "payload": {"settings": self.store.get(&key), "coordinates": {"column": i.pos.1, "row": i.pos.0}, "isInMultiAction": false}});
                    for pi in st.pis.iter().filter(|p| p.key == key) {
                        let _ = pi.tx.send(msg.to_string());
                    }
                }
                let _ = &mut st;
            }
            "setGlobalSettings" | "getGlobalSettings" => {
                if ev == "setGlobalSettings" {
                    let _ = self.store.set_global(p);
                }
                let mut out = json!({"event": "didReceiveGlobalSettings", "payload": {"settings": self.store.get_global()}});
                if let Some(id) = v.get("id") {
                    out["id"] = id.clone();
                }
                Self::plugin_send(&self.st.lock().unwrap(), out);
            }
            "getResources" | "setResources" => {
                let mut out = json!({"event": "didReceiveResources", "context": ctx, "payload": {"resources": {}}});
                if let Some(id) = v.get("id") {
                    out["id"] = id.clone();
                }
                let _ = out;
            }
            "openUrl" => {
                if let Some(u) = p["url"].as_str().filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
                    open_url(u);
                }
            }
            "logMessage" => log(format!("[{}] {}", self.meta.sd_uuid, p["message"].as_str().unwrap_or(""))),
            "sendToPropertyInspector" => {
                let st = self.st.lock().unwrap();
                if let Some(i) = st.instances.get(&ctx) {
                    let key = key_for(&i.inputs, &ctx);
                    let msg = json!({"event": "sendToPropertyInspector", "action": i.map.uuid, "context": ctx, "payload": p});
                    for pi in st.pis.iter().filter(|p| p.key == key) {
                        let _ = pi.tx.send(msg.to_string());
                    }
                }
            }
            "setFeedback" => {
                // Only a dial's slider follows the plugin's own number (its bar), and its title.
                if let Some(pct) = bridge_core::feedback::bar_percent(p) {
                    let mut st = self.st.lock().unwrap();
                    if let Some(i) = st.instances.get_mut(&ctx).filter(|i| i.map.kind == "slider") {
                        i.last_value = Some(pct);
                        self.host_send(json!({"type": "state", "instance_id": ctx, "state": format!("{pct:.0}")}));
                    }
                }
                if let Some(t) = bridge_core::feedback::title(p) {
                    self.host_send(json!({"type": "state", "instance_id": ctx, "label": t}));
                }
            }
            "switchToProfile" | "setFeedbackLayout" | "setTriggerDescription" => {}
            other => log(format!("[{}] ignored command {other}", self.meta.sd_uuid)),
        }
    }
}

pub fn open_url(url: &str) {
    paths::open_url(url);
}

// ── plugin process ──

fn node_exe() -> String {
    std::env::var("KNOBDECK_BRIDGE_NODE").unwrap_or_else(|_| "node".into())
}

fn spawn_plugin(rt: &Arc<Rt>, d: &Arc<Daemon>) -> std::io::Result<Child> {
    let reg_uuid = random_hex(12).to_uppercase();
    let event = "registerPlugin".to_string();
    d.regs.lock().unwrap().insert(reg_uuid.clone(), (rt.clone(), event.clone()));
    let dir = std::path::PathBuf::from(&rt.meta.plugin_dir);
    let code = dir.join(&rt.meta.code);
    let log_path = paths::logs_dir().join(format!("{}.log", bridge_core::slug(&rt.meta.sd_uuid)));
    std::fs::create_dir_all(paths::logs_dir())?;
    let out = std::fs::OpenOptions::new().create(true).append(true).open(log_path)?;
    let mut c = if rt.meta.code.ends_with(".js") || rt.meta.code.ends_with(".mjs") || rt.meta.code.ends_with(".cjs") {
        let mut c = Command::new(node_exe());
        c.arg(&code);
        c
    } else {
        Command::new(&code)
    };
    c.current_dir(&dir)
        .args(["-port", &d.port.to_string(), "-pluginUUID", &reg_uuid, "-registerEvent", &event, "-info", &rt.info().to_string()])
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c.spawn()
}

fn supervise(rt: Arc<Rt>, d: Arc<Daemon>) {
    std::thread::spawn(move || {
        let mut crashes = 0u32;
        loop {
            match spawn_plugin(&rt, &d) {
                Ok(c) => {
                    log(format!("[{}] started pid {}", rt.meta.sd_uuid, c.id()));
                    *rt.child.lock().unwrap() = Some(c);
                }
                Err(e) => log(format!("[{}] cannot start: {e}", rt.meta.sd_uuid)),
            }
            let started = std::time::Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if rt.stopping.load(Ordering::SeqCst) {
                    return;
                }
                let exited = rt.child.lock().unwrap().as_mut().map_or(true, |c| matches!(c.try_wait(), Ok(Some(_))));
                if exited {
                    break;
                }
            }
            if started.elapsed() > Duration::from_secs(60) {
                crashes = 0;
            }
            crashes += 1;
            rt.st.lock().unwrap().plugin_tx = None;
            if crashes > 5 {
                log(format!("[{}] crashed {crashes} times, giving up", rt.meta.sd_uuid));
                let ids: Vec<String> = rt.st.lock().unwrap().instances.keys().cloned().collect();
                for id in ids {
                    rt.host_send(json!({"type": "error", "instance_id": id, "message": "The plugin keeps crashing (see the bridge log)"}));
                }
                return;
            }
            log(format!("[{}] exited, restarting in {}s", rt.meta.sd_uuid, 1 << (crashes - 1)));
            std::thread::sleep(Duration::from_secs(1 << (crashes - 1)));
        }
    });
}

// ── WebSocket / HTTP ──

fn pump(ws: &mut WebSocket<TcpStream>, rx: &Receiver<String>, mut on: impl FnMut(Value)) {
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    on(v);
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        loop {
            match rx.try_recv() {
                Ok(m) => {
                    if ws.send(Message::Text(m)).is_err() {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    return;
                }
            }
        }
    }
}

fn serve_tcp(d: Arc<Daemon>, l: TcpListener) {
    for s in l.incoming().flatten() {
        let d = d.clone();
        std::thread::spawn(move || {
            let _ = handle_tcp(d, s);
        });
    }
}

fn handle_tcp(d: Arc<Daemon>, s: TcpStream) -> anyhow::Result<()> {
    if !s.peer_addr()?.ip().is_loopback() {
        return Ok(());
    }
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut buf = [0u8; 4096];
    let n = s.peek(&mut buf)?;
    let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
    if head.contains("upgrade: websocket") {
        let mut ws = tungstenite::accept(s.try_clone()?)?;
        // The first message registers: {event, uuid}.
        let first = loop {
            match ws.read() {
                Ok(Message::Text(t)) => break serde_json::from_str::<Value>(&t)?,
                Ok(_) => {}
                Err(e) => return Err(e.into()),
            }
        };
        // Outgoing messages are sent between reads, so this is the most a message can wait.
        s.set_read_timeout(Some(Duration::from_millis(3)))?;
        let uuid = first["uuid"].as_str().unwrap_or("").to_string();
        if first["event"] == "registerPropertyInspector" {
            return pi_session(&d, ws, &uuid);
        }
        let reg = d.regs.lock().unwrap().remove(&uuid);
        let Some((rt, event)) = reg else { return Ok(()) };
        if first["event"] != event.as_str() {
            return Ok(());
        }
        return plugin_session(&d, rt, ws);
    }
    http(&d, s)
}

fn plugin_session(_d: &Arc<Daemon>, rt: Arc<Rt>, mut ws: WebSocket<TcpStream>) -> anyhow::Result<()> {
    let (tx, rx) = channel::<String>();
    {
        // Under the lock, so a sync can't slip between the snapshot and going live.
        let mut st = rt.st.lock().unwrap();
        st.plugin_tx = Some(tx.clone());
        let _ = tx.send(json!({"event": "deviceDidConnect", "device": DEVICE, "deviceInfo": device_info()}).to_string());
        let _ = tx.send(json!({"event": "didReceiveGlobalSettings", "payload": {"settings": rt.store.get_global()}}).to_string());
        for (id, i) in st.instances.iter() {
            let _ = tx.send(rt.event("willAppear", id, i, &rt.settings_of(id, i)).to_string());
        }
    }
    log(format!("[{}] registered", rt.meta.sd_uuid));
    pump(&mut ws, &rx, |v| rt.on_plugin_msg(v));
    rt.st.lock().unwrap().plugin_tx = None;
    Ok(())
}

fn pi_session(d: &Arc<Daemon>, mut ws: WebSocket<TcpStream>, inspector: &str) -> anyhow::Result<()> {
    let Some(p) = d.pending_pi.lock().unwrap().get(inspector).map(|p| PendingPi { sd_uuid: p.sd_uuid.clone(), key: p.key.clone(), action_uuid: p.action_uuid.clone() }) else {
        return Ok(());
    };
    let Some(rt) = d.rts.lock().unwrap().get(&p.sd_uuid).cloned() else { return Ok(()) };
    let (tx, rx) = channel::<String>();
    let ctx_instances = |st: &St| -> Vec<String> {
        st.instances.iter().filter(|(id, i)| key_for(&i.inputs, id) == p.key && i.map.uuid == p.action_uuid).map(|(id, _)| id.clone()).collect()
    };
    {
        let mut st = rt.st.lock().unwrap();
        st.pis.push(PiConn { inspector: inspector.to_string(), key: p.key.clone(), tx: tx.clone() });
        for id in ctx_instances(&st) {
            let i = &st.instances[&id];
            Rt::plugin_send(&st, json!({"event": "propertyInspectorDidAppear", "action": i.map.uuid, "context": id, "device": DEVICE}));
        }
    }
    let action_uuid = p.action_uuid.clone();
    let key = p.key.clone();
    pump(&mut ws, &rx, |v| {
        let ev = v["event"].as_str().unwrap_or("");
        let payload = &v["payload"];
        let st = rt.st.lock().unwrap();
        match ev {
            "getSettings" => {
                let m = json!({"event": "didReceiveSettings", "action": action_uuid, "context": inspector, "device": DEVICE,
                    "payload": {"settings": rt.store.get(&key), "coordinates": {"column": 0, "row": 0}, "isInMultiAction": false}});
                let _ = tx.send(m.to_string());
            }
            "setSettings" => {
                let _ = rt.store.set(&key, payload);
                for id in ctx_instances(&st) {
                    let i = &st.instances[&id];
                    Rt::plugin_send(&st, rt.event("didReceiveSettings", &id, i, &rt.store.get(&key)));
                }
            }
            "getGlobalSettings" => {
                let _ = tx.send(json!({"event": "didReceiveGlobalSettings", "payload": {"settings": rt.store.get_global()}}).to_string());
            }
            "setGlobalSettings" => {
                let _ = rt.store.set_global(payload);
                let m = json!({"event": "didReceiveGlobalSettings", "payload": {"settings": rt.store.get_global()}});
                Rt::plugin_send(&st, m.clone());
                for pi in st.pis.iter() {
                    let _ = pi.tx.send(m.to_string());
                }
            }
            "sendToPlugin" => {
                if let Some(id) = ctx_instances(&st).first() {
                    Rt::plugin_send(&st, json!({"event": "sendToPlugin", "action": action_uuid, "context": id, "payload": payload}));
                }
            }
            "openUrl" => {
                if let Some(u) = payload["url"].as_str().filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
                    open_url(u);
                }
            }
            "logMessage" => log(format!("[{} PI] {}", rt.meta.sd_uuid, payload["message"].as_str().unwrap_or(""))),
            _ => {}
        }
    });
    let mut st = rt.st.lock().unwrap();
    st.pis.retain(|c| c.inspector != inspector);
    for id in ctx_instances(&st) {
        let i = &st.instances[&id];
        Rt::plugin_send(&st, json!({"event": "propertyInspectorDidDisappear", "action": i.map.uuid, "context": id, "device": DEVICE}));
    }
    d.pending_pi.lock().unwrap().remove(inspector);
    Ok(())
}

const MAX_UPLOAD: usize = 300 * 1024 * 1024;

fn json_reply(s: &mut TcpStream, code: &str, v: Value) {
    let body = v.to_string();
    let _ = write!(s, "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

/// The adapter's own page and its API (`/`, `/api/*`). Needs the daemon's token and a loopback Host.
fn api(d: &Arc<Daemon>, s: &mut TcpStream, method: &str, path: &str, headers: &HashMap<String, String>, body: Vec<u8>) {
    if path == "/" {
        let page = include_str!("ui.html").as_bytes();
        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", page.len());
        let _ = s.write_all(page);
        return;
    }
    if headers.get("x-adapter-token").map(String::as_str) != Some(d.token.as_str()) {
        return json_reply(s, "401 Unauthorized", json!({"error": "open this page from KnobDeck"}));
    }
    match (method, path) {
        ("GET", "/api/state") => {
            let plugins: Vec<Value> = bridge_core::import::list()
                .iter()
                .map(|m| json!({"uuid": m.sd_uuid, "name": m.name, "version": m.version, "actions": m.actions.len()}))
                .collect();
            json_reply(s, "200 OK", json!({"plugins": plugins}));
        }
        ("POST", "/api/import") => {
            let name = headers.get("x-filename").map(|n| percent_decode(n)).unwrap_or_default();
            let tmp = paths::data_dir().join("tmp");
            let _ = std::fs::create_dir_all(&tmp);
            let file = tmp.join(format!("upload-{}.zip", random_hex(4)));
            let result = (|| -> anyhow::Result<Value> {
                std::fs::write(&file, &body)?;
                let exe = std::env::current_exe()?;
                let shim = exe.with_file_name(format!("bridge-shim{}", std::env::consts::EXE_SUFFIX));
                let r = bridge_core::import::import(&file, &shim, &exe)?;
                let shown = bridge_core::import::list().into_iter().find(|m| manifest::knobdeck_id(&m.sd_uuid) == r.knobdeck_id);
                Ok(json!({"id": r.knobdeck_id, "name": shown.map(|m| m.name).unwrap_or(name), "notes": r.notes}))
            })();
            let _ = std::fs::remove_file(&file);
            match result {
                Ok(v) => json_reply(s, "200 OK", v),
                Err(e) => json_reply(s, "400 Bad Request", json!({"error": format!("{e:#}")})),
            }
        }
        ("POST", "/api/remove") => {
            let uuid = serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["uuid"].as_str().map(str::to_owned)).unwrap_or_default();
            match bridge_core::import::remove(&uuid, false) {
                Ok(()) => json_reply(s, "200 OK", json!({"ok": true})),
                Err(e) => json_reply(s, "400 Bad Request", json!({"error": format!("{e:#}")})),
            }
        }
        _ => json_reply(s, "404 Not Found", json!({"error": "not found"})),
    }
}

fn http(d: &Arc<Daemon>, mut s: TcpStream) -> anyhow::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut it = line.split_whitespace();
    let method = it.next().unwrap_or("").to_string();
    let target = it.next().unwrap_or("/").to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    // A page on another site must not reach this server through the browser (DNS rebinding).
    let host_ok = headers.get("host").map_or(false, |h| h == &format!("127.0.0.1:{}", d.port) || h == &format!("localhost:{}", d.port));
    if !host_ok {
        return Ok(());
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let respond = |s: &mut TcpStream, code: &str, ctype: &str, body: &[u8]| {
        let _ = write!(s, "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        let _ = s.write_all(body);
    };
    if path == "/" || path.starts_with("/api/") {
        let len: usize = headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
        if len > MAX_UPLOAD {
            return Ok(json_reply(&mut s, "413 Payload Too Large", json!({"error": "file is too large"})));
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body)?;
        api(d, &mut s, &method, path, &headers, body);
        return Ok(());
    }
    // /pi/<plugin slug>/<file>
    let mut parts = path.trim_start_matches('/').splitn(3, '/');
    let (Some("pi"), Some(slug), Some(rel)) = (parts.next(), parts.next(), parts.next()) else {
        respond(&mut s, "404 Not Found", "text/plain", b"not found");
        return Ok(());
    };
    let rts = d.rts.lock().unwrap();
    let Some(rt) = rts.values().find(|r| bridge_core::slug(&r.meta.sd_uuid) == slug).cloned() else {
        respond(&mut s, "404 Not Found", "text/plain", b"plugin is not running");
        return Ok(());
    };
    drop(rts);
    let root = std::fs::canonicalize(&rt.meta.plugin_dir)?;
    let rel = percent_decode(rel);
    let file = std::fs::canonicalize(root.join(&rel));
    let Some(file) = file.ok().filter(|f| f.starts_with(&root) && f.is_file()) else {
        respond(&mut s, "404 Not Found", "text/plain", b"not found");
        return Ok(());
    };
    let mut body = Vec::new();
    std::fs::File::open(&file)?.read_to_end(&mut body)?;
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let ctype = match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    };
    if ext == "html" || ext == "htm" {
        let inspector = query.split('&').find_map(|kv| kv.strip_prefix("i=")).unwrap_or("");
        if let Some(p) = d.pending_pi.lock().unwrap().get(inspector) {
            let i = {
                let st = rt.st.lock().unwrap();
                let a = st.instances.values().find(|i| i.map.uuid == p.action_uuid);
                a.map(|i| (i.map.controller.clone(), (i.pos.1, i.pos.0)))
            };
            let (controller, (col, row)) = i.unwrap_or(("Keypad".into(), (0, 0)));
            let action_info = json!({
                "action": p.action_uuid, "context": inspector, "device": DEVICE,
                "payload": {"controller": controller, "coordinates": {"column": col, "row": row},
                            "isInMultiAction": false, "settings": rt.store.get(&p.key)}
            });
            let lit = |v: &Value| serde_json::to_string(&v.to_string()).unwrap().replace("</", "<\\/");
            let script = format!(
                "<script>window.addEventListener('load',function(){{if(window.connectElgatoStreamDeckSocket)window.connectElgatoStreamDeckSocket({},\"{}\",\"registerPropertyInspector\",{},{});}});</script>",
                d.port, inspector, lit(&rt.info()), lit(&action_info)
            );
            let mut html = String::from_utf8_lossy(&body).into_owned();
            match html.rfind("</body>") {
                Some(at) => html.insert_str(at, &script),
                None => html.push_str(&script),
            }
            body = html.into_bytes();
        }
    }
    respond(&mut s, "200 OK", ctype, &body);
    Ok(())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── shim sessions ──

fn stop_all(d: &Arc<Daemon>) {
    let all: Vec<Arc<Rt>> = d.rts.lock().unwrap().values().cloned().collect();
    for rt in &all {
        rt.stop(d);
    }
    std::thread::sleep(Duration::from_millis(600));
    let _ = std::fs::remove_file(paths::ports_path());
    log("daemon stopped");
    std::process::exit(0);
}

fn session(d: Arc<Daemon>, s: TcpStream) -> anyhow::Result<()> {
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut r = BufReader::new(s.try_clone()?);
    let mut first = String::new();
    r.read_line(&mut first)?;
    s.set_read_timeout(None)?;
    let hs: Value = serde_json::from_str(&first)?;
    if hs["token"].as_str() != Some(d.token.as_str()) {
        return Ok(());
    }
    match hs["cmd"].as_str() {
        Some("stop") => stop_all(&d),
        Some("ping") => return Ok(()),
        _ => {}
    }
    let sd_uuid = hs["plugin"].as_str().unwrap_or("").to_string();
    let meta: BridgeMeta = std::fs::read(paths::generated_dir().join(manifest::knobdeck_id(&sd_uuid)).join("bridge.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| anyhow::anyhow!("unknown plugin {sd_uuid}"))?;
    let rt = Arc::new(Rt {
        store: Store::new(&meta.sd_uuid),
        meta,
        host: Mutex::new(Some(s)),
        st: Mutex::new(St::default()),
        child: Mutex::new(None),
        stopping: AtomicBool::new(false),
    });
    if let Some(old) = d.rts.lock().unwrap().insert(sd_uuid.clone(), rt.clone()) {
        old.stop(&d);
    }
    log(format!("[{sd_uuid}] host connected"));
    for line in r.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        match v["type"].as_str() {
            Some("hello") => {
                supervise(rt.clone(), d.clone());
                rt.host_send(json!({"type": "ready", "protocol": 2, "done": true}));
            }
            Some("sync") => {
                let list: Vec<HInst> = serde_json::from_value(v["instances"].clone()).unwrap_or_default();
                rt.on_sync(&list);
            }
            Some("invoke") => rt.on_invoke(
                v["id"].as_str().unwrap_or(""),
                v["instance_id"].as_str().unwrap_or(""),
                v["trigger"].as_str().unwrap_or(""),
                &v["inputs"],
            ),
            Some("request") => rt.on_request(&d, v["id"].as_str().unwrap_or(""), v["method"].as_str().unwrap_or(""), &v["params"]),
            Some("shutdown") => break,
            _ => {}
        }
    }
    log(format!("[{sd_uuid}] host disconnected"));
    *rt.host.lock().unwrap() = None;
    rt.stop(&d);
    Ok(())
}

/// Whether a daemon answers on the control port.
pub fn running() -> bool {
    let Some((ctl, _)) = paths::read_ports() else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], ctl)), Duration::from_millis(500)) else {
        return false;
    };
    let token = std::fs::read_to_string(paths::token_path()).unwrap_or_default();
    writeln!(s, "{}", json!({"token": token.trim(), "cmd": "ping"})).is_ok()
}

/// Ask a running daemon to stop; true when one was running.
pub fn stop() -> bool {
    let Some((ctl, _)) = paths::read_ports() else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], ctl)), Duration::from_millis(500)) else {
        return false;
    };
    let token = std::fs::read_to_string(paths::token_path()).unwrap_or_default();
    let _ = writeln!(s, "{}", json!({"token": token.trim(), "cmd": "stop"}));
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        if !running() {
            break;
        }
    }
    true
}

/// The page URL of the running daemon (starting it first when needed).
pub fn ui_url() -> anyhow::Result<String> {
    if !running() {
        paths::spawn_detached(&std::env::current_exe()?, &["daemon"])?;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if running() {
                break;
            }
        }
    }
    let (_, http) = paths::read_ports().ok_or_else(|| anyhow::anyhow!("the daemon did not start"))?;
    let token = std::fs::read_to_string(paths::token_path())?;
    Ok(format!("http://127.0.0.1:{http}/?t={}", token.trim()))
}

pub fn run() -> anyhow::Result<()> {
    let run_dir = paths::run_dir();
    std::fs::create_dir_all(&run_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if running() {
        println!("The adapter daemon is already running.");
        return Ok(());
    }
    let token = random_hex(16);
    {
        let mut o = std::fs::OpenOptions::new();
        o.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        o.open(paths::token_path())?.write_all(token.as_bytes())?;
    }
    let tcp = TcpListener::bind("127.0.0.1:0")?;
    let ctl = TcpListener::bind("127.0.0.1:0")?;
    let port = tcp.local_addr()?.port();
    let d = Arc::new(Daemon {
        port,
        token,
        regs: Mutex::new(HashMap::new()),
        pending_pi: Mutex::new(HashMap::new()),
        rts: Mutex::new(HashMap::new()),
    });
    std::fs::write(paths::ports_path(), json!({"ctl": ctl.local_addr()?.port(), "http": port}).to_string())?;
    log(format!("adapter daemon up: control 127.0.0.1:{} · ws/http 127.0.0.1:{port}", ctl.local_addr()?.port()));
    println!("KnobDeck SD Plugin Adapter daemon running (Ctrl+C to stop). Log: {}", paths::logs_dir().join("daemon.log").display());
    {
        let d = d.clone();
        std::thread::spawn(move || serve_tcp(d, tcp));
    }
    for s in ctl.incoming().flatten() {
        if !s.peer_addr().map_or(false, |a| a.ip().is_loopback()) {
            continue;
        }
        let d = d.clone();
        std::thread::spawn(move || {
            if let Err(e) = session(d, s) {
                log(format!("session ended: {e}"));
            }
        });
    }
    Ok(())
}

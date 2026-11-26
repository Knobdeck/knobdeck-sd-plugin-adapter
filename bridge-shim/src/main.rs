//! The executable every generated KnobDeck plugin points at. It holds no logic: it relays the
//! host's protocol-v2 lines to the bridge daemon and the daemon's lines back. If the daemon
//! cannot be reached it still answers the host, with an error that says what to start.

use bridge_core::{manifest::BridgeMeta, paths};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

fn connect_once() -> Option<std::net::TcpStream> {
    let (ctl, _) = paths::read_ports()?;
    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], ctl)), Duration::from_millis(500)).ok()
}

fn connect(meta: &BridgeMeta) -> Option<std::net::TcpStream> {
    if let Some(s) = connect_once() {
        return Some(s);
    }
    // Start the daemon once, detached, and wait a few seconds for it.
    paths::spawn_detached(std::path::Path::new(&meta.daemon_exe), &["daemon"]).ok()?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(s) = connect_once() {
            return Some(s);
        }
    }
    None
}

fn degraded(why: &str) {
    let out = std::io::stdout();
    let mut out = out.lock();
    let mut put = |v: Value| {
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    };
    for line in BufReader::new(std::io::stdin()).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        match v["type"].as_str() {
            Some("hello") => put(json!({"type":"ready","protocol":2,"done":true})),
            Some("invoke") => {
                put(json!({"type":"ack","id":v["id"]}));
                put(json!({"type":"error","instance_id":v["instance_id"],"id":v["id"],"message":why}));
                put(json!({"type":"done","id":v["id"]}));
            }
            Some("request") => put(json!({"type":"result","id":v["id"],"ok":false,"error":why})),
            Some("shutdown") => return,
            _ => {}
        }
    }
}

fn main() {
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default();
    let Some(meta) = std::fs::read(dir.join("bridge.json")).ok().and_then(|b| serde_json::from_slice::<BridgeMeta>(&b).ok()) else {
        eprintln!("bridge-shim: bridge.json is missing next to the shim");
        return degraded("bridge.json is missing; import the plugin again");
    };
    let Some(sock) = connect(&meta) else {
        return degraded("Start KnobDeck Bridge (knobdeck-bridge daemon)");
    };
    let token = std::fs::read_to_string(paths::token_path()).unwrap_or_default();
    let mut w = sock.try_clone().expect("clone socket");
    let _ = writeln!(w, "{}", json!({"token": token.trim(), "plugin": meta.sd_uuid}));

    // stdin -> daemon
    let t = std::thread::spawn(move || {
        for line in BufReader::new(std::io::stdin()).lines().map_while(Result::ok) {
            if writeln!(w, "{line}").is_err() {
                break;
            }
        }
        let _ = w.shutdown(std::net::Shutdown::Write);
    });
    // daemon -> stdout
    let out = std::io::stdout();
    for line in BufReader::new(sock).lines().map_while(Result::ok) {
        let mut o = out.lock();
        if writeln!(o, "{line}").and_then(|_| o.flush()).is_err() {
            break;
        }
    }
    let _ = t.join();
}

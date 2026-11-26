//! Stream Deck `manifest.json` in, KnobDeck `manifest.json` + `actions.json` + `bridge.json` out.

use crate::slug;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SdManifest {
    #[serde(rename = "UUID")]
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default, rename = "SDKVersion")]
    pub sdk_version: Option<u32>,
    #[serde(default)]
    pub code_path: Option<String>,
    #[serde(default)]
    pub code_path_win: Option<String>,
    #[serde(default)]
    pub code_path_mac: Option<String>,
    #[serde(default)]
    pub code_path_linux: Option<String>,
    #[serde(default)]
    pub property_inspector_path: Option<String>,
    #[serde(default)]
    pub nodejs: Option<Value>,
    #[serde(default, rename = "OS")]
    pub os: Vec<Value>,
    #[serde(default)]
    pub actions: Vec<SdAction>,
    #[serde(default)]
    pub applications_to_monitor: Option<Value>,
    #[serde(default)]
    pub profiles: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SdAction {
    #[serde(rename = "UUID")]
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub controllers: Vec<String>,
    #[serde(default)]
    pub states: Vec<SdState>,
    #[serde(default)]
    pub property_inspector_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SdState {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// What the daemon needs to know about one generated action (`bridge.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionMap {
    /// KnobDeck's full action id.
    pub id: String,
    /// The Stream Deck action UUID.
    pub uuid: String,
    /// `Keypad` or `Encoder`.
    pub controller: String,
    /// `button`, `toggle` or `slider`.
    pub kind: String,
    pub states: usize,
    pub pi: Option<String>,
}

/// `bridge.json`, next to the generated plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeMeta {
    pub sd_uuid: String,
    pub name: String,
    pub version: String,
    /// The extracted Stream Deck plugin folder.
    pub plugin_dir: String,
    /// What to run, relative to `plugin_dir`.
    pub code: String,
    pub daemon_exe: String,
    pub actions: Vec<ActionMap>,
}

pub fn parse(dir: &Path) -> Result<SdManifest> {
    let p = dir.join("manifest.json");
    let raw = std::fs::read_to_string(&p).with_context(|| format!("no manifest.json in {}", dir.display()))?;
    // Real manifests are plain JSON, but some carry a BOM.
    let m: SdManifest = serde_json::from_str(raw.trim_start_matches('\u{feff}')).context("manifest.json is not valid")?;
    Ok(m)
}

pub fn knobdeck_id(sd_uuid: &str) -> String {
    format!("sd-{}", slug(sd_uuid))
}

/// Same rule as the SDK's `make_full_id`: `<plugin>-<action>-<sha1(plugin:action)[..8]>`.
pub fn full_id(plugin_id: &str, action_id: &str) -> String {
    let digest = Sha1::digest(format!("{plugin_id}:{action_id}").as_bytes());
    let hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{plugin_id}-{action_id}-{hash}")
}

/// The file to run on this OS, or why there is none.
pub fn code_for_this_os(m: &SdManifest) -> Result<String> {
    let os_pick = if cfg!(windows) {
        m.code_path_win.clone()
    } else if cfg!(target_os = "macos") {
        m.code_path_mac.clone()
    } else {
        m.code_path_linux.clone()
    };
    if let Some(p) = os_pick.or_else(|| m.code_path.clone()) {
        return Ok(p);
    }
    bail!("this plugin has no code for this operating system")
}

fn local_name(plugin: &str, action: &str) -> String {
    let rest = action.strip_prefix(plugin).unwrap_or(action);
    let s = slug(rest);
    if s.is_empty() { slug(action) } else { s }
}

/// Everything generated from one Stream Deck manifest, and the notes for the import report.
pub struct Generated {
    pub manifest: Value,
    pub actions: Value,
    pub actions_map: Vec<ActionMap>,
    pub notes: Vec<String>,
}

pub fn generate(m: &SdManifest, icon_file: Option<&str>) -> Generated {
    let id = knobdeck_id(&m.uuid);
    let mut notes = Vec::new();
    let mut list = Vec::new();
    let mut map = Vec::new();

    for a in &m.actions {
        let controllers: Vec<String> = if a.controllers.is_empty() { vec!["Keypad".into()] } else { a.controllers.clone() };
        let pi = a.property_inspector_path.clone().or_else(|| m.property_inspector_path.clone());
        for c in &controllers {
            if c != "Keypad" && c != "Encoder" {
                notes.push(format!("{}: controller {c} is not supported and was skipped", a.name));
                continue;
            }
            let both = controllers.len() > 1;
            let mut local = local_name(&m.uuid, &a.uuid);
            if both {
                local.push_str(if c == "Keypad" { "-key" } else { "-dial" });
            }
            let fid = full_id(&id, &local);
            let label = if both { format!("{} ({})", a.name, if c == "Keypad" { "key" } else { "dial" }) } else { a.name.clone() };

            let kind = if c == "Encoder" {
                "slider"
            } else if a.states.len() >= 2 {
                "toggle"
            } else {
                "button"
            };
            let mut e = json!({
                "id": fid, "label": label, "type": kind, "icon": "puzzle", "inputs": [],
            });
            if let Some(t) = &a.tooltip {
                e["description"] = t.clone().into();
            }
            if pi.is_some() {
                e["inputs"] = json!([{
                    "key": "sd_ref", "label": "External plugin configuration", "type": "text",
                    "placeholder": "Not configured", "query": format!("configure:{}", a.uuid), "queryLabel": "Configure"
                }]);
            }
            match kind {
                "toggle" => {
                    let states: Vec<Value> = a
                        .states
                        .iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let n = s.name.clone().filter(|n| !n.is_empty()).or_else(|| s.title.clone().filter(|t| !t.is_empty()));
                            json!({"value": i.to_string(), "label": n.unwrap_or_else(|| format!("State {}", i + 1)), "icon": "puzzle"})
                        })
                        .collect();
                    e["states"] = Value::Array(states);
                }
                "slider" => {
                    e["min"] = 0.into();
                    e["max"] = 100.into();
                    e["step"] = 1.into();
                }
                _ => {}
            }
            list.push(e);
            map.push(ActionMap {
                id: fid,
                uuid: a.uuid.clone(),
                controller: c.clone(),
                kind: kind.into(),
                states: a.states.len().max(1),
                pi: pi.clone(),
            });
        }
    }
    if !m.profiles.is_empty() {
        notes.push("profiles shipped with the plugin are ignored".into());
    }
    if m.applications_to_monitor.is_some() {
        notes.push("ApplicationsToMonitor is not forwarded yet (applicationDidLaunch is never sent)".into());
    }

    let mut manifest = json!({
        "id": id, "name": m.name, "protocol": 2,
        "runtime": { "executable": format!("bridge-shim{}", std::env::consts::EXE_SUFFIX), "query_timeout_ms": 20000 },
        "version": if m.version.is_empty() { "0.0.0" } else { &m.version },
        "author": if m.author.is_empty() { "Unknown" } else { &m.author },
        "description": m.description,
    });
    if let Some(i) = icon_file {
        manifest["icon"] = i.into();
    }
    Generated { manifest, actions: json!({ "actions": list }), actions_map: map, notes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SdManifest {
        serde_json::from_value(json!({
            "UUID": "com.acme.thing", "Name": "Thing", "Version": "1.0.0.0", "CodePath": "bin/plugin.js",
            "PropertyInspectorPath": "ui/pi.html",
            "Actions": [
                {"UUID": "com.acme.thing.count", "Name": "Count", "Controllers": ["Keypad","Encoder"], "States": [{}]},
                {"UUID": "com.acme.thing.switch", "Name": "Switch", "States": [{"Name":"Off"},{"Name":"On"}]}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn splits_controllers_and_toggles() {
        let g = generate(&sample(), None);
        let kinds: Vec<_> = g.actions_map.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["button", "slider", "toggle"]);
        assert_eq!(g.actions["actions"][2]["states"][1]["label"], "On");
        assert_eq!(g.actions["actions"][0]["inputs"][0]["key"], "sd_ref");
        assert!(g.actions_map[0].id.starts_with("sd-com-acme-thing-count-key-"));
    }

    #[test]
    fn ids_are_stable() {
        assert_eq!(full_id("discord", "mute"), "discord-mute-b094f5e1");
        assert_eq!(knobdeck_id("com.Acme.Thing"), "sd-com-acme-thing");
    }
}

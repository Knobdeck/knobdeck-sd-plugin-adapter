//! Settings the Stream Deck plugin reads and writes. They live here and never reach the host.
//! A key is the `sd_ref` token of an instance, or `i-<instance id>` before it is configured.

use crate::{paths, slug};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(plugin_uuid: &str) -> Self {
        Store { dir: paths::settings_dir().join(slug(plugin_uuid)) }
    }
    fn file(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", slug(key)))
    }
    pub fn get(&self, key: &str) -> Value {
        std::fs::read(self.file(key))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    }
    pub fn exists(&self, key: &str) -> bool {
        self.file(key).exists()
    }
    pub fn set(&self, key: &str, v: &Value) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let v = if v.is_object() { v.clone() } else { Value::Object(Map::new()) };
        std::fs::write(self.file(key), serde_json::to_vec_pretty(&v)?)
    }
    pub fn get_global(&self) -> Value {
        self.get("_global")
    }
    pub fn set_global(&self, v: &Value) -> std::io::Result<()> {
        self.set("_global", v)
    }
}

/// The store key for an instance: its token when it has one, else its instance id.
pub fn key_for(inputs: &Value, instance_id: &str) -> String {
    match inputs.get("sd_ref").and_then(Value::as_str).filter(|t| t.starts_with("sdr_")) {
        Some(t) => t.to_string(),
        None => format!("i-{instance_id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_prefers_token() {
        assert_eq!(key_for(&json!({"sd_ref":"sdr_ab"}), "i1"), "sdr_ab");
        assert_eq!(key_for(&json!({"sd_ref":""}), "i1"), "i-i1");
        assert_eq!(key_for(&json!({}), "i1"), "i-i1");
    }
}

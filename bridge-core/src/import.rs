//! Import a `.streamDeckPlugin` (zip) or a folder, generate the KnobDeck shim plugin, register it.

use crate::manifest::{self, BridgeMeta};
use crate::{paths, slug};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MAX_FILES: usize = 5000;
const MAX_BYTES: u64 = 500 * 1024 * 1024;

pub struct Report {
    pub knobdeck_id: String,
    pub dir: PathBuf,
    pub notes: Vec<String>,
}

/// Extract `zip_path` into `dest`, refusing paths that leave it.
pub fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path)?).context("not a zip file")?;
    if z.len() > MAX_FILES {
        bail!("archive has too many files");
    }
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let rel = f.enclosed_name().ok_or_else(|| anyhow!("unsafe path in archive: {}", f.name()))?;
        let out = dest.join(&rel);
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            bail!("unsafe path in archive: {}", f.name());
        }
        if f.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        total += f.size();
        if total > MAX_BYTES {
            bail!("archive is too large");
        }
        if let Some(p) = out.parent() {
            fs::create_dir_all(p)?;
        }
        let mut buf = Vec::new();
        f.by_ref().take(MAX_BYTES).read_to_end(&mut buf)?;
        fs::write(&out, buf)?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

/// The folder with `manifest.json`: the root, or the single `*.sdPlugin` folder inside it.
fn find_root(dir: &Path) -> Result<PathBuf> {
    if dir.join("manifest.json").exists() {
        return Ok(dir.to_path_buf());
    }
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() && p.join("manifest.json").exists() {
            return Ok(p);
        }
    }
    bail!("no manifest.json found")
}

/// A Stream Deck image path has no extension; find the file.
fn find_image(root: &Path, base: &str) -> Option<PathBuf> {
    for ext in ["png", "svg", "gif", "jpg"] {
        for suffix in ["", "@2x"] {
            let p = root.join(format!("{base}{suffix}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let p = root.join(base);
    p.is_file().then_some(p)
}

pub fn import(src: &Path, shim_exe: &Path, daemon_exe: &Path) -> Result<Report> {
    let tmp = paths::data_dir().join("tmp").join(crate::random_hex(4));
    fs::create_dir_all(&tmp)?;
    let result = import_inner(src, &tmp, shim_exe, daemon_exe);
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn import_inner(src: &Path, tmp: &Path, shim_exe: &Path, daemon_exe: &Path) -> Result<Report> {
    let staged = tmp.join("src");
    if src.is_dir() {
        copy_dir(src, &staged)?;
    } else {
        extract_zip(src, &staged)?;
    }
    let root = find_root(&staged)?;
    let m = manifest::parse(&root)?;
    let mut notes = Vec::new();

    match m.sdk_version {
        Some(2) | Some(3) | None => {}
        Some(v) => bail!("SDKVersion {v} is not supported (2 and 3 are)"),
    }
    let code = manifest::code_for_this_os(&m)?;
    if !root.join(&code).exists() {
        bail!("the plugin's code file {code} is missing");
    }
    let is_js = code.ends_with(".js") || code.ends_with(".mjs") || code.ends_with(".cjs");
    if !is_js {
        notes.push(format!("{code} looks like a native program; it runs only if it is built for this OS"));
    }
    // Node plugins run on any OS that has Node, whatever their `OS` list says (it only ever names
    // mac and windows). Say something only when the plugin ships a program for other systems.
    if !is_js && !m.os.is_empty() {
        let here = if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "mac" } else { "linux" };
        if !m.os.iter().any(|o| o["Platform"].as_str() == Some(here)) {
            notes.push(format!("the plugin does not list {here} in OS and runs a native program; it is imported anyway and may not work"));
        }
    }

    // Move the plugin into place.
    let dest = paths::plugins_dir().join(slug(&m.uuid));
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_dir(&root, &dest)?;

    // Generated plugin folder.
    let id = manifest::knobdeck_id(&m.uuid);
    let gen_dir = paths::generated_dir().join(&id);
    if gen_dir.exists() {
        fs::remove_dir_all(&gen_dir)?;
    }
    fs::create_dir_all(gen_dir.join("icons"))?;
    let icon_file = m.icon.as_deref().and_then(|b| find_image(&dest, b)).and_then(|p| {
        let name = format!("app-icon.{}", p.extension()?.to_string_lossy());
        fs::copy(&p, gen_dir.join("icons").join(&name)).ok()?;
        fs::copy(&p, gen_dir.join(&name)).ok()?;
        Some(name)
    });
    let g = manifest::generate(&m, icon_file.as_deref());
    notes.extend(g.notes.clone());

    let shim_name = format!("bridge-shim{}", std::env::consts::EXE_SUFFIX);
    fs::copy(shim_exe, gen_dir.join(&shim_name)).with_context(|| format!("copy shim from {}", shim_exe.display()))?;
    fs::write(gen_dir.join("manifest.json"), serde_json::to_vec_pretty(&g.manifest)?)?;
    fs::write(gen_dir.join("actions.json"), serde_json::to_vec_pretty(&g.actions)?)?;
    let meta = BridgeMeta {
        sd_uuid: m.uuid.clone(),
        name: m.name.clone(),
        version: m.version.clone(),
        plugin_dir: dest.to_string_lossy().into_owned(),
        code,
        daemon_exe: daemon_exe.to_string_lossy().into_owned(),
        actions: g.actions_map,
    };
    fs::write(gen_dir.join("bridge.json"), serde_json::to_vec_pretty(&meta)?)?;

    // Same as the host's own install: cache the icons, then a normal (not unpacked) registry entry.
    let icons = paths::host_icons_dir(&id);
    let _ = fs::remove_dir_all(&icons);
    copy_dir(&gen_dir.join("icons"), &icons)?;
    register(&id, &m.name, &gen_dir)?;
    Ok(Report { knobdeck_id: id, dir: gen_dir, notes })
}

fn registry_path() -> PathBuf {
    paths::host_config_dir().join("plugin-registry.json")
}

/// Add (or replace) the plugin's entry in KnobDeck's registry, as an installed plugin.
pub fn register(id: &str, name: &str, dir: &Path) -> Result<()> {
    let p = registry_path();
    let mut list: Vec<Value> = fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    list.retain(|e| e["id"] != id);
    list.push(json!({"id": id, "name": name, "dir": dir.to_string_lossy(), "unpacked": false}));
    fs::create_dir_all(p.parent().unwrap())?;
    fs::write(&p, serde_json::to_vec_pretty(&list)?)?;
    Ok(())
}

pub fn unregister(id: &str) -> Result<()> {
    let p = registry_path();
    if let Some(mut list) = fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok()) {
        list.retain(|e| e["id"] != id);
        fs::write(&p, serde_json::to_vec_pretty(&list)?)?;
    }
    Ok(())
}

/// Imported plugins, from the generated folders.
pub fn list() -> Vec<BridgeMeta> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(paths::generated_dir()) {
        for e in rd.flatten() {
            if let Some(m) = fs::read(e.path().join("bridge.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
                out.push(m);
            }
        }
    }
    out
}

pub fn remove(sd_uuid: &str, with_settings: bool) -> Result<()> {
    let id = manifest::knobdeck_id(sd_uuid);
    unregister(&id)?;
    let _ = fs::remove_dir_all(paths::generated_dir().join(&id));
    let _ = fs::remove_dir_all(paths::host_icons_dir(&id));
    let _ = fs::remove_dir_all(paths::plugins_dir().join(slug(sd_uuid)));
    if with_settings {
        let _ = fs::remove_dir_all(paths::settings_dir().join(slug(sd_uuid)));
    }
    Ok(())
}

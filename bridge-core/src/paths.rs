use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default()
}

/// The bridge's own data folder (`KNOBDECK_BRIDGE_HOME` overrides it).
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("KNOBDECK_BRIDGE_HOME") {
        return PathBuf::from(p);
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/knobdeck-bridge")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join("knobdeck-bridge")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("knobdeck-bridge")
    }
}

/// KnobDeck's config folder, where `plugin-registry.json` lives (`KNOBDECK_CONFIG_DIR` overrides it).
pub fn host_config_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("KNOBDECK_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/KnobDeck")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join("KnobDeck")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("KnobDeck")
    }
}

/// Extracted Stream Deck plugins: `plugins/<sd uuid>/`.
pub fn plugins_dir() -> PathBuf {
    data_dir().join("plugins")
}
/// Generated KnobDeck plugins are installed like any plugin: `<KnobDeck config>/plugins/<id>/`.
pub fn generated_dir() -> PathBuf {
    host_config_dir().join("plugins")
}
/// KnobDeck's icon cache for a plugin (`<KnobDeck config>/icons/<id>/`).
pub fn host_icons_dir(id: &str) -> PathBuf {
    host_config_dir().join("icons").join(id)
}
pub fn settings_dir() -> PathBuf {
    data_dir().join("settings")
}
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}
/// Ports and token of the running daemon live here (mode 0700 on Unix).
pub fn run_dir() -> PathBuf {
    data_dir().join("run")
}
/// `{"ctl": <port>, "http": <port>}`, written by the daemon while it runs.
pub fn ports_path() -> PathBuf {
    run_dir().join("ports.json")
}
pub fn token_path() -> PathBuf {
    run_dir().join("token")
}

/// The running daemon's ports, if the file exists (it may be stale: connect to find out).
pub fn read_ports() -> Option<(u16, u16)> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(ports_path()).ok()?).ok()?;
    Some((v["ctl"].as_u64()? as u16, v["http"].as_u64()? as u16))
}

/// Open `url` in the default browser.
pub fn open_url(url: &str) {
    use std::process::{Command, Stdio};
    let mut c = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// Run `exe args` fully detached from this process (no console, own process group).
pub fn spawn_detached(exe: &std::path::Path, args: &[&str]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut c = Command::new(exe);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000 | 0x0000_0200); // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
    }
    c.spawn().map(|_| ())
}

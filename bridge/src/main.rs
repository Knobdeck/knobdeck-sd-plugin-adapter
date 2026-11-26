//! `knobdeck-sd-plugin-adapter`: run plugins made for Stream Deck next to KnobDeck.
//!
//! KnobDeck's Adapters screen drives this program through `adapter.json`: `daemon` (run), `stop`,
//! `status`, `open` and `uninstall`. The other commands are for the terminal.

mod daemon;

use bridge_core::{import, paths};
use std::path::PathBuf;

fn usage() {
    println!(
        "knobdeck-sd-plugin-adapter — run Stream Deck plugins next to KnobDeck\n\n\
         USAGE\n  daemon                 run the daemon in this terminal\n  \
         stop                   stop a running daemon\n  \
         status                 exit 0 when the daemon is running\n  \
         open                   open the adapter's page in the browser (starts the daemon if needed)\n  \
         import <file|folder>   import a .streamDeckPlugin or a .sdPlugin folder\n  \
         list                   list imported plugins\n  \
         remove <uuid> [--settings]\n  \
         uninstall [--purge]    stop the daemon and remove every imported plugin (--purge: settings and data too)\n  \
         where                  print the data and log folders\n\n\
         The shim starts the daemon by itself when KnobDeck runs an imported plugin."
    );
}

/// The shim is shipped next to this executable.
fn sibling(name: &str) -> PathBuf {
    std::env::current_exe().unwrap_or_default().with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("daemon") => daemon::run(),
        Some("stop") => {
            println!("{}", if daemon::stop() { "Stopped." } else { "The daemon was not running." });
            Ok(())
        }
        Some("status") => {
            let up = daemon::running();
            println!("{}", if up { "running" } else { "stopped" });
            std::process::exit(if up { 0 } else { 1 });
        }
        Some("open") => {
            let url = daemon::ui_url()?;
            paths::open_url(&url);
            println!("Opened the adapter page in your browser.");
            Ok(())
        }
        Some("import") => {
            let src = args.get(1).ok_or_else(|| anyhow::anyhow!("give a .streamDeckPlugin file or a folder"))?;
            let shim = sibling("bridge-shim");
            anyhow::ensure!(shim.exists(), "bridge-shim not found next to this program ({})", shim.display());
            let r = import::import(&PathBuf::from(src), &shim, &std::env::current_exe()?)?;
            println!("Imported as '{}'.", r.knobdeck_id);
            for n in &r.notes {
                println!("  note: {n}");
            }
            println!("\nInstalled into KnobDeck ({}). Restart KnobDeck to see it in Plugins.", r.dir.display());
            Ok(())
        }
        Some("list") => {
            for m in import::list() {
                println!("{}  {}  v{}  ({} actions)", m.sd_uuid, m.name, m.version, m.actions.len());
            }
            Ok(())
        }
        Some("remove") => {
            let u = args.get(1).ok_or_else(|| anyhow::anyhow!("give the plugin uuid"))?;
            import::remove(u, args.iter().any(|a| a == "--settings"))
        }
        Some("uninstall") => {
            daemon::stop();
            let purge = args.iter().any(|a| a == "--purge");
            for m in import::list() {
                import::remove(&m.sd_uuid, purge)?;
            }
            if purge {
                let _ = std::fs::remove_dir_all(paths::data_dir());
            }
            println!("Removed every imported plugin{}.", if purge { " and all adapter data" } else { " (settings kept)" });
            Ok(())
        }
        Some("where") => {
            println!("data: {}\nlogs: {}", paths::data_dir().display(), paths::logs_dir().display());
            Ok(())
        }
        _ => {
            usage();
            Ok(())
        }
    }
}

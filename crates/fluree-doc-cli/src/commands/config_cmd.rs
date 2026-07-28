//! `fdoc config` — inspect and set the deep reader's settings.

use crate::cli::ConfigCommands;
use crate::config::{self, Value};
use std::path::PathBuf;

pub(crate) fn run(command: ConfigCommands) -> i32 {
    match command {
        ConfigCommands::Path => path(),
        ConfigCommands::Show => show(),
        ConfigCommands::Init { global } => init(global),
        ConfigCommands::Set { key, value } => set(&key, &value),
        ConfigCommands::Gemini {
            credentials,
            project,
            model,
        } => gemini(credentials, project, model),
    }
}

fn path() -> i32 {
    match config::load() {
        Ok(l) => {
            println!("{}", l.path.display());
            if !l.exists {
                eprintln!("note: no file there yet — `fdoc config init` writes one");
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn show() -> i32 {
    let l = match config::load() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    println!(
        "config   {}{}",
        l.path.display(),
        if l.exists { "" } else { "  (not written yet)" }
    );
    let e = &l.config.escalation;
    println!("enabled  {}", e.enabled);
    println!("provider {}", e.provider.as_deref().unwrap_or("(unset)"));
    println!("model    {}", l.config.model());
    println!("workers  {}", e.concurrency);
    println!(
        "columns  {}   (escalate pages whose panels a projection cannot see)",
        e.on_column_doubt
    );
    // The path, not the key. A credential file is a secret and printing its
    // location is the useful half.
    println!(
        "gemini   credentials {}",
        e.gemini
            .credentials
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unset)".into())
    );
    if let Some(p) = &e.gemini.project {
        println!("         project {p}");
    }
    println!();
    match l.config.missing() {
        None => println!("escalation is ready — `fdoc convert <pdf>` will use it"),
        Some(why) => println!("escalation is off: {why}"),
    }
    0
}

fn init(global: bool) -> i32 {
    let dir = if global {
        config::default_path()
    } else {
        match std::env::current_dir() {
            Ok(d) => d.join(config::FDOC_DIR),
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    };
    let path = dir.join(config::CONFIG_FILE);
    if path.exists() {
        eprintln!("error: {} already exists", path.display());
        return 1;
    }
    // Writing through `set` would drop the template's comments, which are the
    // point of `init` — the file is documentation as much as configuration.
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: {}: {e}", dir.display());
        return 1;
    }
    if let Err(e) = std::fs::write(&path, config::TEMPLATE) {
        eprintln!("error: {}: {e}", path.display());
        return 1;
    }
    restrict(&path);
    println!("wrote {}", path.display());
    println!("next: fdoc config gemini --credentials <service-account.json>");
    0
}

fn restrict(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        if let Some(d) = path.parent() {
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn target_path() -> Result<PathBuf, String> {
    let l = config::load()?;
    Ok(l.path)
}

fn set(key: &str, value: &str) -> i32 {
    let path = match target_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if let Err(e) = config::set(&path, key, Value::parse(value)) {
        eprintln!("error: {e}");
        return 1;
    }
    println!("{key} = {value}   ({})", path.display());
    0
}

/// The guided setup: point at a service-account key and the reader is on.
fn gemini(credentials: PathBuf, project: Option<String>, model: Option<String>) -> i32 {
    // Resolve before storing. A relative path in a config file resolves
    // against whatever directory the next run happens to start in, which is
    // a credential that works until someone changes directory.
    let credentials = match credentials.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {}: {e}", credentials.display());
            return 1;
        }
    };
    // Read it now, so a wrong file is caught here rather than mid-batch.
    let text = match std::fs::read_to_string(&credentials) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {}: {e}", credentials.display());
            return 1;
        }
    };
    let key: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {} is not JSON: {e}", credentials.display());
            return 1;
        }
    };
    for field in ["client_email", "private_key", "project_id"] {
        if key.get(field).and_then(serde_json::Value::as_str).is_none() {
            eprintln!(
                "error: {} has no {field} — this does not look like a service-account key",
                credentials.display()
            );
            eprintln!("       create one under IAM > Service Accounts > Keys, with the Vertex AI User role");
            return 1;
        }
    }
    let path = match target_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let write = |k: &str, v: Value| -> Result<(), String> { config::set(&path, k, v) };
    let result = (|| -> Result<(), String> {
        write("escalation.provider", Value::Str("gemini".into()))?;
        write(
            "escalation.gemini.credentials",
            Value::Str(credentials.display().to_string()),
        )?;
        if let Some(p) = &project {
            write("escalation.gemini.project", Value::Str(p.clone()))?;
        }
        if let Some(m) = &model {
            write("escalation.model", Value::Str(m.clone()))?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("error: {e}");
        return 1;
    }
    let project = project.or_else(|| {
        key.get("project_id")
            .and_then(serde_json::Value::as_str)
            .map(String::from)
    });
    println!("escalation configured in {}", path.display());
    println!(
        "  provider gemini, model {}, project {}",
        model.as_deref().unwrap_or(config::DEFAULT_MODEL),
        project.as_deref().unwrap_or("(from the key)")
    );
    println!("\n`fdoc convert <pdf>` now escalates the pages that ask for it.");
    println!("`--no-escalate` turns it off for one run; `fdoc triage <pdf>` shows what would go.");
    0
}

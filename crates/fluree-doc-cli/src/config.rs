//! `.fdoc/config.toml` — where a deployment's escalation settings live.
//!
//! The engine itself takes no configuration: a deterministic parse of a
//! document is the same everywhere, and that is the point of it. What needs
//! configuring is the one thing that reaches outside the process — the deep
//! reader — because it costs money, needs credentials, and must therefore be
//! something a user turns on deliberately rather than something that happens
//! to them.
//!
//! Discovery follows the same rule as the Fluree CLI: walk up from the working
//! directory looking for `.fdoc/`, so a project can pin its own settings, and
//! fall back to a per-user directory so `fdoc config` once is enough. The file
//! names a credential path and may hold an API key, so it is created
//! owner-only.

use std::fs;
use std::path::{Path, PathBuf};

/// The per-project configuration directory.
pub const FDOC_DIR: &str = ".fdoc";
/// The configuration file inside it.
pub const CONFIG_FILE: &str = "config.toml";
/// Overrides the per-user location, for tests and for pinning a deployment.
pub const HOME_ENV: &str = "FDOC_HOME";

/// What `fdoc` knows about running a deep reader.
///
/// Every field is optional because a partial config is a real state: a user
/// who has named a model but not yet a credential should be told what is
/// missing, not handed a parse error.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub escalation: Escalation,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Escalation {
    /// Whether a configured reader runs when no flag says otherwise.
    ///
    /// Defaults to true, because configuring a reader is itself the decision
    /// to use one. `--no-escalate` overrides it per run, and setting this
    /// false makes `--escalate` the opt-in.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Which service reads the crops. Only `gemini` is implemented.
    pub provider: Option<String>,
    /// Model name, passed to the provider unchanged.
    pub model: Option<String>,
    /// Crops read at once. The reader is network-bound, so this is a
    /// politeness and rate-limit setting rather than a CPU one.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Also escalate a page whose columns the page-global projection cannot
    /// see — panels side by side under a full-width heading, where reading
    /// order runs across the panels instead of down them.
    ///
    /// Off by default, and the default is a measurement rather than caution:
    /// over the evaluation corpus this flags 22 documents, gains less in
    /// total than heading doubt alone and makes five worse. On layout-heavy
    /// material — decks, brochures, one-pagers — it marks exactly the pages
    /// that read across their panels. Nothing on the page separates the two
    /// populations, so it is a fact about a corpus and belongs in a corpus's
    /// configuration.
    #[serde(default)]
    pub on_column_doubt: bool,
    #[serde(default)]
    pub gemini: Gemini,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Gemini {
    /// Path to a Google service-account JSON key.
    pub credentials: Option<PathBuf>,
    /// Cloud project. Read from the key file when absent, which is almost
    /// always the right answer.
    pub project: Option<String>,
}

fn yes() -> bool {
    true
}

fn default_concurrency() -> usize {
    6
}

impl Default for Escalation {
    fn default() -> Self {
        Escalation {
            enabled: true,
            provider: None,
            model: None,
            concurrency: default_concurrency(),
            on_column_doubt: false,
            gemini: Gemini::default(),
        }
    }
}

/// The default model, when the config names a provider but not a model.
pub const DEFAULT_MODEL: &str = "gemini-3-flash-preview";

impl Config {
    /// Is there enough here to run a deep reader?
    ///
    /// Deliberately not "is the file present": a config naming a provider with
    /// no credential is configured for nothing, and reporting it as ready
    /// turns a setup mistake into a runtime failure halfway through a batch.
    pub fn reader_is_configured(&self) -> bool {
        self.missing().is_none()
    }

    /// What stands between this config and a working reader, in words a user
    /// can act on. `None` when nothing does.
    pub fn missing(&self) -> Option<String> {
        match self.escalation.provider.as_deref() {
            None => Some("no provider is set".into()),
            Some("gemini") => match &self.escalation.gemini.credentials {
                None => Some("no credentials are set for gemini".into()),
                Some(p) if !p.exists() => Some(format!(
                    "the gemini credentials file is missing: {}",
                    p.display()
                )),
                Some(_) => None,
            },
            Some(other) => Some(format!("provider {other:?} is not one this build knows")),
        }
    }

    pub fn model(&self) -> &str {
        self.escalation.model.as_deref().unwrap_or(DEFAULT_MODEL)
    }
}

/// Where the config was found, and what it says.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub path: PathBuf,
    /// False when no file exists at `path` — the config is the default and
    /// `path` is where writing one would put it.
    pub exists: bool,
    pub config: Config,
}

/// Find and read the configuration.
///
/// Never fails on absence: a missing file is the default config, which
/// escalates nothing. A malformed file *does* fail, because silently ignoring
/// a config a user wrote is worse than stopping.
pub fn load() -> Result<Loaded, String> {
    let path = locate().unwrap_or_else(|| default_path().join(CONFIG_FILE));
    if !path.is_file() {
        return Ok(Loaded {
            path,
            exists: false,
            config: Config::default(),
        });
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Loaded {
        path,
        exists: true,
        config,
    })
}

/// The config file in effect, if one exists: nearest `.fdoc/` walking up from
/// the working directory, else the per-user one.
fn locate() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(FDOC_DIR).join(CONFIG_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    let global = default_path().join(CONFIG_FILE);
    global.is_file().then_some(global)
}

/// The per-user configuration directory: `$FDOC_HOME`, else the platform's
/// own config location.
///
/// Resolved here rather than through a crate. The rule is three lines per
/// platform, and the obvious dependency for it carries an MPL-2.0 transitive
/// — which would put the first copyleft licence into a tree that has none,
/// for less code than this comment.
pub fn default_path() -> PathBuf {
    if let Some(home) = std::env::var_os(HOME_ENV) {
        return PathBuf::from(home);
    }
    let name = "fluree-doc-parse";
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join(name))
        .unwrap_or_else(|| PathBuf::from(FDOC_DIR))
}

/// Set one dotted key, creating the file if it does not exist.
///
/// `toml_edit` rather than a serialize round-trip, so comments and any key
/// this build does not know about survive being edited by it.
pub fn set(path: &Path, key: &str, value: Value) -> Result<(), String> {
    use toml_edit::{DocumentMut, Item};

    let mut doc: DocumentMut = match fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .map_err(|e| format!("{}: {e}", path.display()))?,
        Err(_) => DocumentMut::new(),
    };
    let parts: Vec<&str> = key.split('.').collect();
    let (last, parents) = parts.split_last().ok_or("empty key")?;
    let mut node = doc.as_item_mut();
    for p in parents {
        // Implicit tables so `escalation.gemini.credentials` writes
        // `[escalation.gemini]` rather than an empty `[escalation]` above it.
        node = &mut node[*p];
        if node.is_none() {
            *node = Item::Table(toml_edit::Table::new());
        }
        if let Some(t) = node.as_table_mut() {
            t.set_implicit(true);
        }
    }
    node[*last] = match value {
        Value::Str(s) => toml_edit::value(s),
        Value::Bool(b) => toml_edit::value(b),
        Value::Int(i) => toml_edit::value(i),
    };
    write(path, &doc.to_string())
}

/// A value a config key may take, as the CLI can express it.
pub enum Value {
    Str(String),
    Bool(bool),
    Int(i64),
}

impl Value {
    /// Parse a command-line value, preferring the type the key implies.
    ///
    /// `true`/`false` and bare integers become their own types; everything
    /// else is a string. A path that happens to be `42` is not a real risk,
    /// and a boolean written as a string silently disables a setting.
    pub fn parse(raw: &str) -> Value {
        match raw {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => match raw.parse::<i64>() {
                Ok(i) => Value::Int(i),
                Err(_) => Value::Str(raw.to_string()),
            },
        }
    }
}

/// Write the config, owner-only.
///
/// The file names the path to a private key and may come to hold a token, so
/// it must not be group- or world-readable. `fs::write` honours the umask,
/// which is typically `0o644`, so the mode is set explicitly afterwards.
fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        restrict(dir, 0o700)?;
    }
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    restrict(path, 0o600)
}

fn restrict(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("cannot restrict {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

/// The commented starting point `fdoc config init` writes.
pub const TEMPLATE: &str = r#"# fluree-doc-parse configuration.
#
# Only the deep reader is configured here. Everything else about a parse is
# deterministic and takes no settings.
#
# With no provider set, `fdoc convert` never leaves the deterministic tier and
# never reaches the network. Setting one is what turns escalation on.

[escalation]
# Run the configured reader when no flag says otherwise. `--no-escalate`
# overrides this per run; set it false to make `--escalate` the opt-in.
enabled = true

# The only provider this build implements.
# provider = "gemini"

# Passed to the provider unchanged.
# model = "gemini-3-flash-preview"

# Crops read at once.
# concurrency = 6

# Also escalate pages whose columns a page-global projection cannot see:
# panels side by side under a full-width heading, where reading order runs
# across the panels instead of down them. Off by default -- on a corpus of
# reports it costs more than it gains, and on decks and brochures it marks
# exactly the pages that need it. `fdoc triage <file>` reports which pages
# this would add, without sending anything.
# on_column_doubt = false

[escalation.gemini]
# Path to a Google service-account JSON key with the Vertex AI User role.
# `fdoc config gemini --credentials <path>` writes this for you.
# credentials = "/path/to/service-account.json"

# Read from the key file when absent, which is usually right.
# project = "my-gcp-project"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_per_user_path_is_under_a_home_and_named_for_the_tool() {
        // $FDOC_HOME wins outright; without it the platform's own location is
        // used, which must at least be absolute and ours.
        let p = default_path();
        assert!(
            p.ends_with("fluree-doc-parse") || p.ends_with(FDOC_DIR),
            "{p:?}"
        );
    }

    #[test]
    fn an_absent_config_escalates_nothing() {
        let c = Config::default();
        assert!(!c.reader_is_configured());
        assert_eq!(c.missing().as_deref(), Some("no provider is set"));
    }

    #[test]
    fn a_provider_without_a_credential_is_not_configured() {
        let c: Config = toml::from_str("[escalation]\nprovider = \"gemini\"\n").unwrap();
        assert!(!c.reader_is_configured());
        assert!(c.missing().unwrap().contains("credentials"));
    }

    #[test]
    fn a_credential_that_is_not_there_is_reported_by_path() {
        let c: Config = toml::from_str(
            "[escalation]\nprovider = \"gemini\"\n[escalation.gemini]\ncredentials = \"/nope/key.json\"\n",
        )
        .unwrap();
        assert!(c.missing().unwrap().contains("/nope/key.json"));
    }

    #[test]
    fn an_unknown_provider_says_so_rather_than_trying() {
        let c: Config = toml::from_str("[escalation]\nprovider = \"parrot\"\n").unwrap();
        assert!(c.missing().unwrap().contains("parrot"));
    }

    #[test]
    fn enabled_defaults_on_so_configuring_a_reader_uses_it() {
        let c: Config = toml::from_str("[escalation]\nprovider = \"gemini\"\n").unwrap();
        assert!(c.escalation.enabled);
        assert_eq!(c.escalation.concurrency, 6);
        assert_eq!(c.model(), DEFAULT_MODEL);
    }

    #[test]
    fn column_doubt_escalation_is_off_until_a_corpus_asks_for_it() {
        let c: Config = toml::from_str("[escalation]\nprovider = \"gemini\"\n").unwrap();
        assert!(!c.escalation.on_column_doubt);
        let c: Config = toml::from_str("[escalation]\non_column_doubt = true\n").unwrap();
        assert!(c.escalation.on_column_doubt);
    }

    #[test]
    fn setting_a_nested_key_writes_one_table() {
        let tmp = std::env::temp_dir().join(format!("fdoc-cfg-{}", std::process::id()));
        let path = tmp.join(CONFIG_FILE);
        let _ = fs::remove_dir_all(&tmp);
        set(&path, "escalation.provider", Value::Str("gemini".into())).unwrap();
        set(
            &path,
            "escalation.gemini.credentials",
            Value::Str("/k.json".into()),
        )
        .unwrap();
        set(&path, "escalation.enabled", Value::Bool(false)).unwrap();
        let c: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.escalation.provider.as_deref(), Some("gemini"));
        assert_eq!(
            c.escalation.gemini.credentials.as_deref(),
            Some(Path::new("/k.json"))
        );
        assert!(!c.escalation.enabled);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn editing_keeps_comments_and_unknown_keys() {
        let tmp = std::env::temp_dir().join(format!("fdoc-cfg-keep-{}", std::process::id()));
        let path = tmp.join(CONFIG_FILE);
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(&path, "# keep me\n[escalation]\nfuture_key = 1\n").unwrap();
        set(&path, "escalation.provider", Value::Str("gemini".into())).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains("future_key"), "{text}");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_template_parses_and_configures_nothing() {
        let c: Config = toml::from_str(TEMPLATE).unwrap();
        assert!(!c.reader_is_configured());
    }
}

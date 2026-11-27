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

use std::collections::BTreeMap;
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
    #[serde(default)]
    pub ocr: Ocr,
    #[serde(default)]
    pub limits: Limits,
}

/// What one document may take, where the built-in defaults do not suit
/// (`docs/reference/limits.md`). The `--max-*` flags and the `FDOC_MAX_*`
/// variables override each.
///
/// A key the table does not know, and a value of the wrong type (`"64"`,
/// `-1`, `2.5`), are set aside, to be warned about ([`Limits::warnings`]),
/// not refused: refused, either made the whole file unreadable, and with it
/// the escalation settings beside it, for a typo in a limit. The other
/// tables accept keys a later build may add; this one says so, since a
/// misspelt limit silently not applied is a document read within a bound
/// its user thinks they raised.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    /// Memory one document may take, in MiB.
    pub max_memory: Option<u64>,
    /// Steps of work one document may take.
    pub max_work: Option<u64>,
    /// Pages a paged document is read to.
    pub max_pages: Option<usize>,
    /// Pixels a page is rendered with.
    pub max_render_pixels: Option<u64>,
    /// The keys of the table this build does not know.
    other: BTreeMap<String, toml::Value>,
    /// The known keys whose value is no whole number the limit holds, with
    /// the value as written.
    invalid: BTreeMap<String, String>,
}

/// Each known key read on its own, so a value of the wrong type loses that
/// limit alone.
impl<'de> serde::Deserialize<'de> for Limits {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut table = BTreeMap::<String, toml::Value>::deserialize(d)?;
        let mut invalid = BTreeMap::new();
        let mut take = |key: &str| -> Option<i64> {
            let v = table.remove(key)?;
            let n = v.as_integer().filter(|n| *n >= 0);
            if n.is_none() {
                invalid.insert(key.to_string(), v.to_string());
            }
            n
        };
        let max_memory = take("max_memory").map(|n| n as u64);
        let max_work = take("max_work").map(|n| n as u64);
        let max_pages = take("max_pages").map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        let max_render_pixels = take("max_render_pixels").map(|n| n as u64);
        Ok(Self {
            max_memory,
            max_work,
            max_pages,
            max_render_pixels,
            other: table,
            invalid,
        })
    }
}

impl Limits {
    /// The keys of the table this build does not know, in order: each is
    /// ignored, and worth a warning.
    pub fn unknown(&self) -> impl Iterator<Item = &str> {
        self.other.keys().map(String::as_str)
    }

    /// What is wrong with the table, a line each, to be warned about: a key
    /// it does not know, a value that is no whole number of zero or more.
    /// Each is ignored, the rest of the table taken.
    pub fn warnings(&self) -> Vec<String> {
        let unknown = self
            .unknown()
            .map(|key| format!("[limits] has no key {key:?}; it is ignored"));
        let invalid = self
            .invalid
            .iter()
            .map(|(key, v)| format!("[limits] {key} = {v} is not a whole number; it is ignored"));
        unknown.chain(invalid).collect()
    }

    /// The limits in effect: each flag in `flags`, else its variable read
    /// by `env`, else this, else the default. A variable read that is not a
    /// number, or a limit of zero, is an error naming where it was set; a
    /// variable a flag overrides is not read, and one set empty is unset.
    pub fn resolve(
        &self,
        flags: &crate::cli::LimitArgs,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<fluree_doc_model::budget::Limits, String> {
        /// One limit: its flag, its variable and its config key by name, the
        /// value each holds, and the default.
        fn pick<T: std::str::FromStr + PartialEq + Default + Copy>(
            (flag, var, key): (&str, &str, &str),
            given: Option<T>,
            env: &impl Fn(&str) -> Option<String>,
            config: Option<T>,
            default: T,
        ) -> Result<T, String> {
            let (v, from) = if let Some(v) = given {
                (v, flag.to_string())
            } else if let Some(v) = env(var).filter(|v| !v.trim().is_empty()) {
                // Set but empty is unset, as a shell's `VAR=` often means.
                let n = v
                    .trim()
                    .parse::<T>()
                    .map_err(|_| format!("{var}: not a number: {v:?}"))?;
                (n, var.to_string())
            } else if let Some(v) = config {
                (v, format!("[limits] {key}"))
            } else {
                return Ok(default);
            };
            if v == T::default() {
                return Err(format!("{from}: a limit of zero reads nothing"));
            }
            Ok(v)
        }
        let default = fluree_doc_model::budget::Limits::default();
        let mut l = default;
        l.memory = pick(
            ("--max-memory", "FDOC_MAX_MEMORY", "max_memory"),
            flags.max_memory,
            &env,
            self.max_memory,
            default.memory >> 20,
        )?
        .saturating_mul(1 << 20);
        l.work = pick(
            ("--max-work", "FDOC_MAX_WORK", "max_work"),
            flags.max_work,
            &env,
            self.max_work,
            default.work,
        )?;
        l.pages = pick(
            ("--max-pages", "FDOC_MAX_PAGES", "max_pages"),
            flags.max_pages,
            &env,
            self.max_pages,
            default.pages,
        )?;
        l.render_pixels = pick(
            (
                "--max-render-pixels",
                "FDOC_MAX_RENDER_PIXELS",
                "max_render_pixels",
            ),
            flags.max_render_pixels,
            &env,
            self.max_render_pixels,
            default.render_pixels,
        )?;
        Ok(l)
    }
}

/// The built-in OCR engine. Unlike a model reader it reaches nothing outside
/// the machine: it is configured here only so a deployment names its assets
/// once. `--ocr` and `--models` override it per run.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Ocr {
    /// Which engine reads routed pixels. Only `oar-ocr` is built in.
    pub engine: Option<String>,
    /// The assets directory: the models and ONNX Runtime.
    pub models: Option<PathBuf>,
    /// ONNX Runtime threads per crop read (default 1).
    pub threads: Option<usize>,
    /// Most crops the engine reads in one document (default
    /// [`DEFAULT_OCR_MAX_CROPS`], `0` for no limit). Past it, pages read
    /// whole come first, then routed regions, then small ones, and a note
    /// says how many were left.
    ///
    /// Its own limit rather than `escalation.max_crops`: a scanned page is
    /// one crop, so a model's budget of 200 would leave page 201 of a long
    /// scan unread, and a crop costs a fraction of a second of this
    /// machine's time rather than a paid call.
    pub max_crops: Option<usize>,
    /// Most small regions the engine reads on one page, the largest first
    /// (default `0`, every one: [`fluree_doc_pdf::ocr::DEFAULT_SMALL_REGIONS_PER_PAGE`]).
    /// Routed regions and pages read whole are always read. Set it where a
    /// page's time must be bounded: a drawing's callouts can make a hundred
    /// crops of one page.
    pub max_small_regions_per_page: Option<usize>,
    /// Most megapixels the engine's detector reads in one pass (default
    /// 12.5: a page scanned at 300 dpi, or a 12 Mpx photo). A larger image
    /// is read in overlapping strips, or, too wide for them, shrunk. What
    /// bounds a reading's memory, about 230 bytes a pixel.
    pub max_megapixels: Option<f64>,
    /// The memory, in MiB, the passes the engine reads at once may take
    /// (default a third of the memory free, at most 8192), each counted at
    /// about 230 bytes a pixel: a pass that does not fit waits for others
    /// to finish.
    pub max_memory: Option<u64>,
}

/// Default for `ocr.max_crops`: enough for a thousand-page scan read whole.
/// A form's checkbox and rule masks, which once made a crop each, never
/// count toward it: an image too small to hold a line of text is not read
/// (`route::MIN_TEXT_LINE_PX`).
pub const DEFAULT_OCR_MAX_CROPS: usize = 1000;

impl Ocr {
    /// `max_crops`, or its default.
    pub fn max_crops(&self) -> usize {
        self.max_crops.unwrap_or(DEFAULT_OCR_MAX_CROPS)
    }

    /// `max_small_regions_per_page`, or its default.
    pub fn max_small_regions_per_page(&self) -> usize {
        self.max_small_regions_per_page
            .unwrap_or(fluree_doc_pdf::ocr::DEFAULT_SMALL_REGIONS_PER_PAGE)
    }
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
    /// Most crops one document may send to the reader. Past it, the most
    /// valuable crops are read and the rest keep their deterministic
    /// reading, with a note saying how many. Zero means no limit.
    ///
    /// The deterministic pass is complete on its own; every crop only
    /// improves on it. So the limit truncates rather than fails, and the
    /// default is sized for a report, not a thousand-table chart — which
    /// is the document that needs it.
    #[serde(default = "default_max_crops")]
    pub max_crops: usize,
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

fn default_max_crops() -> usize {
    200
}

impl Default for Escalation {
    fn default() -> Self {
        Escalation {
            enabled: true,
            provider: None,
            model: None,
            concurrency: default_concurrency(),
            on_column_doubt: false,
            max_crops: default_max_crops(),
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
        Value::Float(f) => toml_edit::value(f),
    };
    write(path, &doc.to_string())
}

/// A value a config key may take, as the CLI can express it.
pub enum Value {
    Str(String),
    Bool(bool),
    Int(i64),
    Float(f64),
}

impl Value {
    /// Parse a command-line value, preferring the type the key implies.
    ///
    /// `true`/`false`, bare integers and decimals (`12.5`) become their own
    /// types; everything else is a string. A path that happens to be `42` is not a real risk,
    /// and a boolean written as a string silently disables a setting.
    pub fn parse(raw: &str) -> Value {
        match raw {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => match (raw.parse::<i64>(), raw.parse::<f64>()) {
                (Ok(i), _) => Value::Int(i),
                (_, Ok(f)) if f.is_finite() && raw.contains('.') => Value::Float(f),
                _ => Value::Str(raw.to_string()),
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

# Most crops one document may send. Past it the most valuable are read and
# the rest keep their deterministic reading, with a note. 0 means no limit.
# `fdoc triage <file>` prints the count a document would send.
# max_crops = 200

[escalation.gemini]
# Path to a Google service-account JSON key with the Vertex AI User role.
# `fdoc config gemini --credentials <path>` writes this for you.
# credentials = "/path/to/service-account.json"

# Read from the key file when absent, which is usually right.
# project = "my-gcp-project"

[limits]
# What one document may take; past it the document is refused, or a paged
# one read to its last page within the limit. The --max-* flags and the
# FDOC_MAX_* variables override these. See docs/reference/limits.md.
# max_memory = 2560          # MiB
# max_work = 268435456       # steps
# max_pages = 10000
# max_render_pixels = 64000000
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
        assert_eq!(c.limits, Limits::default());
    }

    #[test]
    fn a_limit_is_a_flag_over_its_variable_over_the_config() {
        use fluree_doc_model::budget::Limits as Bounds;
        let c: Config = toml::from_str(
            "[limits]\nmax_memory = 100\nmax_pages = 7\nmax_work = 9\nmax_render_pixels = 11\n",
        )
        .unwrap();
        let none = |_: &str| None;
        let got = c
            .limits
            .resolve(&crate::cli::LimitArgs::default(), none)
            .unwrap();
        assert_eq!(
            (got.memory, got.work, got.pages, got.render_pixels),
            (100 << 20, 9, 7, 11)
        );
        let env = |v: &str| (v == "FDOC_MAX_PAGES").then(|| " 20 ".to_string());
        assert_eq!(
            c.limits.resolve(&Default::default(), env).unwrap().pages,
            20
        );
        let flags = crate::cli::LimitArgs {
            max_pages: Some(30),
            max_memory: Some(1),
            ..Default::default()
        };
        let got = c.limits.resolve(&flags, env).unwrap();
        assert_eq!((got.pages, got.memory, got.work), (30, 1 << 20, 9));
        // Nothing set: the defaults.
        assert_eq!(
            Limits::default()
                .resolve(&Default::default(), none)
                .unwrap(),
            Bounds::default()
        );
        // A variable that is not a number, or a zero, is said.
        let bad = |v: &str| (v == "FDOC_MAX_WORK").then(|| "lots".to_string());
        let e = Limits::default()
            .resolve(&Default::default(), bad)
            .unwrap_err();
        assert!(e.contains("FDOC_MAX_WORK"), "{e}");
        let zero = crate::cli::LimitArgs {
            max_render_pixels: Some(0),
            ..Default::default()
        };
        let e = Limits::default().resolve(&zero, none).unwrap_err();
        assert!(e.starts_with("--max-render-pixels: "), "{e}");
        // A zero is said where it was set, the variable's and the config's.
        let zero = |v: &str| (v == "FDOC_MAX_PAGES").then(|| "0".to_string());
        let e = Limits::default()
            .resolve(&Default::default(), zero)
            .unwrap_err();
        assert!(e.starts_with("FDOC_MAX_PAGES: "), "{e}");
        let c: Config = toml::from_str("[limits]\nmax_memory = 0\n").unwrap();
        let e = c.limits.resolve(&Default::default(), none).unwrap_err();
        assert!(e.starts_with("[limits] max_memory: "), "{e}");
        // A flag set, its variable is not read, nor refused.
        let bad = |v: &str| (v == "FDOC_MAX_MEMORY").then(|| "lots".to_string());
        assert_eq!(c.limits.resolve(&flags, bad).unwrap().memory, 1 << 20);
        // A key the table does not know is set aside, to be said, and the
        // rest of the file read: a typo in a limit loses no other setting.
        let c: Config = toml::from_str(
            "[escalation]\nprovider = \"gemini\"\n[limits]\nmax_pagez = 3\nmax_pages = 4\n",
        )
        .unwrap();
        assert_eq!(c.limits.unknown().collect::<Vec<_>>(), ["max_pagez"]);
        assert_eq!(c.limits.max_pages, Some(4));
        assert_eq!(c.escalation.provider.as_deref(), Some("gemini"));
        assert_eq!(
            c.limits.warnings(),
            ["[limits] has no key \"max_pagez\"; it is ignored"]
        );
        // A variable set empty is unset: the config's value holds.
        let empty = |v: &str| (v == "FDOC_MAX_PAGES").then(|| " ".to_string());
        assert_eq!(
            c.limits.resolve(&Default::default(), empty).unwrap().pages,
            4
        );
    }

    #[test]
    fn a_limit_of_the_wrong_type_loses_that_limit_alone() {
        let c: Config = toml::from_str(
            "[escalation]\nprovider = \"gemini\"\n[limits]\nmax_memory = \"64\"\n\
             max_work = -1\nmax_pages = 2.5\nmax_render_pixels = 9\n",
        )
        .unwrap();
        assert_eq!(c.escalation.provider.as_deref(), Some("gemini"));
        let l = &c.limits;
        assert_eq!(
            (l.max_memory, l.max_work, l.max_pages, l.max_render_pixels),
            (None, None, None, Some(9))
        );
        assert_eq!(
            l.warnings(),
            [
                "[limits] max_memory = \"64\" is not a whole number; it is ignored",
                "[limits] max_pages = 2.5 is not a whole number; it is ignored",
                "[limits] max_work = -1 is not a whole number; it is ignored",
            ]
        );
        assert_eq!(
            l.resolve(&Default::default(), |_| None)
                .unwrap()
                .render_pixels,
            9
        );
        // A table that holds them right warns about nothing.
        let c: Config = toml::from_str("[limits]\nmax_pages = 4\n").unwrap();
        assert!(c.limits.warnings().is_empty());
    }
}

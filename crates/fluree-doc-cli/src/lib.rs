//! `fdoc` — command-line surface for the fluree-doc-parse extraction core.
//!
//! Structured as a library plus a thin binary so the command set can be
//! mounted as a subcommand of a larger Fluree CLI: embed [`cli::Commands`]
//! (or the whole [`cli::Cli`]) and dispatch through [`commands::run`].

pub mod cli;
pub mod commands;
pub mod config;
pub(crate) mod escalate;

/// The reader `convert` sends a file named `name` holding `data` to
/// (`"Pdf"`, `"Xlsb"`, `"Unsupported"`, …), and why it refuses it where it
/// does: the content detection, for the fuzz targets under `fuzz/`. Not part
/// of the command surface.
#[doc(hidden)]
pub fn detect(name: &str, data: &[u8]) -> (String, Option<String>) {
    let path = std::path::Path::new(name);
    let kind = commands::common::input_kind(path, data);
    (
        format!("{kind:?}"),
        commands::common::unsupported(Some(path), data),
    )
}

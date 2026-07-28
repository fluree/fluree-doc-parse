//! `fdoc` — command-line surface for the fluree-doc-parse extraction core.
//!
//! Structured as a library plus a thin binary so the command set can be
//! mounted as a subcommand of a larger Fluree CLI: embed [`cli::Commands`]
//! (or the whole [`cli::Cli`]) and dispatch through [`commands::run`].

pub mod cli;
pub mod commands;
pub mod config;
pub(crate) mod escalate;

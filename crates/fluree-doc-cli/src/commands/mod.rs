pub mod common;
pub mod completions;
pub mod config_cmd;
pub mod convert;
pub mod dev;
pub mod forms;
pub mod render;
pub mod triage;

use crate::cli::{Cli, Commands, ConvertArgs, DevCommands, Format};

/// Dispatch a parsed CLI to its command, returning the process exit code.
pub fn run(cli: Cli) -> i32 {
    match cli.command {
        Commands::Convert(args) => convert::run(&args, cli.verbose, cli.quiet),
        Commands::Forms { pdf } => forms::run(&pdf),
        Commands::Triage { path } => triage::run(&path),
        Commands::Config { command } => config_cmd::run(command),
        Commands::Render {
            path,
            out,
            scale,
            pages,
        } => render::run(&path, &out, scale, pages.as_deref()),
        Commands::Completions { shell } => {
            completions::run(shell);
            0
        }
        Commands::Dev { command } => {
            run_dev(command);
            0
        }
        // Hidden single-file compatibility forms of `convert`.
        Commands::Md { pdf } => convert::run(&compat(pdf, Format::Md), false, true),
        Commands::Json { pdf } => convert::run(&compat(pdf, Format::Json), false, true),
        Commands::Xhtml { pdf } => convert::run(&compat(pdf, Format::Xhtml), false, true),
    }
}

fn compat(pdf: std::path::PathBuf, format: Format) -> ConvertArgs {
    ConvertArgs {
        inputs: vec![pdf],
        format,
        output: None,
        out_dir: None,
        pages: None,
        jobs: 1,
        layout_boxes: None,
        tier_results: None,
        structure_results: None,
        emit_anchors: false,
        base_iri: None,
        doc_iri: None,
        escalate: false,
        // The compatibility forms never escalate. Benchmark adapters shell
        // these, and a score has to be reproducible offline by whoever reads
        // it — a configured key on the machine that ran it must not be able
        // to change the number.
        no_escalate: true,
    }
}

fn run_dev(cmd: DevCommands) {
    match cmd {
        DevCommands::Probe { dir } => dev::probe(&dir),
        DevCommands::Find { pdf, text } => dev::find(&pdf, &text),
        DevCommands::Lines { pdf, page } => dev::lines(&pdf, page),
        DevCommands::Gaps { path } => dev::gaps(&path),
        DevCommands::Pair { pdf, text } => dev::pair(&pdf, &text),
        DevCommands::Furniture { pdf } => dev::furn(&pdf),
        DevCommands::Leading { path } => dev::leading(&path),
        DevCommands::Blocks { pdf, page } => dev::blocks(&pdf, page),
        DevCommands::Outline { pdf } => dev::outline_cmd(&pdf),
        DevCommands::Links { pdf } => dev::links(&pdf),
        DevCommands::Headings { pdf } => dev::headings(&pdf),
        DevCommands::Rules { pdf, page } => dev::rules(&pdf, page),
        DevCommands::Tables { pdf, page } => dev::tables(&pdf, page),
        DevCommands::Figures { pdf, page } => dev::figures(&pdf, page),
        DevCommands::Fidelity { pdf } => dev::fidelity(&pdf),
        DevCommands::Columns { pdf, page } => dev::columns(&pdf, page),
        DevCommands::Aligned { pdf } => dev::aligned_diag(&pdf),
        DevCommands::Weights { pdf } => dev::weights(&pdf),
        DevCommands::Glyphs { pdf, page, y0, y1 } => dev::glyphs(&pdf, page, y0, y1),
        DevCommands::RenderPages { path, out } => dev::render_pages(&path, &out),
        DevCommands::Timings { path, warmup, runs } => dev::timings(&path, warmup, runs),
        DevCommands::RenderRouted { path, out } => dev::render_routed(&path, &out),
        DevCommands::RenderCrops {
            manifest,
            corpus,
            out,
        } => dev::render_crops(&manifest, &corpus, &out),
    }
}

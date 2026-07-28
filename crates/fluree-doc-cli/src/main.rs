use clap::Parser;
use fluree_doc_cli::cli::Cli;
use fluree_doc_cli::commands;

fn main() {
    let cli = Cli::parse();
    std::process::exit(commands::run(cli));
}

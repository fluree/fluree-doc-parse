//! `fdoc completions` — shell completion scripts via clap_complete.

use clap::CommandFactory;

pub fn run(shell: clap_complete::Shell) {
    let mut cmd = crate::cli::Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
}

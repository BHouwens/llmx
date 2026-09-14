//! llmx — pick a model, get a working agent.
//!
//! llmx performs no inference. It resolves a declarative model profile into a
//! healthy OpenAI-compatible endpoint, generates matching codex-local
//! configuration, launches codex-local in the current directory, and cleans
//! up whatever it started when codex-local exits.

mod app;
mod backend;
mod codex;
mod config;
mod endpoint;
mod logging;
mod paths;
mod process;
mod system;
#[cfg(test)]
mod testutil;
mod tui;

use anyhow::Result;
use clap::Parser;
use clap::Subcommand;

#[derive(Parser, Debug)]
#[command(
    name = "llmx",
    version,
    about = "Launch codex-local against a local or remote model, without the setup",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Launch a profile by id, skipping the picker.
    Run {
        /// Profile id from your models.toml.
        id: String,
    },

    /// List configured model profiles.
    List,

    /// Probe every configured endpoint. Starts nothing.
    Status,

    /// Check the local setup: config, executables, ssh aliases, writable paths.
    Doctor,

    /// Show what `llmx run <id>` would do, without doing it.
    Explain {
        /// Profile id from your models.toml.
        id: String,
    },

    /// Show session logs.
    Logs {
        /// List log files instead of printing the newest one.
        #[arg(long)]
        list: bool,
        /// Number of trailing lines to print.
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // A current-thread runtime is enough: llmx waits on I/O, it does not
    // compute. Keeping one thread matters because the machine may already be
    // giving almost all its memory to a model.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let code = runtime.block_on(run(cli))?;

    // Drop the runtime before exiting so reader tasks stop cleanly.
    drop(runtime);
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        None => app::App::load()?.run_tui().await,
        Some(Command::Run { id }) => app::App::load()?.run_profile(&id).await,
        Some(Command::List) => {
            app::App::load()?.list();
            Ok(0)
        }
        Some(Command::Status) => {
            app::App::load()?.status().await?;
            Ok(0)
        }
        Some(Command::Doctor) => app::App::load()?.doctor().await,
        Some(Command::Explain { id }) => {
            app::App::load()?.explain(&id)?;
            Ok(0)
        }
        Some(Command::Logs { list, lines }) => {
            app::App::load()?.logs(list, lines)?;
            Ok(0)
        }
    }
}

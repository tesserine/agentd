use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use agentd::config::Config;
use agentd::{
    ManualRunRequest, RunnerSessionExecutor, configure_tracing, request_manual_run,
    run_daemon_until_shutdown,
};
use clap::{Parser, Subcommand};
use signal_hook::consts::signal::{SIGINT, SIGTERM};

const DEFAULT_CONFIG_PATH: &str = "/etc/agentd/agentd.toml";

#[derive(Parser, Debug)]
#[command(name = "agentd")]
struct Cli {
    #[arg(long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start the foreground daemon.
    Daemon,
    /// Trigger a manual session through the running daemon.
    Run {
        agent: String,
        repo: String,
        #[arg(long)]
        work_unit: Option<String>,
    },
}

fn main() -> ExitCode {
    if let Err(error) = configure_tracing() {
        eprintln!("failed to initialize tracing: {error}");
        return ExitCode::FAILURE;
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;

    match cli.command {
        None | Some(Command::Daemon) => run_daemon(config),
        Some(Command::Run {
            agent,
            repo,
            work_unit,
        }) => run_client(config, agent, repo, work_unit),
    }
}

fn run_daemon(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, shutdown.clone())?;
    signal_hook::flag::register(SIGTERM, shutdown.clone())?;

    let executor = RunnerSessionExecutor;
    run_daemon_until_shutdown(config, &executor, shutdown.as_ref())?;
    Ok(())
}

fn run_client(
    config: Config,
    agent: String,
    repo: String,
    work_unit: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let outcome = request_manual_run(
        &config,
        &ManualRunRequest {
            agent,
            repo_url: repo,
            work_unit,
        },
    )?;

    match outcome {
        agentd_runner::SessionOutcome::Succeeded => println!("session succeeded"),
        agentd_runner::SessionOutcome::Failed { exit_code } => {
            println!("session failed (exit code {exit_code})")
        }
        agentd_runner::SessionOutcome::TimedOut => println!("session timed out"),
    }

    Ok(())
}

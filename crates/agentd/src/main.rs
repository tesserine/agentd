use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::{error::Error, fmt};

use agentd::config::{Config, DaemonConfig};
use agentd::{
    RunRequest, RunnerSessionExecutor, configure_tracing, request_run, run_daemon_until_shutdown,
};
use clap::{Parser, Subcommand};
use signal_hook::consts::signal::{SIGINT, SIGTERM};

const DEFAULT_CONFIG_PATH: &str = "/etc/agentd/agentd.toml";

#[derive(Debug)]
enum RunCommandError {
    Failed { exit_code: i32 },
    TimedOut,
}

impl fmt::Display for RunCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed { exit_code } => write!(f, "session failed (exit code {exit_code})"),
            Self::TimedOut => write!(f, "session timed out"),
        }
    }
}

impl Error for RunCommandError {}

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

    match cli.command {
        None | Some(Command::Daemon) => run_daemon(Config::load(&cli.config)?),
        Some(Command::Run {
            agent,
            repo,
            work_unit,
        }) => run_client(DaemonConfig::load(&cli.config)?, agent, repo, work_unit),
    }
}

fn run_daemon(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    register_termination_handlers(shutdown.clone())?;

    let executor = RunnerSessionExecutor;
    run_daemon_until_shutdown(config, executor, shutdown.as_ref())?;
    Ok(())
}

fn register_termination_handlers(shutdown: Arc<AtomicBool>) -> Result<(), std::io::Error> {
    for signal in [SIGINT, SIGTERM] {
        signal_hook::flag::register_conditional_shutdown(signal, 1, shutdown.clone())?;
        signal_hook::flag::register(signal, shutdown.clone())?;
    }

    Ok(())
}

fn run_client(
    config: DaemonConfig,
    agent: String,
    repo: String,
    work_unit: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let outcome = request_run(
        &config,
        &RunRequest {
            agent,
            repo_url: repo,
            work_unit,
        },
    )?;

    match outcome {
        agentd_runner::SessionOutcome::Succeeded => {
            println!("session succeeded");
            Ok(())
        }
        agentd_runner::SessionOutcome::Failed { exit_code } => {
            Err(Box::new(RunCommandError::Failed { exit_code }))
        }
        agentd_runner::SessionOutcome::TimedOut => Err(Box::new(RunCommandError::TimedOut)),
    }
}

#[cfg(test)]
mod tests {
    use super::register_termination_handlers;
    use std::io::Error;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn second_sigterm_exits_immediately_after_first_starts_shutdown() {
        unsafe {
            libc::alarm(10);
            match libc::fork() {
                -1 => panic!("fork failed: {}", Error::last_os_error()),
                0 => {
                    let shutdown = Arc::new(AtomicBool::new(false));
                    register_termination_handlers(Arc::clone(&shutdown))
                        .expect("termination handlers should register");

                    while !shutdown.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(10));
                    }

                    loop {
                        thread::sleep(Duration::from_secs(1));
                    }
                }
                pid => {
                    thread::sleep(Duration::from_millis(250));
                    assert_eq!(
                        0,
                        libc::kill(pid, libc::SIGTERM),
                        "first SIGTERM should send"
                    );
                    thread::sleep(Duration::from_millis(100));

                    let terminated = libc::waitpid(pid, ptr::null_mut(), libc::WNOHANG);
                    assert_eq!(
                        0, terminated,
                        "process should still be draining after the first SIGTERM"
                    );

                    assert_eq!(
                        0,
                        libc::kill(pid, libc::SIGTERM),
                        "second SIGTERM should send"
                    );
                    let terminated = libc::waitpid(pid, ptr::null_mut(), 0);
                    assert_eq!(pid, terminated, "process should exit on the second SIGTERM");
                }
            }
        }
    }
}

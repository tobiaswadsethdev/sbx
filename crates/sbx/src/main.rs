//! `sbx` - run several coding agents in parallel, each in its own sandbox.

mod doctor;

use clap::{Parser, Subcommand};
use openshell_client::CliClient;

#[derive(Parser)]
#[command(
    name = "sbx",
    version,
    about = "Parallel coding agents in OpenShell sandboxes"
)]
struct Cli {
    /// Gateway name to operate on (defaults to the active one).
    #[arg(long, global = true)]
    gateway: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that everything sbx depends on is present and working.
    Doctor,
}

fn main() {
    let cli = Cli::parse();

    let mut client = CliClient::new();
    if let Some(g) = cli.gateway {
        client = client.with_gateway(g);
    }

    let code = match cli.command {
        Command::Doctor => {
            let checks = doctor::run(&client);
            doctor::report(&checks)
        }
    };

    std::process::exit(code);
}

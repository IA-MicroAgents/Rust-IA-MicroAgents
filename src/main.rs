use ai_microagents::{app, cli::Cli, telemetry::logging};
use clap::{CommandFactory, Parser};

#[tokio::main]
async fn main() {
    logging::init();

    let cli = Cli::parse();
    if let Err(err) = app::dispatch(cli).await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }

    let _ = Cli::command();
}

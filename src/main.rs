use clap::{CommandFactory, Parser};
use ferrum::{app, cli::Cli, telemetry::logging};

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

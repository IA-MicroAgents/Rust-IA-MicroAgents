use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ai-microagents",
    version,
    about = "Deterministic Telegram-first AI orchestrator"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Init,
    Run,
    Dashboard,
    Doctor,
    Replay(ReplayArgs),
    Chat(ChatArgs),
    ExportTrace(ExportTraceArgs),
    Team {
        #[command(subcommand)]
        command: TeamCommands,
    },
    Identity {
        #[command(subcommand)]
        command: IdentityCommands,
    },
    Skills {
        #[command(subcommand)]
        command: SkillCommands,
    },
}

#[derive(Debug, Args)]
pub struct ReplayArgs {
    pub event_id: String,
}

#[derive(Debug, Args)]
pub struct ChatArgs {
    #[arg(long)]
    pub stdin: bool,
}

#[derive(Debug, Args)]
pub struct ExportTraceArgs {
    pub conversation_id: i64,
}

#[derive(Debug, Subcommand)]
pub enum IdentityCommands {
    Lint,
}

#[derive(Debug, Subcommand)]
pub enum SkillCommands {
    Lint,
}

#[derive(Debug, Subcommand)]
pub enum TeamCommands {
    Status,
    Simulate,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

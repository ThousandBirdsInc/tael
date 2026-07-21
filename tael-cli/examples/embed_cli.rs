//! Mount the entire tael CLI — every subcommand, including the `live` TUI —
//! inside a host application's own clap command tree.
//!
//! ```sh
//! cargo run -p tael-cli --example embed_cli -- tael services
//! cargo run -p tael-cli --example embed_cli -- tael live
//! cargo run -p tael-cli --example embed_cli -- deploy
//! ```

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "myapp", about = "A host application that embeds tael")]
struct MyApp {
    #[command(subcommand)]
    command: MyCommand,

    #[command(flatten)]
    tael_opts: tael_cli::GlobalOpts,
}

#[derive(Subcommand)]
enum MyCommand {
    /// A command of the host application itself
    Deploy,
    /// Every tael subcommand, mounted under `myapp tael <cmd>`
    #[command(subcommand)]
    Tael(tael_cli::Commands),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = MyApp::parse();
    match app.command {
        MyCommand::Deploy => {
            println!("deploying the host application…");
            Ok(())
        }
        MyCommand::Tael(cmd) => tael_cli::run_command(cmd, &app.tael_opts).await,
    }
}

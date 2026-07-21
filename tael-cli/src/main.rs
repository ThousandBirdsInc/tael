use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tael_cli::Cli::parse().run().await
}

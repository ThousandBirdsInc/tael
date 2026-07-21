//! Embed tael inside another application: an in-process server, programmatic
//! queries through the typed client, CLI-style command dispatch, and the
//! interactive `tael live` TUI — all from one host binary.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p tael-cli --example embedded            # server + queries
//! cargo run -p tael-cli --example embedded -- --live  # also open the live TUI
//! ```

use std::time::Duration;

use tael_cli::tael_server::{ServerConfig, run_embedded};
use tael_cli::{Commands, GlobalOpts, TaelClient};

const REST_ADDR: &str = "127.0.0.1:17701";
const OTLP_ADDR: &str = "127.0.0.1:14317";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Start the full tael server (OTLP ingest, storage, REST API) on a
    //    background task inside this process. `run_embedded` is quiet: no
    //    startup banner, and it leaves the host's tracing subscriber alone.
    let mut config = ServerConfig::from_env();
    config.otlp_grpc_addr = OTLP_ADDR.to_string();
    config.rest_api_addr = REST_ADDR.to_string();
    config.rest_api_socket = None;
    let data_root = std::env::temp_dir().join("tael-embedded-example");
    config.data_dir = data_root.join("data").to_string_lossy().into_owned();
    config.wal_dir = data_root.join("wal").to_string_lossy().into_owned();
    tokio::spawn(run_embedded(config));

    let server_url = format!("http://{REST_ADDR}");
    let client = TaelClient::new(&server_url);

    // Wait until the REST API accepts connections.
    for attempt in 0..50 {
        match client.healthz().await {
            Ok(_) => break,
            Err(e) if attempt == 49 => return Err(e.context("embedded server never became ready")),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    println!("embedded tael server ready on {server_url} (OTLP gRPC on {OTLP_ADDR})");

    // 2. Query it programmatically with the typed client — structured JSON in,
    //    no subprocess, no stdout parsing.
    let services = client.list_services().await?;
    println!("services: {services}");

    // 3. Or dispatch any `tael` CLI command in-process, output rendering
    //    included — the same code path as the real binary.
    let opts = GlobalOpts {
        server: server_url.clone(),
        ..GlobalOpts::default()
    };
    tael_cli::run_command(Commands::Services, &opts).await?;

    // 4. Hand the terminal to the `tael live` TUI. It restores the terminal
    //    when the user quits, so the host application can carry on after.
    if std::env::args().any(|a| a == "--live") {
        tael_cli::tui::run_with_options(&server_url, tael_cli::tui::LiveOptions::default()).await?;
    }

    Ok(())
}

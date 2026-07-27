use clap::Parser;

fn main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{}", serde_json::json!({ "error": e.to_string() }));
            return std::process::ExitCode::FAILURE;
        }
    };
    // Exit codes encode the failure category so callers can branch without
    // parsing output — see `tael_cli::exit`.
    tael_cli::exit::finish(runtime.block_on(tael_cli::Cli::parse().run()))
}

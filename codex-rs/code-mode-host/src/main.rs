use clap::Parser;

const BUILD_VERSION: &str = match option_env!("CODEX_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Parser)]
#[command(version = BUILD_VERSION)]
struct Cli {
    /// Transport endpoint: `stdio`, `stdio://`, `ws://IP:PORT`, or `grpc://IP:PORT`.
    #[arg(
        long,
        value_name = "URL",
        default_value = codex_code_mode_host::DEFAULT_LISTEN_URL
    )]
    listen: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    codex_code_mode_host::run_main(&Cli::parse().listen).await
}

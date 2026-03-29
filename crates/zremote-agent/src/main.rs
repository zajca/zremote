use clap::Parser;

#[derive(Default, Parser)]
#[command(name = "zremote-agent", version, about = "ZRemote agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<zremote_agent::Commands>,
}

fn main() {
    let cli = Cli::try_parse().unwrap_or_default();
    zremote_agent::run(cli.command);
}

use anyhow::Result;
use clap::Parser;
use clap::ArgAction;
use codex_arg0::arg0_dispatch_or_else;
use codex_utils_cli::CliConfigOverrides;

#[derive(Parser, Debug)]
#[command(
    name = env!("CARGO_PKG_NAME"),
    about = "An ACP-compatible coding agent powered by Codex",
    disable_version_flag = true
)]
struct Cli {
    /// Print version
    #[arg(short = 'v', long = "version", action = ArgAction::SetTrue)]
    version: bool,
    #[command(flatten)]
    config_overrides: CliConfigOverrides,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    arg0_dispatch_or_else(|args| async move {
        nuwax_codex_acp::run_main(args.codex_linux_sandbox_exe, cli.config_overrides).await?;
        Ok(())
    })
}

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    smplx_cli_test::Cli::parse().run()?;

    Ok(())
}

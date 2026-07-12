use anyhow::Context as _;

fn main() -> anyhow::Result<()> {
    let launch = workbench::WorkbenchLaunch::from_cli_arguments(std::env::args_os().skip(1))
        .context("usage: workbench <collection-directory> <relative-markdown-path>")?;
    launch.run();
    Ok(())
}

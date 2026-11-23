mod cli;
mod collect;
mod db;
mod parse;
mod sync;

fn main() -> anyhow::Result<()> {
    cli::run()
}

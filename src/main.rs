mod cli;
mod collect;
mod db;
mod hypr;
mod parse;
mod sync;

fn main() -> anyhow::Result<()> {
    cli::run()
}

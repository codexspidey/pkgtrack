mod collect;
mod db;
mod parse;
mod sync;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let conn = db::open()?;

    if args.len() > 1 && args[1] == "sync" {
        sync::run(&conn)?;
    } else if args.len() > 1 && args[1] == "aggregate" {
        collect::aggregate(&conn)?;
    } else {
        println!("pkgtrack database initialized");
        println!("Available: sync, aggregate");
    }
    Ok(())
}

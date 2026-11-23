mod collect;
mod db;
mod parse;
mod sync;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // init commands do not touch SQLite
    if args.len() > 2 && args[1] == "init" && args[2] == "bash" {
        print!("{}", include_str!("bash_hook.sh"));
        return Ok(());
    }

    let conn = db::open()?;

    if args.len() > 1 && args[1] == "sync" {
        sync::run(&conn)?;
    } else if args.len() > 1 && args[1] == "aggregate" {
        collect::aggregate(&conn)?;
    } else {
        println!("pkgtrack database initialized");
        println!("Available: sync, aggregate, init bash");
    }
    Ok(())
}

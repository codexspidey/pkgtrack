mod db;
mod sync;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let conn = db::open()?;

    if args.len() > 1 && args[1] == "sync" {
        sync::run(&conn)?;
    } else {
        println!("pkgtrack database initialized");
        println!("Run 'pkgtrack sync' to rebuild package mapping");
    }
    Ok(())
}

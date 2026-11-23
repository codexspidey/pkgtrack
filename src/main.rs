mod db;

fn main() -> anyhow::Result<()> {
    let conn = db::open()?;
    let version = db::get_meta_int(&conn, "schema_version").unwrap_or(0);
    println!("pkgtrack database initialized (schema version {version})");
    Ok(())
}



use std::path::Path;

use dotenv::{dotenv, vars};

fn main() -> Result<(), anyhow::Error> {
    println!("cargo::rerun-if-changed=.env");
    
    if Path::new(".env").exists() {
        dotenv()?;

        for (key, value) in vars() {
            println!("cargo::rustc-env={key}={value}");
        }
    }

    Ok(())
}

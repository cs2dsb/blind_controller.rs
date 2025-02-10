use std::{env, fs, path::{Path, PathBuf}};
use chrono::Utc;

use dotenv::{dotenv, vars};

fn main() -> Result<(), anyhow::Error> {
    println!("cargo::rerun-if-changed=.env");
    
    if Path::new(".env").exists() {
        dotenv()?;

        for (key, value) in vars() {
            println!("cargo::rustc-env={key}={value}");
        }
    }

    let now = Utc::now();
    let millis = now.timestamp_millis();
    println!("cargo::rustc-env=BUILD_DATE={millis}");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    fs::write(out_dir.join("BUILD_DATE"), format!("{millis}"))?;

    println!("cargo:rustc-env=TARGET_TRIPLE={}", env::var("TARGET")?);

    Ok(())
}

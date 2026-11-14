#[cfg(target_os = "android")]
use anyhow::{Context, Result};
#[cfg(target_os = "android")]
use std::path::Path;

#[cfg(target_os = "android")]
fn main() -> Result<()> {
    let out = std::env::args().nth(1).context("usage: kms_blobs OUT_DIR")?;
    print!("{}", skyward_ui::surface::kms::dump_color_blobs("/dev/dri/card0", Path::new(&out))?);
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn main() {}

#[cfg(target_os = "android")]
use anyhow::Result;

#[cfg(target_os = "android")]
fn main() -> Result<()> {
    print!("{}", skyward_ui::surface::kms::dump_properties("/dev/dri/card0")?);
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn main() {}

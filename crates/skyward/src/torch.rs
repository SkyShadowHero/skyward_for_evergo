//! The flashlight: the switch the phone's flash driver leaves in the
//! recovery, one file it writes 1 to for the LED lit as a torch and 0 to
//! for it dark.

use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use std::fs;
use std::path::Path;

/// What to do with the flashlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TorchAction {
    /// Turn it on.
    On,
    /// Turn it off.
    Off,
    /// Say whether it is on, changing nothing.
    Status,
}

/// Do `action` to the flashlight, and say where it stands.
pub fn run(cfg: &skyward_config::Flashlight, action: TorchAction) -> Result<()> {
    let on = match action {
        TorchAction::On => set(cfg, true)?,
        TorchAction::Off => set(cfg, false)?,
        TorchAction::Status => status(cfg)?,
    };
    println!("the flashlight is {} at {}", if on { "on" } else { "off" }, cfg.path);
    Ok(())
}

/// Whether the flashlight is on, as its driver says: the switch's file
/// holds 1 while it is on and 0 while it is off.
pub fn status(cfg: &skyward_config::Flashlight) -> Result<bool> {
    let path = Path::new(&cfg.path);
    let text = fs::read_to_string(path).with_context(|| format!("read the flashlight's switch at {}", path.display()))?;
    match text.trim() {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(anyhow!("{} says {other:?}, which is neither 0 (off) nor 1 (on)", path.display())),
    }
}

/// Turn the flashlight on or off, and give whether it is on by then. A
/// write that fails may have reached the driver all the same, so a turn-on
/// that reports a failure is followed by a turn-off, that the LED is not
/// left lit.
pub fn set(cfg: &skyward_config::Flashlight, on: bool) -> Result<bool> {
    let path = Path::new(&cfg.path);
    if let Err(e) = write(path, on) {
        if on && let Err(off) = write(path, false) {
            eprintln!("turn the flashlight off again at {}: {off:#}", path.display());
        }
        return Err(e);
    }
    status(cfg)
}

/// Write 1 or 0 to the switch at `path`, naming the file in what it says if
/// it cannot.
fn write(path: &Path, on: bool) -> Result<()> {
    let value = if on { "1" } else { "0" };
    fs::write(path, value).with_context(|| format!("write {value} to the flashlight's switch at {}", path.display()))
}

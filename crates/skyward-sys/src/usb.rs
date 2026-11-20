//! The functions the USB port offers, which init sets up as
//! `sys.usb.config` asks: adb while the recovery waits, with files over MTP
//! beside it while file transfer is on.

use crate::{prop, set_prop, wait_prop};
use anyhow::{ensure, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

static MTP: AtomicBool = AtomicBool::new(false);

/// One change of the port at a time. The pages ask for one while the MTP
/// thread may ask for another, and a phone's controller takes ill to two
/// setups of the port at once: the properties then say it is in a state it
/// never reaches, and the port can be left with no function on it.
static CHANGING: Mutex<()> = Mutex::new(());

/// What the port offers while nothing else has it.
pub fn resting() -> &'static str {
    if MTP.load(Ordering::Relaxed) {
        "mtp,adb"
    } else {
        "adb"
    }
}

/// Whether MTP goes beside adb while the port rests: from when its function
/// has its descriptors until it goes.
pub fn set_mtp(on: bool) {
    MTP.store(on, Ordering::Relaxed);
}

/// What init says the port offers now.
pub fn state() -> Option<String> {
    prop("sys.usb.state")
}

/// Ask init for `config` and wait until it is in place. Going to "none"
/// also waits for adbd to be gone, so its function is free.
pub fn set(config: &str, timeout: Duration) -> Result<()> {
    let _changing = changing();
    set_taking(config, timeout)
}

/// `set`, with the port's changes already held.
fn set_taking(config: &str, timeout: Duration) -> Result<()> {
    set_prop("sys.usb.config", config)?;
    wait_prop("sys.usb.state", config, timeout)?;
    if config == "none" {
        let start = Instant::now();
        while !matches!(prop("init.svc.adbd").as_deref(), None | Some("stopped")) {
            ensure!(start.elapsed() < timeout, "adbd still runs {} ms after the port went to none", timeout.as_millis());
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

/// Hand the port to `config` through "none", which stops the programs
/// serving it and takes every function off; init then sets the port up
/// once the programs behind the new configuration are ready.
pub fn switch(config: &str, timeout: Duration) -> Result<()> {
    let _changing = changing();
    switch_taking(config, timeout)
}

/// `switch`, with the port's changes already held.
fn switch_taking(config: &str, timeout: Duration) -> Result<()> {
    set_taking("none", timeout)?;
    set_taking(config, timeout)
}

/// `switch`, or nothing at all where the port offers `config` already: a
/// port set up again costs a computer the connection it has, and taking the
/// functions off and putting them back for nothing is the one change a
/// controller does not always come back from.
pub fn switch_if_needed(config: &str, timeout: Duration) -> Result<()> {
    let _changing = changing();
    if state().as_deref() == Some(config) {
        return Ok(());
    }
    switch_taking(config, timeout)
}

/// Wait a change of the port under way, out of the others' way.
fn changing() -> MutexGuard<'static, ()> {
    CHANGING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

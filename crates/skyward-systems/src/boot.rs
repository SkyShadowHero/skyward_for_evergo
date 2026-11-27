//! The init_boot image the other system starts from: the bootloader joins
//! its ramdisk to the installed system's vendor ramdisk, and the program
//! Skyward puts in it finds the system's root in the data partition and
//! starts the system's own first program from it.
//!
//! On a phone whose boot partition holds the kernel and the first ramdisk
//! together, there is no init_boot partition and no recovery partition to
//! put away: the installed system's own first ramdisk is used in the other
//! system's boot image, with Skyward's program as the kernel's first one
//! and the recovery's own kept beside it, for the starts the bootloader
//! makes for the recovery.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use skyward_block::area::Area;
use sha2::{Digest, Sha256};
use skyward_bootimg::{lz4_legacy, BootHeader, BootImage, Format, Ramdisk};

/// Where the program finds what to start, in the ramdisk.
pub const START: &str = "skyward/system.toml";
/// The program, as the system's first program where the system brings no
/// ramdisk of its own.
pub const INIT: &str = "init";
/// The program, where the system brings its own ramdisk: it lays the root
/// out as a device and says where, for that ramdisk's own first program.
pub const MAP: &str = "skyward/map";
/// The file that says the boot image holds a system beside the installed
/// one and the recovery's own first ramdisk together: the kernel unpacks it
/// on a phone whose boot partition carries both, and Skyward's program,
/// which the kernel starts, reads it to know that a start the bootloader
/// did not say is the system's is the recovery's.
pub const CARRIER: &str = "skyward/carrier";
/// The installed system's own first program, where the kernel starts it:
/// the second-stage init of the ramdisk the boot partition holds, which the
/// bootloader's recovery start goes on from.
pub const RECOVERY: &str = "system/bin/init";

/// The private Android metadata filesystem comes first after the area header;
/// the rest of the area is the block device Android encrypts for /data.
pub const ANDROID_METADATA_BYTES: u64 = 64 << 20;
pub const ANDROID_DATA_BYTES: u64 = 16 << 30;
pub const ANDROID_AREA_BYTES: u64 = skyward_block::area::HEADER + ANDROID_METADATA_BYTES + ANDROID_DATA_BYTES;
pub const ANDROID_BOOT_MARKER: &str = "bootstat/skyward-boot";

/// What the program reads of the system it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    /// The system's id, which the program names when the start stops.
    pub id: String,
    pub filesystem: String,
    pub init: String,
    pub root: Area,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub android: Option<Area>,
}

impl Start {
    pub fn parse(text: &str) -> Result<Start> {
        toml::from_str(text).context("read what the system starts with")
    }
}

/// Whether the root holds Android's second stage, rather than a program
/// which can start with the root and the kernel filesystems alone.
pub fn android_root(root: &std::path::Path, init: &str) -> bool {
    (init == "/init" || init == "/system/bin/init") && root.join(RECOVERY).is_file() && root.join("system/etc/init/hw/init.rc").is_file()
}

/// Skyward's archive in an init_boot image: `program` at `at`, and `start`
/// beside it, compressed as the vendor ramdisk before it is, for the kernel
/// to read on into it from there.
fn archive(at: &str, program: &[u8], start: &Start) -> Result<Vec<u8>> {
    let mut rd = Ramdisk { entries: Vec::new() };
    rd.add(at, 0o750, program.to_vec())?;
    rd.add(START, 0o644, toml::to_string(start).context("write what the system starts with")?.into_bytes())?;
    Ok(lz4_legacy(&rd.archive()?))
}

/// The init_boot image that starts the system `start` describes: the
/// header of `template`, the phone's own init_boot, and a ramdisk holding
/// `program` as the first program with `start` beside it.
pub fn init_boot(template: &[u8], program: &[u8], start: &Start) -> Result<Vec<u8>> {
    let mut image = BootImage::parse(template).context("read the phone's init_boot image")?;
    ensure!(image.kernel.is_empty(), "the phone's init_boot image holds a kernel");
    image.ramdisk = archive(INIT, program, start)?;
    image.to_bytes()
}

/// The system's own init_boot image `own` with the program as [`MAP`] and
/// `start` beside it, in an archive after its ramdisk's own; and the bytes
/// of that ramdisk of its own.
pub fn with_start(own: &[u8], program: &[u8], start: &Start) -> Result<(Vec<u8>, u64)> {
    let mut image = BootImage::parse(own).context("read the system's init_boot image")?;
    let own_ramdisk = image.ramdisk.len() as u64;
    image.ramdisk.extend_from_slice(&archive(MAP, program, start)?);
    Ok((image.to_bytes()?, own_ramdisk))
}

/// The boot set of the system `start` describes, on a phone whose boot
/// partition holds the kernel and the first ramdisk together: the other
/// system's kernel, and the installed system's own first ramdisk with
/// Skyward's first program made its `/init`, so that a start the bootloader
/// says is the system's goes on to the system in the data partition, and
/// one it says is the recovery's hands back to the recovery that ramdisk
/// carries. `recovery` is the installed system's boot image as the phone
/// holds it, `other` the other system's boot image, and `program` Skyward's
/// first program, as the recovery it runs from keeps it.
pub fn boot_carrier(recovery: &[u8], other: &[u8], program: &[u8], start: &Start) -> Result<Vec<u8>> {
    let installed = BootImage::parse(recovery).context("read the installed system's boot image")?;
    let other = BootImage::parse(other).context("read the other system's boot image")?;
    ensure!(!other.kernel.is_empty(), "the other system's boot image holds no kernel");
    let mut rd = Ramdisk::decode(&installed.ramdisk).context("read the installed system's first ramdisk")?;
    ensure!(rd.find(CARRIER).is_none(), "the installed system's boot image starts a system beside it already");
    ensure!(
        rd.find(RECOVERY).is_some(),
        "the installed system's first ramdisk holds no {RECOVERY}, which the recovery comes up from"
    );
    // The kernel starts Skyward's first program, which leaves the recovery's
    // own program where it finds it and hands back to it.
    rd.entries.retain(|entry| entry.name != INIT);
    rd.link(INIT, INIT_PROGRAM)?;
    if rd.find(INIT_PROGRAM).is_some() {
        rd.replace(INIT_PROGRAM, program.to_vec())?;
    } else {
        rd.add(INIT_PROGRAM, 0o755, program.to_vec())?;
    }
    rd.add(CARRIER, 0o644, b"boot\n".to_vec())?;
    rd.add(START, 0o644, toml::to_string(start).context("write what the system starts with")?.into_bytes())?;
    let ramdisk = packed(&installed.ramdisk, &rd)?;
    // The other system's image is the one whose kernel, device tree and
    // command line are that system's own where it keeps them in a legacy
    // header, as this phone's bootloader reads them; an image whose header
    // is the ramdisk-only one of recent phones has no such fields, so the
    // installed system's header lends them.
    let mut image = if matches!(&other.header, BootHeader::Legacy(_)) {
        other
    } else {
        let mut image = installed;
        image.kernel = other.kernel;
        if !other.dtb.is_empty() {
            image.dtb = other.dtb;
        }
        image
    };
    image.ramdisk = ramdisk;
    image.to_bytes()
}

/// The first program Skyward keeps in the ramdisk of a phone whose boot
/// partition holds both systems' first ramdisks, which the kernel starts.
pub(crate) const INIT_PROGRAM: &str = "skyward/system-init";

/// A fresh carrier using the stored second-system kernel and the current
/// Recovery ramdisk, including its configuration, tools and first program.
pub fn carrier_with_recovery(stored: &[u8], recovery: &[u8], program: &[u8], start: &Start, partition_size: u64) -> Result<Vec<u8>> {
    let bytes = boot_carrier(recovery, stored, program, start)?;
    crate::footer::footed(&bytes, crate::store::BOOT, partition_size, &Sha256::digest(&bytes))
}

/// The installed system's first ramdisk `old` with `rd` packed in its place,
/// as the image wraps it: gzip or LZ4 in the legacy framing, either of which
/// the kernel on the phone unpacks, and both packed in process, as the
/// recovery has no other tool for it.
fn packed(old: &[u8], rd: &Ramdisk) -> Result<Vec<u8>> {
    let lz4 = [0x02u8, 0x21, 0x4c, 0x18];
    let gzip = [0x1fu8, 0x8b];
    let found = (0..old.len().saturating_sub(lz4.len())).find_map(|at| {
        let rest = &old[at..];
        if rest.starts_with(&lz4) {
            Some((at, Format::Lz4))
        } else if rest.starts_with(&gzip) {
            Some((at, Format::Gzip))
        } else {
            None
        }
    });
    let (at, format) = found.context("the installed system's first ramdisk is no archive the kernel reads")?;
    let mut out = old[..at].to_vec();
    match format {
        Format::Lz4 => out.extend_from_slice(&lz4_legacy(&rd.archive()?)),
        Format::Gzip => out.extend_from_slice(&rd.encode_as(Format::Gzip)?),
    }
    Ok(out)
}

/// What an LZ4 archive in the legacy framing opens with, as Skyward's
/// archive does.
const LZ4_LEGACY: [u8; 4] = [0x02, 0x21, 0x4c, 0x18];

/// Whether `image` is an init_boot image that starts a system from the
/// data partition: one whose ramdisk ends in Skyward's archive, which says
/// in [`START`] what to start. The archive comes last in the ramdisk,
/// after the system's own where it brings one, so it is looked for from
/// each place an archive may open, the last first. Anything else an
/// init_boot partition may hold, an image that does not read among it, is
/// none of Skyward's.
pub fn starts_from_data(image: &[u8]) -> bool {
    let Ok(image) = BootImage::parse(image) else { return false };
    let mut before = image.ramdisk.len();
    while let Some(at) = image.ramdisk[..before].windows(LZ4_LEGACY.len()).rposition(|opening| opening == LZ4_LEGACY) {
        if Ramdisk::decode(&image.ramdisk[at..]).is_ok_and(|archive| archive.find(START).is_some()) {
            return true;
        }
        before = at;
    }
    false
}

/// The init_boot image `image`, as [`init_boot`] made it or, for a system
/// whose own ramdisk of `own_ramdisk` bytes comes first in it, as
/// [`with_start`] did, made again with `program` and `start`.
pub fn with_program(image: &[u8], own_ramdisk: Option<u64>, program: &[u8], start: &Start) -> Result<Vec<u8>> {
    let mut image = BootImage::parse(image).context("read the other system's init_boot image")?;
    let (kept, at) = match own_ramdisk {
        Some(bytes) => (bytes, MAP),
        None => (0, INIT),
    };
    ensure!(kept <= image.ramdisk.len() as u64, "the other system's own ramdisk of {kept} bytes is longer than the {} its init_boot image holds", image.ramdisk.len());
    image.ramdisk.truncate(kept as usize);
    image.ramdisk.extend_from_slice(&archive(at, program, start)?);
    image.to_bytes()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::state::tests::area;
    use abootimg_oxide::{HeaderV3, OsVersionPatch};
    use skyward_bootimg::BootHeader;

    /// A version 4 init_boot image with `ramdisk` and no kernel, filling a
    /// partition of a MiB.
    pub fn template(ramdisk: &[u8]) -> Vec<u8> {
        let header = HeaderV3 {
            kernel_size: 0,
            ramdisk_size: ramdisk.len() as u32,
            osversionpatch: OsVersionPatch(0),
            cmdline: Box::new([0; 1536]),
            v4_signature_size: Some(0),
        };
        let mut image = BootImage {
            header: BootHeader::RamdiskOnly(header),
            kernel: Vec::new(),
            ramdisk: ramdisk.to_vec(),
            second: Vec::new(),
            recovery_dtbo: Vec::new(),
            dtb: Vec::new(),
        }
        .to_bytes()
        .unwrap();
        image.resize(1 << 20, 0);
        image
    }

    /// The entries of Skyward's archive `bytes`, by name, with their data.
    pub fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(bytes[..4], [0x02, 0x21, 0x4c, 0x18], "LZ4 in the legacy framing, as the vendor ramdisk is");
        Ramdisk::decode(bytes).unwrap().entries.into_iter().map(|e| (e.name, e.data)).collect()
    }

    pub fn start() -> Start {
        Start { id: "test".into(), filesystem: "ext4".into(), init: "/sbin/init".into(), root: area("/data/skyward/systems/test", 1 << 20), android: None }
    }

    #[test]
    fn the_new_ramdisk_starts_the_program_with_what_it_starts_beside_it() {
        let image = init_boot(&template(b"stock"), b"program", &start()).unwrap();
        let back = BootImage::parse(&image).unwrap();
        assert_eq!(back.header.header_version(), 4);
        assert!(back.kernel.is_empty());
        let found = entries(&back.ramdisk);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, [INIT, "skyward", START]);
        assert_eq!(found[0].1, b"program");
        assert_eq!(Start::parse(std::str::from_utf8(&found[2].1).unwrap()).unwrap(), start());
    }

    #[test]
    fn a_systems_own_ramdisk_keeps_its_own_first_program() {
        let own = template(b"its own archive");
        let (image, own_ramdisk) = with_start(&own, b"program", &start()).unwrap();
        assert_eq!(own_ramdisk, b"its own archive".len() as u64);
        let back = BootImage::parse(&image).unwrap();
        let (theirs, ours) = back.ramdisk.split_at(b"its own archive".len());
        assert_eq!(theirs, b"its own archive", "its archive first");
        let names: Vec<String> = entries(ours).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["skyward", MAP, START], "and no first program of Skyward's");
    }

    #[test]
    fn an_image_that_starts_a_system_from_the_data_partition_is_told_from_any_other() {
        let mut ours = init_boot(&template(b"stock"), b"program", &start()).unwrap();
        // As a partition holds it: zeros to the partition's end.
        ours.resize(1 << 20, 0);
        assert!(starts_from_data(&ours));
        // A system's own archive is an LZ4 archive too, and Skyward's
        // comes after it.
        let its_own = lz4_legacy(&Ramdisk { entries: Vec::new() }.archive().unwrap());
        let (after_its_own, _) = with_start(&template(&its_own), b"program", &start()).unwrap();
        assert!(starts_from_data(&after_its_own));
        assert!(!starts_from_data(&template(&its_own)), "the installed system's own image");
        assert!(!starts_from_data(&template(b"stock")), "a ramdisk that is no archive");
        assert!(!starts_from_data(&vec![0u8; 1 << 20]), "a partition erased");
        assert!(!starts_from_data(b""));
    }

    #[test]
    fn an_image_made_again_carries_the_new_program_and_nothing_of_the_old() {
        let mut made = init_boot(&template(b"stock"), b"old program", &start()).unwrap();
        // As the store and the partition hold it: zeros to the partition's end.
        made.resize(made.len() + 8192, 0);
        let again = BootImage::parse(&with_program(&made, None, b"new program", &start()).unwrap()).unwrap();
        assert_eq!((again.header.header_version(), again.kernel.len()), (4, 0), "still an init_boot image");
        let found = entries(&again.ramdisk);
        assert_eq!(found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), [INIT, "skyward", START]);
        assert_eq!(found[0].1, b"new program");
        assert_eq!(Start::parse(std::str::from_utf8(&found[2].1).unwrap()).unwrap(), start());
    }

    #[test]
    fn a_systems_own_ramdisk_is_kept_when_its_image_is_made_again() {
        let (made, own_ramdisk) = with_start(&template(b"its own archive"), b"old program", &start()).unwrap();
        let again = BootImage::parse(&with_program(&made, Some(own_ramdisk), b"new", &start()).unwrap()).unwrap();
        let (theirs, ours) = again.ramdisk.split_at(own_ramdisk as usize);
        assert_eq!(theirs, b"its own archive");
        let found = entries(ours);
        assert_eq!(found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["skyward", MAP, START]);
        assert_eq!(found[1].1, b"new");
        assert!(with_program(&made, Some(1 << 20), b"new", &start()).is_err(), "an own ramdisk longer than the image's");
    }

    /// A version 2 image with `kernel` and `ramdisk` in the layout this
    /// phone's bootloader reads, filling a partition of `size` bytes.
    fn legacy(kernel: &[u8], ramdisk: &[u8], size: usize) -> Vec<u8> {
        use abootimg_oxide::{HeaderV0, HeaderV0Versioned};
        let header = HeaderV0 {
            kernel_size: 0,
            kernel_addr: 0x4008_0000,
            ramdisk_size: 0,
            ramdisk_addr: 0x5110_0000,
            second_bootloader_size: 0,
            second_bootloader_addr: 0,
            tags_addr: 0x47c8_0000,
            page_size: 2048,
            osversionpatch: OsVersionPatch(0),
            board_name: [0; 16],
            cmdline: Box::new([0; 512 + 1024]),
            hash_digest: [0; 32],
            versioned: HeaderV0Versioned::V2 { recovery_dtbo_size: 0, recovery_dtbo_addr: 0, dtb_size: 0, dtb_addr: 0 },
        };
        let mut image = BootImage {
            header: BootHeader::Legacy(header),
            kernel: kernel.to_vec(),
            ramdisk: ramdisk.to_vec(),
            second: Vec::new(),
            recovery_dtbo: Vec::new(),
            dtb: Vec::new(),
        }
        .to_bytes()
        .unwrap();
        image.resize(size, 0);
        image
    }

    /// The installed system's boot ramdisk: the recovery's own programs in
    /// it, and `/init` as the stock init's link, as the phone's is.
    fn recovery_ramdisk() -> Ramdisk {
        let mut rd = Ramdisk { entries: Vec::new() };
        rd.add(RECOVERY, 0o750, b"second-stage init".to_vec()).unwrap();
        rd.add("system/bin/recovery", 0o750, b"skyward".to_vec()).unwrap();
        rd.link("init", "system/bin/init").unwrap();
        rd.add("skyward/system-init", 0o750, b"old program".to_vec()).unwrap();
        rd
    }

    #[test]
    fn a_boot_carrier_image_is_made_again_with_the_program_of_the_skyward_that_switches() {
        let mut rd = Ramdisk { entries: Vec::new() };
        rd.add(RECOVERY, 0o750, b"second-stage init".to_vec()).unwrap();
        rd.add("system/bin/recovery", 0o750, b"skyward".to_vec()).unwrap();
        rd.link("init", "system/bin/init").unwrap();
        rd.add(INIT_PROGRAM, 0o750, b"old program".to_vec()).unwrap();
        let installed = template(&rd.encode_as(Format::Gzip).unwrap());
        let mut other = BootImage::parse(&template(b"stock")).unwrap();
        other.kernel = b"other kernel".to_vec();
        let other = other.to_bytes().unwrap();
        let carrier = boot_carrier(&installed, &other, b"old program", &start()).unwrap();
        let partition = 2 << 20;
        rd.replace("system/bin/recovery", b"new recovery".to_vec()).unwrap();
        let updated = template(&rd.encode_as(Format::Gzip).unwrap());
        let mut now = start();
        now.android = Some(area("/data/skyward/systems/test.android", ANDROID_AREA_BYTES));
        let again = carrier_with_recovery(&carrier, &updated, b"new program", &now, partition).unwrap();
        assert_eq!(again.len() as u64, partition, "it fills the boot partition");
        assert_eq!(&again[again.len() - 64..again.len() - 60], b"AVBf", "with a footer of its own");
        let back = BootImage::parse(&again).unwrap();
        assert_eq!(back.kernel, b"other kernel", "the other system's kernel, as it was");
        let rd = Ramdisk::decode(&back.ramdisk).unwrap();
        assert_eq!(rd.find(INIT_PROGRAM).unwrap().data, b"new program", "the program at hand");
        assert!(rd.find(CARRIER).is_some(), "and the recovery still in the ramdisk");
        assert_eq!(rd.find("system/bin/recovery").unwrap().data, b"new recovery");
        assert_eq!(Start::parse(std::str::from_utf8(&rd.find(START).unwrap().data).unwrap()).unwrap(), now);
        assert!(carrier_with_recovery(&carrier, &template(b"stock"), b"new", &now, partition).is_err(), "an image that leaves no recovery to go back to");
    }

    #[test]
    fn a_boot_carrier_image_starts_the_other_system_and_keeps_the_recovery() {
        let rd = recovery_ramdisk();
        let installed = legacy(b"installed kernel", &rd.encode_as(Format::Gzip).unwrap(), 1 << 20);
        let other = legacy(b"other kernel", b"", 1 << 20);
        let image = boot_carrier(&installed, &other, b"new program", &start()).unwrap();
        let back = BootImage::parse(&image).unwrap();
        assert_eq!(back.header.header_version(), 2, "the phone's own header, which its bootloader reads");
        assert_eq!(back.kernel, b"other kernel", "the other system's kernel");
        assert_eq!(&back.ramdisk[..2], &[0x1f, 0x8b], "the ramdisk packed the way the phone's own is");
        let rd = Ramdisk::decode(&back.ramdisk).unwrap();
        let recovery = rd.find(RECOVERY).expect("the recovery comes up from it still");
        assert_eq!(recovery.data, b"second-stage init");
        let init = rd.find(INIT).expect("the kernel starts Skyward's program");
        assert_eq!((init.mode & 0o170000, init.data.as_slice()), (0o120000, b"skyward/system-init".as_slice()));
        let program = rd.find(INIT_PROGRAM).expect("Skyward's program is there for the link");
        assert_eq!(program.data, b"new program", "the program at hand, in place of the one installed with it");
        assert!(rd.find(CARRIER).is_some(), "the program knows the recovery is in this ramdisk");
        assert_eq!(Start::parse(std::str::from_utf8(&rd.find(START).unwrap().data).unwrap()).unwrap(), start());

        // The same with the other framing the kernel reads.
        let lz4 = lz4_legacy(&recovery_ramdisk().archive().unwrap());
        let installed = legacy(b"installed kernel", &lz4, 1 << 20);
        let image = boot_carrier(&installed, &other, b"program", &start()).unwrap();
        let back = BootImage::parse(&image).unwrap();
        assert_eq!(&back.ramdisk[..4], &LZ4_LEGACY, "packed as it was, in process");
        assert!(Ramdisk::decode(&back.ramdisk).unwrap().find(CARRIER).is_some());
        assert!(boot_carrier(&installed, &legacy(b"", b"", 8192), b"program", &start()).is_err(), "no kernel to start");
        let no_recovery = {
            let mut rd = recovery_ramdisk();
            rd.entries.retain(|e| e.name != RECOVERY);
            legacy(b"installed kernel", &rd.encode_as(Format::Gzip).unwrap(), 1 << 20)
        };
        assert!(boot_carrier(&no_recovery, &other, b"program", &start()).is_err(), "no recovery to come up from");
    }
}

//! The boot set of a second system for a phone whose boot partition holds
//! the kernel and the first ramdisk together, made from the phone's own
//! images: the installed system's boot image with Skyward packed on it,
//! and the other system's boot image.
//!
//! The images are named by `EVERGO_BOOT` (the installed system's boot image,
//! as the boot partition holds it), `EVERGO_STOCK` (the other system's boot
//! image) and `EVERGO_OUT` (where the boot set is written). Run with
//! `EVERGO_BOOT=... EVERGO_STOCK=... EVERGO_OUT=... cargo test -p
//! skyward-systems --test evergo_boot_carrier -- --ignored --nocapture`.
//! The resulting images contain dummy disk extents and must not be flashed.

use sha2::{Digest, Sha256};
use skyward_block::area::{Area, HEADER};
use skyward_bootimg::{BootImage, Ramdisk};
use skyward_systems::boot::{self, Start, INIT, RECOVERY, START};
use std::path::Path;

/// The boot partition of the Redmi Note 11 5G, in bytes.
const PARTITION: u64 = 128 << 20;

#[test]
#[ignore = "the phone's own images are not in the repository"]
fn the_boot_set_of_a_second_system_is_made_from_the_phones_own_images() {
    let read = |name: &str| std::fs::read(std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))).unwrap();
    let installed = read("EVERGO_BOOT");
    let other = read("EVERGO_STOCK");
    let out = Path::new(&std::env::var("EVERGO_OUT").expect("EVERGO_OUT is not set")).to_path_buf();
    std::fs::create_dir_all(&out).unwrap();

    let recovery = Ramdisk::decode(&BootImage::parse(&installed).unwrap().ramdisk).unwrap();
    let program = &recovery.find("skyward/system-init").expect("the recovery carries Skyward's first program").data;
    let root_size = (16 << 30) + HEADER;
    let start = Start {
        id: "lunaris".into(),
        filesystem: "ext4".into(),
        init: "/init".into(),
        root: Area {
            partition: "userdata".into(),
            file: "/data/skyward/systems/lunaris.img".into(),
            id: "00112233445566778899aabbccddeeff".into(),
            size: root_size,
            extents: vec![(1 << 20, root_size)],
        },
        android: Some(Area {
            partition: "userdata".into(),
            file: "/data/skyward/systems/lunaris.android".into(),
            id: "ffeeddccbbaa99887766554433221100".into(),
            size: boot::ANDROID_AREA_BYTES,
            extents: vec![((1 << 20) + root_size, boot::ANDROID_AREA_BYTES)],
        }),
    };

    let image = boot::boot_carrier(&installed, &other, program, &start).expect("make the boot set");
    let path = out.join("second-boot.img");
    std::fs::write(&path, &image).unwrap();
    let booted = BootImage::parse(&image).unwrap();
    assert_eq!(booted.header.header_version(), 2, "the phone's bootloader reads a legacy header");
    let second = BootImage::parse(&other).unwrap();
    assert_eq!(booted.kernel, second.kernel, "the other system's kernel");
    assert_eq!(booted.dtb, second.dtb, "the other system's device tree");
    let rd = Ramdisk::decode(&booted.ramdisk).unwrap();
    let init = rd.find(INIT).expect("the kernel starts Skyward's first program");
    assert_eq!((init.mode & 0o170000, init.data.as_slice()), (0o120000, b"skyward/system-init".as_slice()));
    assert_eq!(&rd.find("skyward/system-init").unwrap().data, program, "the program at hand, for the link");
    assert_eq!(rd.find(RECOVERY).expect("the recovery's own first program stays").data, recovery.find(RECOVERY).unwrap().data, "as the installed system's ramdisk has it");
    assert!(rd.find(boot::CARRIER).is_some(), "the program knows the recovery is in this ramdisk");
    assert_eq!(Start::parse(std::str::from_utf8(&rd.find(START).unwrap().data).unwrap()).unwrap(), start);
    assert!(rd.find("system/bin/recovery").is_some(), "the recovery's own program stays in the ramdisk");

    let whole = skyward_systems::footer::footed(&image, "boot", PARTITION, &Sha256::digest(&image)).expect("foot the boot set");
    assert_eq!(whole.len() as u64, PARTITION, "it fills the partition");
    std::fs::write(out.join("second-boot-partition.img"), &whole).unwrap();

    // The same boot set as a switch makes it again: the program of the
    // Skyward that switches goes into it, and the recovery it carries stays.
    let stored = {
        let image = boot::boot_carrier(&installed, &other, b"older program", &start).expect("make the boot set as an older Skyward did");
        skyward_systems::footer::footed(&image, "boot", PARTITION, &Sha256::digest(&image)).expect("foot it")
    };
    let again = boot::carrier_with_recovery(&stored, &installed, program, &start, PARTITION).expect("make the boot set again");
    assert_eq!(again.len() as u64, PARTITION, "it fills the partition");
    assert_eq!(&again[again.len() - 64..again.len() - 60], b"AVBf", "with a footer of its own");
    let back = BootImage::parse(&again).unwrap();
    let rd = Ramdisk::decode(&back.ramdisk).unwrap();
    assert_eq!(&rd.find("skyward/system-init").unwrap().data, program, "the program at hand, in place of the older");
    assert!(rd.find(boot::CARRIER).is_some(), "the recovery still in the ramdisk");
    assert_eq!(Start::parse(std::str::from_utf8(&rd.find(START).unwrap().data).unwrap()).unwrap(), start, "what it starts, as it was");
    assert_eq!(rd.find(RECOVERY).unwrap().data, recovery.find(RECOVERY).unwrap().data, "and the recovery's own first program");
    std::fs::write(out.join("second-boot-partition-refreshed.img"), &again).unwrap();
    println!("boot set: {} bytes, kernel {} bytes, ramdisk {} bytes, whole partition {} bytes; refreshed {} bytes", image.len(), booted.kernel.len(), booted.ramdisk.len(), whole.len(), again.len());
}

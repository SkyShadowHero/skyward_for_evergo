use abootimg_oxide::{binrw::io::Cursor, Header, HeaderV0Versioned, HeaderV3};
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::Path;

const PAGE: usize = 4096;

/// The header of a boot image: the ramdisk-only layout of versions 3 and 4
/// (`boot`, `init_boot`, `recovery` of recent phones), or the legacy layout
/// of versions 0 to 2, which carries load addresses and, from version 2, the
/// device tree and a recovery dtbo. Xiaomi's Android 11 and 12 images keep
/// their kernel in a version 2 header on phones with no recovery partition,
/// so the recovery is packed on top of the phone's `boot` partition there.
#[derive(Clone, Debug)]
pub enum BootHeader {
    Legacy(abootimg_oxide::HeaderV0),
    RamdiskOnly(HeaderV3),
}

impl BootHeader {
    pub fn header_version(&self) -> u32 {
        match self {
            BootHeader::Legacy(h) => h.header_version(),
            BootHeader::RamdiskOnly(h) => h.header_version(),
        }
    }
}

/// A boot image split into its header, kernel, ramdisk and, where a legacy
/// header carries them, the second bootloader, the recovery dtbo and the
/// device tree.
pub struct BootImage {
    pub header: BootHeader,
    pub kernel: Vec<u8>,
    pub ramdisk: Vec<u8>,
    pub second: Vec<u8>,
    pub recovery_dtbo: Vec<u8>,
    pub dtb: Vec<u8>,
}

fn section(bytes: &[u8], at: usize, len: usize, what: &str) -> Result<Vec<u8>> {
    let end = at.checked_add(len).ok_or_else(|| anyhow!("{what} runs past the end of the image"))?;
    bytes.get(at..end).map(<[u8]>::to_vec).ok_or_else(|| anyhow!("{what} runs past the end of the image"))
}

impl BootImage {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        BootImage::parse(&bytes).with_context(|| format!("read {}", path.display()))
    }

    /// The image `bytes` hold, as a partition holds it: the image, then
    /// whatever fills the partition after it.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let header = match Header::parse(&mut Cursor::new(bytes)).context("header")? {
            Header::V3(h) => BootHeader::RamdiskOnly(h),
            Header::V0(h) if h.header_version() == 2 => BootHeader::Legacy(h),
            Header::V0(h) => bail!("header version {} predates the ramdisk-only layout", h.header_version()),
        };
        let (kernel_size, ramdisk_size, page) = match &header {
            BootHeader::RamdiskOnly(h) => (h.kernel_size as usize, h.ramdisk_size as usize, PAGE),
            BootHeader::Legacy(h) => (h.kernel_size as usize, h.ramdisk_size as usize, h.page_size as usize),
        };
        let mut image = BootImage {
            kernel: section(bytes, page, kernel_size, "kernel")?,
            ramdisk: section(bytes, page + page_align(kernel_size, page), ramdisk_size, "ramdisk")?,
            second: Vec::new(),
            recovery_dtbo: Vec::new(),
            dtb: Vec::new(),
            header,
        };
        if let BootHeader::Legacy(h) = &image.header {
            // Each section of a legacy image follows the last, padded to the
            // page size; the sections of no size are left out.
            let page = h.page_size as usize;
            let mut at = page + page_align(kernel_size, page) + page_align(ramdisk_size, page);
            let (second_size, dtbo_size, dtb_size) = match h.versioned {
                HeaderV0Versioned::V0 => (0, 0, 0),
                HeaderV0Versioned::V1 { recovery_dtbo_size, .. } => {
                    (h.second_bootloader_size as usize, recovery_dtbo_size as usize, 0)
                }
                HeaderV0Versioned::V2 { recovery_dtbo_size, dtb_size, .. } => {
                    (h.second_bootloader_size as usize, recovery_dtbo_size as usize, dtb_size as usize)
                }
            };
            if second_size > 0 {
                image.second = section(bytes, at, second_size, "second bootloader")?;
            }
            at += page_align(second_size, page);
            if dtbo_size > 0 {
                image.recovery_dtbo = section(bytes, at, dtbo_size, "recovery dtbo")?;
            }
            at += page_align(dtbo_size, page);
            if dtb_size > 0 {
                image.dtb = section(bytes, at, dtb_size, "dtb")?;
            }
        }
        Ok(image)
    }

    /// Make `rd` the image's ramdisk, wrapped the way the image already wraps
    /// it and keeping whatever header the maker puts ahead of the archive, as
    /// Xiaomi keeps two pages of one.
    pub fn replace_ramdisk(&mut self, rd: &crate::Ramdisk) -> Result<()> {
        let old = std::mem::take(&mut self.ramdisk);
        let (at, format) = crate::ramdisk::locate(&old)
            .with_context(|| format!("the ramdisk's {} bytes hold no LZ4 or gzip archive", old.len()))?;
        let mut ramdisk = old[..at].to_vec();
        ramdisk.extend_from_slice(&rd.encode_as(format)?);
        self.ramdisk = ramdisk;
        Ok(())
    }

    /// The image with sizes taken from its parts. A version 3 or 4 header
    /// keeps its version; a version 4 header carries no boot signature, which
    /// unlocked bootloaders do not ask for. A legacy header keeps its load
    /// addresses, its command line, its hash and its version 2 device tree.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        match &self.header {
            BootHeader::RamdiskOnly(header) => {
                let mut header = header.clone();
                header.kernel_size = u32::try_from(self.kernel.len()).context("kernel size")?;
                header.ramdisk_size = u32::try_from(self.ramdisk.len()).context("ramdisk size")?;
                header.v4_signature_size = header.v4_signature_size.map(|_| 0);
                Header::V3(header).write(&mut out).context("write header")?;
                pad(&mut out, PAGE);
                out.extend_from_slice(&self.kernel);
                pad(&mut out, PAGE);
                out.extend_from_slice(&self.ramdisk);
                pad(&mut out, PAGE);
            }
            BootHeader::Legacy(header) => {
                let page = header.page_size as usize;
                let mut header = header.clone();
                header.kernel_size = u32::try_from(self.kernel.len()).context("kernel size")?;
                header.ramdisk_size = u32::try_from(self.ramdisk.len()).context("ramdisk size")?;
                header.second_bootloader_size = u32::try_from(self.second.len()).context("second size")?;
                match &mut header.versioned {
                    HeaderV0Versioned::V0 => {}
                    HeaderV0Versioned::V1 { recovery_dtbo_size, .. } => {
                        *recovery_dtbo_size = u32::try_from(self.recovery_dtbo.len()).context("dtbo size")?;
                    }
                    HeaderV0Versioned::V2 { recovery_dtbo_size, dtb_size, .. } => {
                        *recovery_dtbo_size = u32::try_from(self.recovery_dtbo.len()).context("dtbo size")?;
                        *dtb_size = u32::try_from(self.dtb.len()).context("dtb size")?;
                    }
                }
                Header::V0(header).write(&mut out).context("write header")?;
                // The kernel starts on the page after the header, as the
                // bootloader reads it; the sections follow, each padded, with
                // the empty ones left out.
                pad(&mut out, page);
                for part in [&self.kernel, &self.ramdisk, &self.second, &self.recovery_dtbo, &self.dtb] {
                    if part.is_empty() {
                        continue;
                    }
                    out.extend_from_slice(part);
                    pad(&mut out, page);
                }
            }
        }
        Ok(out)
    }
}

fn page_align(size: usize, page: usize) -> usize {
    size.div_ceil(page) * page
}

fn pad(out: &mut Vec<u8>, page: usize) {
    out.resize(out.len().div_ceil(page) * page, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use abootimg_oxide::{HeaderV0, OsVersionPatch};

    /// A version 2 image round-trips, and its kernel sits at the page
    /// boundary after the header, where a bootloader reads it.
    #[test]
    fn a_v2_image_round_trips_with_its_kernel_page_aligned() {
        let header = HeaderV0 {
            kernel_size: 0,
            kernel_addr: 0x8000,
            ramdisk_size: 0,
            ramdisk_addr: 0x02000000,
            second_bootloader_size: 0,
            second_bootloader_addr: 0,
            tags_addr: 0x01e00000,
            page_size: 4096,
            osversionpatch: OsVersionPatch(0),
            board_name: [0; 16],
            cmdline: Box::new([0; 512 + 1024]),
            hash_digest: [0; 32],
            versioned: HeaderV0Versioned::V2 { recovery_dtbo_size: 0, recovery_dtbo_addr: 0, dtb_size: 0, dtb_addr: 0 },
        };
        let image = BootImage {
            header: BootHeader::Legacy(header),
            kernel: vec![0x4d, 0x5a, 0x00, 0x91, 0xff, 0x7f, 0x9c, 0x14],
            ramdisk: vec![1, 2, 3, 4, 5],
            second: Vec::new(),
            recovery_dtbo: Vec::new(),
            dtb: Vec::new(),
        }
        .to_bytes()
        .unwrap();

        let back = BootImage::parse(&image).unwrap();
        assert_eq!(back.header.header_version(), 2);
        assert_eq!(back.kernel, vec![0x4d, 0x5a, 0x00, 0x91, 0xff, 0x7f, 0x9c, 0x14], "the kernel comes back whole");
        assert_eq!(back.ramdisk, vec![1, 2, 3, 4, 5]);
        assert_eq!(&image[PAGE..PAGE + 4], &[0x4d, 0x5a, 0x00, 0x91], "the kernel starts where the bootloader reads it");
    }
}

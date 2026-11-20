pub const ACPI_ID: &str = "ELAN06FA";
pub const ACPI_COMPATIBLE: &str = "PNP0C50";
pub const VENDOR_ID: u16 = 0x04f3;
pub const PRODUCT_ID: u16 = 0x327e;
pub const DESCRIPTOR_LENGTH: usize = 675;
pub const DESCRIPTOR_DIGEST: u64 = 0x6339_8f68_4a18_7986;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unverified {
    Vendor(u16),
    Product(u16),
    Descriptor { length: usize, digest: u64 },
}

pub const fn digest(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut at = 0;
    while at < bytes.len() {
        hash ^= bytes[at] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        at += 1;
    }
    hash
}

pub fn verify(vendor: u16, product: u16, descriptor: &[u8]) -> Result<(), Unverified> {
    if vendor != VENDOR_ID {
        return Err(Unverified::Vendor(vendor));
    }
    if product != PRODUCT_ID {
        return Err(Unverified::Product(product));
    }
    let found = digest(descriptor);
    if descriptor.len() != DESCRIPTOR_LENGTH || found != DESCRIPTOR_DIGEST {
        return Err(Unverified::Descriptor { length: descriptor.len(), digest: found });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELAN: &[u8] = include_bytes!("../tests/data/elan-327e.rdesc");

    #[test]
    fn only_the_verified_pad_passes() {
        assert_eq!(verify(VENDOR_ID, PRODUCT_ID, ELAN), Ok(()));
        assert_eq!(verify(0x06cb, PRODUCT_ID, ELAN), Err(Unverified::Vendor(0x06cb)));
        assert_eq!(verify(VENDOR_ID, 0x3282, ELAN), Err(Unverified::Product(0x3282)));
        let mut changed = ELAN.to_vec();
        changed[100] ^= 1;
        assert!(matches!(verify(VENDOR_ID, PRODUCT_ID, &changed), Err(Unverified::Descriptor { length: 675, .. })));
        assert!(matches!(verify(VENDOR_ID, PRODUCT_ID, &ELAN[..674]), Err(Unverified::Descriptor { length: 674, .. })));
    }
}

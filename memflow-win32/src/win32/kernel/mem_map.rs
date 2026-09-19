use std::prelude::v1::*;

use log::{info, trace};
use memflow::mem::{MemoryMap, MemoryView};
use memflow::types::{mem, umem, Address};

#[allow(clippy::unnecessary_cast)]
const SIZE_4KB: u64 = mem::kb(4) as u64;
const DESCRIPTOR_READ_ATTEMPTS: usize = 3;

/// The maximum number of physical-memory runs accepted from Windows.
pub const PHYSICAL_MEMORY_MAX_RUNS: usize = 32;

pub(super) trait DescriptorWord {
    const BYTES: usize;

    fn decode(bytes: &[u8]) -> Option<u64>;
}

impl DescriptorWord for u32 {
    const BYTES: usize = size_of::<Self>();

    fn decode(bytes: &[u8]) -> Option<u64> {
        bytes
            .get(..Self::BYTES)?
            .try_into()
            .ok()
            .map(u32::from_le_bytes)
            .map(u64::from)
    }
}

impl DescriptorWord for u64 {
    const BYTES: usize = size_of::<Self>();

    fn decode(bytes: &[u8]) -> Option<u64> {
        bytes
            .get(..Self::BYTES)?
            .try_into()
            .ok()
            .map(u64::from_le_bytes)
    }
}

pub(super) fn parse<T: MemoryView, U: DescriptorWord>(
    virt_mem: &mut T,
    descriptor_ptr_ptr: Address,
) -> Option<MemoryMap<(Address, umem)>> {
    let descriptor_ptr = virt_mem.read_addr64(descriptor_ptr_ptr).ok()?.non_null()?;
    trace!("found phys_mem_block pointer at: {}", descriptor_ptr);

    let descriptor_size = U::BYTES * (2 + PHYSICAL_MEMORY_MAX_RUNS * 2);
    for attempt in 1..=DESCRIPTOR_READ_ATTEMPTS {
        let Ok(descriptor) = virt_mem.read_raw(descriptor_ptr, descriptor_size) else {
            info!("failed phys_mem_block read attempt {attempt}/{DESCRIPTOR_READ_ATTEMPTS}");
            continue;
        };
        if let Some(runs) = decode_descriptor::<U>(&descriptor) {
            let mut memory_map = MemoryMap::new();
            for (base, size) in runs {
                trace!("adding memory mapping: base={:x} size={:x}", base, size);
                memory_map.push_remap(base.into(), size as umem, Address::from(base));
            }
            return Some(memory_map);
        }
        info!("invalid phys_mem_block read attempt {attempt}/{DESCRIPTOR_READ_ATTEMPTS}");
    }
    None
}

fn decode_descriptor<U: DescriptorWord>(descriptor: &[u8]) -> Option<Vec<(u64, u64)>> {
    let number_of_runs = word::<U>(descriptor, 0)?;
    let number_of_pages = word::<U>(descriptor, 1)?;
    if number_of_runs == 0 || number_of_runs > PHYSICAL_MEMORY_MAX_RUNS as u64 {
        info!(
            "invalid number of memory segments in phys_mem_block: {} found, 1..={} expected",
            number_of_runs, PHYSICAL_MEMORY_MAX_RUNS
        );
        return None;
    }
    if number_of_pages == 0 {
        info!("phys_mem_block contains no physical pages");
        return None;
    }

    let mut runs = Vec::with_capacity(number_of_runs as usize);
    let mut decoded_pages = 0_u64;
    for index in 0..number_of_runs as usize {
        let base_page = word::<U>(descriptor, 2 + index * 2)?;
        let page_count = word::<U>(descriptor, 3 + index * 2)?;
        if page_count == 0 {
            info!("phys_mem_block run {index} contains no pages");
            return None;
        }
        decoded_pages = decoded_pages.checked_add(page_count)?;
        runs.push((
            base_page.checked_mul(SIZE_4KB)?,
            page_count.checked_mul(SIZE_4KB)?,
        ));
    }
    if decoded_pages != number_of_pages {
        info!(
            "phys_mem_block page count mismatch: header={}, runs={}",
            number_of_pages, decoded_pages
        );
        return None;
    }
    Some(runs)
}

fn word<U: DescriptorWord>(descriptor: &[u8], index: usize) -> Option<u64> {
    U::decode(descriptor.get(index.checked_mul(U::BYTES)?..)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_x64_descriptor_bytes() {
        let mut descriptor = vec![0; 0x210];
        descriptor[0..8].copy_from_slice(&2_u64.to_le_bytes());
        descriptor[8..16].copy_from_slice(&5_u64.to_le_bytes());
        descriptor[16..24].copy_from_slice(&1_u64.to_le_bytes());
        descriptor[24..32].copy_from_slice(&2_u64.to_le_bytes());
        descriptor[32..40].copy_from_slice(&0x100_u64.to_le_bytes());
        descriptor[40..48].copy_from_slice(&3_u64.to_le_bytes());

        assert_eq!(
            decode_descriptor::<u64>(&descriptor),
            Some(vec![(0x1000, 0x2000), (0x10_0000, 0x3000)])
        );
    }

    #[test]
    fn rejects_empty_and_inconsistent_descriptors() {
        assert_eq!(decode_descriptor::<u64>(&[0; 0x210]), None);

        let mut descriptor = vec![0; 0x210];
        descriptor[0..8].copy_from_slice(&1_u64.to_le_bytes());
        descriptor[8..16].copy_from_slice(&2_u64.to_le_bytes());
        descriptor[16..24].copy_from_slice(&1_u64.to_le_bytes());
        descriptor[24..32].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(decode_descriptor::<u64>(&descriptor), None);
    }
}

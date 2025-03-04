//! This file handles memory allocators initialization for the kernel.
//!
//! The physical memory is divided into zones. Each zones contains frames that
//! can be allocated by the buddy allocator
//!
//! The following zones exist:
//! - Kernel: Memory to be allocated by the kernel, shared accross processes. This zone requires
//! that every frames of virtual memory are associated with a unique physical
//! frame.
//! - MMIO: Memory used for Memory Mapped I/O. This zones requires only virtual memory, thus it
//! overlaps with the user zone which allocates the physical memory.
//! - User: Memory used for userspace mappings. This zone doesn't requires virtual memory to
//! correspond with the physical memory, thus it can be located outside of the
//! kernelspace.

use crate::memory;
use crate::memory::buddy;
use crate::memory::memmap;
use crate::util;
use crate::util::math;
use core::cmp::min;
use core::ffi::c_void;

/// Initializes the memory allocators.
pub fn init() {
	let mmap_info = memmap::get_info();

	// The pointer to the beginning of available memory
	let virt_alloc_begin = memory::kern_to_virt(mmap_info.phys_main_begin);
	// The number of available physical memory pages
	let mut available_pages = mmap_info.phys_main_pages;

	// The pointer to the beginning of the buddy allocator's metadata
	let metadata_begin = util::align(virt_alloc_begin, memory::PAGE_SIZE) as *mut c_void;
	// The size of the buddy allocator's metadata
	let metadata_size = available_pages * buddy::get_frame_metadata_size();
	// The end of the buddy allocator's metadata
	let metadata_end = unsafe { metadata_begin.add(metadata_size) };
	// The physical address of the end of the buddy allocator's metadata
	let phys_metadata_end = memory::kern_to_phys(metadata_end);

	// Updating the number of available pages
	available_pages -= math::ceil_div(metadata_size, memory::PAGE_SIZE);

	// The beginning of the kernel's zone
	let kernel_zone_begin = util::align(phys_metadata_end, memory::PAGE_SIZE) as *mut c_void;
	// The maximum number of pages the kernel zone can hold.
	let kernel_max =
		(memory::get_kernelspace_size() - phys_metadata_end as usize) / memory::PAGE_SIZE;
	// The number of frames the kernel zone holds.
	let kernel_zone_frames = min(available_pages, kernel_max);
	// The kernel's zone
	let kernel_zone = buddy::Zone::new(metadata_begin, kernel_zone_frames as _, kernel_zone_begin);

	// Updating the number of available pages
	available_pages -= kernel_zone_frames;

	// The beginning of the userspace's zone
	let userspace_zone_begin =
		unsafe { kernel_zone_begin.add(kernel_zone_frames * memory::PAGE_SIZE) };
	// The beginning of the userspace zone's metadata
	let userspace_metadata_begin =
		unsafe { metadata_begin.add(kernel_zone_frames * buddy::get_frame_metadata_size()) };
	let user_zone = buddy::Zone::new(
		userspace_metadata_begin,
		available_pages as _,
		userspace_zone_begin,
	);

	// TODO MMIO zone

	buddy::init([
		user_zone,
		unsafe { core::mem::zeroed() }, // TODO MMIO
		kernel_zone,
	]);
}

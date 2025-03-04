//! The `madvise` system call gives advices to the kernel about the usage of
//! memory in order to allow optimizations.

use crate::errno::Errno;
use core::ffi::c_int;
use core::ffi::c_void;
use macros::syscall;

#[syscall]
pub fn madvise(_addr: *mut c_void, _length: usize, _advice: c_int) -> Result<i32, Errno> {
	// TODO
	Ok(0)
}

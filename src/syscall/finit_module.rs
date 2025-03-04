//! The `finit_module` system call allows to load a module on the kernel.

use crate::errno;
use crate::errno::AllocError;
use crate::errno::Errno;
use crate::module;
use crate::module::Module;
use crate::process::mem_space::ptr::SyscallString;
use crate::process::Process;
use crate::util::io::IO;
use core::ffi::c_int;
use macros::syscall;

#[syscall]
pub fn finit_module(fd: c_int, _param_values: SyscallString, _flags: c_int) -> Result<i32, Errno> {
	if fd < 0 {
		return Err(errno!(EBADF));
	}

	let open_file_mutex = {
		let proc_mutex = Process::current_assert();
		let proc = proc_mutex.lock();

		if !proc.access_profile.is_privileged() {
			return Err(errno!(EPERM));
		}

		let fds_mutex = proc.get_fds().unwrap();
		let fds = fds_mutex.lock();

		fds.get_fd(fd as _)
			.ok_or_else(|| errno!(EBADF))?
			.get_open_file()
			.clone()
	};
	let image = {
		let mut open_file = open_file_mutex.lock();
		let len = open_file.get_size().try_into().map_err(|_| AllocError)?;
		let mut image = crate::vec![0u8; len]?;
		open_file.read(0, image.as_mut_slice())?;
		image
	};

	let module = Module::load(image.as_slice())?;
	if !module::is_loaded(module.get_name()) {
		module::add(module)?;
		Ok(0)
	} else {
		Err(errno!(EEXIST))
	}
}

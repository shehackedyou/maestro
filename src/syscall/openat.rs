//! The `openat` syscall allows to open a file.

use crate::errno::Errno;
use crate::file::File;
use crate::file::FileContent;
use crate::file::FileType;
use crate::file::Mode;
use crate::file::open_file;
use crate::file;
use crate::process::Process;
use crate::process::mem_space::ptr::SyscallString;
use crate::process::regs::Regs;
use crate::syscall::openat::open_file::FDTarget;
use crate::util::ptr::SharedPtr;
use super::util;

// TODO Implement all flags

/// Returns the file at the given path `path`.
/// TODO doc all args
/// If the file doesn't exist and the O_CREAT flag is set, the file is created, then the function
/// returns it. If the flag is not set, the function returns an error with the appropriate errno.
/// If the file is to be created, the function uses `mode` to set its permissions.
fn get_file(dirfd: i32, pathname: SyscallString, flags: i32, mode: Mode)
	-> Result<SharedPtr<File>, Errno> {
	// Tells whether to follow symbolic links on the last component of the path.
	let follow_links = flags & open_file::O_NOFOLLOW == 0;

	let proc_mutex = Process::get_current().unwrap();
	let proc_guard = proc_mutex.lock();
	let proc = proc_guard.get_mut();

	let mem_space = proc.get_mem_space().unwrap();
	let mem_space_guard = mem_space.lock();

	let pathname = pathname.get(&mem_space_guard)?.ok_or_else(|| errno!(EFAULT))?;

	if flags & open_file::O_CREAT != 0 {
		util::create_file_at(&proc_guard, follow_links, dirfd, pathname, mode,
			FileContent::Regular)
	} else {
		util::get_file_at(&proc_guard, true, dirfd, pathname, 0)
	}
}

/// The implementation of the `openat` syscall.
pub fn openat(regs: &Regs) -> Result<i32, Errno> {
	let dirfd = regs.ebx as i32;
	let pathname: SyscallString = (regs.ecx as usize).into();
	let flags = regs.edx as i32;
	let mode = regs.esi as file::Mode;

	// Getting the file
	let file = get_file(dirfd, pathname, flags, mode)?;

	// If O_DIRECTORY is set and the file is not a directory, return an error
	if flags & open_file::O_DIRECTORY != 0
		&& file.lock().get().get_file_type() != FileType::Directory {
		return Err(errno!(ENOTDIR));
	}

	// Create and return the file descriptor
	let mutex = Process::get_current().unwrap();
	let guard = mutex.lock();
	let proc = guard.get_mut();
	let fd = proc.create_fd(flags & super::open::STATUS_FLAGS_MASK, FDTarget::File(file))?;
	Ok(fd.get_id() as _)
}

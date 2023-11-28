//! The `lchown` system call changes the owner of a symbolic link file.

use crate::errno::Errno;
use crate::file::perm::{Gid, Uid};
use crate::process::mem_space::ptr::SyscallString;
use macros::syscall;

#[syscall]
pub fn lchown(pathname: SyscallString, owner: Uid, group: Gid) -> EResult<i32> {
	super::chown::do_chown(pathname, owner, group, false)
}

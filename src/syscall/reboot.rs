//! The `reboot` system call allows the superuser to power off, reboot, halt or
//! suspend the system.

use crate::errno::Errno;
use crate::process::Process;
use crate::{errno, power};
use core::ffi::c_int;
use core::ffi::c_void;
use macros::syscall;

/// First magic number.
const MAGIC: u32 = 0xde145e83;
/// Second magic number.
const MAGIC2: u32 = 0x40367d6e;

/// Command to power off the system.
const CMD_POWEROFF: u32 = 0;
/// Command to reboot the system.
const CMD_REBOOT: u32 = 1;
/// Command to halt the system.
const CMD_HALT: u32 = 2;
/// Command to suspend the system.
const CMD_SUSPEND: u32 = 3;

#[syscall]
pub fn reboot(magic: c_int, magic2: c_int, cmd: c_int, _arg: *const c_void) -> Result<i32, Errno> {
	if (magic as u32) != MAGIC || (magic2 as u32) != MAGIC2 {
		return Err(errno!(EINVAL));
	}

	{
		let proc_mutex = Process::current_assert();
		let proc = proc_mutex.lock();
		if !proc.access_profile.is_privileged() {
			return Err(errno!(EPERM));
		}
	}

	match cmd as u32 {
		CMD_POWEROFF => {
			crate::println!("Power down...");
			power::shutdown();
		}
		CMD_REBOOT => {
			crate::println!("Rebooting...");
			power::reboot();
		}
		CMD_HALT => {
			crate::println!("Halting...");
			power::halt();
		}
		CMD_SUSPEND => {
			// TODO Use ACPI to suspend the system
			todo!()
		}
		_ => Err(errno!(EINVAL)),
	}
}

//! Files implementation.
//!
//! The kernel allows *mounting* several filesystems together, which are all unified into one
//! filesystem called the VFS (Virtual FileSystem).
//!
//! The root filesystem is passed to the kernel as an argument on boot.
//! Other filesystems are mounted into subdirectories.

pub mod blocking;
pub mod buffer;
pub mod fd;
pub mod fs;
pub mod mapping;
pub mod mountpoint;
pub mod open_file;
pub mod path;
pub mod perm;
pub mod util;
pub mod vfs;

use crate::device;
use crate::device::DeviceID;
use crate::device::DeviceType;
use crate::errno;
use crate::errno::EResult;
use crate::errno::Errno;
use crate::file::buffer::pipe::PipeBuffer;
use crate::file::buffer::socket::Socket;
use crate::file::fs::Filesystem;
use crate::file::perm::Gid;
use crate::file::perm::Uid;
use crate::process::mem_space::MemSpace;
use crate::syscall::ioctl;
use crate::time::clock;
use crate::time::clock::CLOCK_MONOTONIC;
use crate::time::unit::Timestamp;
use crate::time::unit::TimestampScale;
use crate::util::container::hashmap::HashMap;
use crate::util::container::string::String;
use crate::util::io::IO;
use crate::util::lock::IntMutex;
use crate::util::lock::Mutex;
use crate::util::ptr::arc::Arc;
use crate::util::TryClone;
use core::cmp::max;
use core::ffi::c_void;
use mountpoint::MountPoint;
use mountpoint::MountSource;
use path::Path;
use perm::AccessProfile;

/// Type representing an inode.
///
/// An inode is a number representing a node in a filesystem. The kernel doesn't interpret this
/// value in an ways, but it must fulfill one condition: the value must represent a **unique**
/// node in the filesystem, and that exact node **must** be accessible using this value. and
pub type INode = u64;
/// Type representing a file mode, which is a pair of values representing respectively:
/// - UNIX type (regular, directory, etc...)
/// - UNIX permissions (read, write, execute, etc...)
pub type Mode = u32;

/// File type: socket
pub const S_IFSOCK: Mode = 0o140000;
/// File type: symbolic link
pub const S_IFLNK: Mode = 0o120000;
/// File type: regular file
pub const S_IFREG: Mode = 0o100000;
/// File type: block device
pub const S_IFBLK: Mode = 0o060000;
/// File type: directory
pub const S_IFDIR: Mode = 0o040000;
/// File type: character device
pub const S_IFCHR: Mode = 0o020000;
/// File type: FIFO
pub const S_IFIFO: Mode = 0o010000;

/// Directory entry type: Block Device
pub const DT_BLK: u8 = 6;
/// Directory entry type: Char Device
pub const DT_CHR: u8 = 2;
/// Directory entry type: Directory
pub const DT_DIR: u8 = 4;
/// Directory entry type: FIFO
pub const DT_FIFO: u8 = 1;
/// Directory entry type: Symbolic Link
pub const DT_LNK: u8 = 10;
/// Directory entry type: Regular file
pub const DT_REG: u8 = 8;
/// Directory entry type: Socket
pub const DT_SOCK: u8 = 12;
/// Directory entry type: Unknown
pub const DT_UNKNOWN: u8 = 0;

/// Enumeration representing the different file types.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileType {
	/// A regular file storing data.
	Regular,
	/// A directory, containing other files.
	Directory,
	/// A symbolic link, pointing to another file.
	Link,
	/// A named pipe.
	Fifo,
	/// A Unix domain socket.
	Socket,
	/// A Block device file.
	BlockDevice,
	/// A Character device file.
	CharDevice,
}

impl FileType {
	/// Returns the type corresponding to the given mode `mode`.
	///
	/// If the type doesn't exist, the function returns `None`.
	pub fn from_mode(mode: Mode) -> Option<Self> {
		match mode & 0o770000 {
			S_IFSOCK => Some(Self::Socket),
			S_IFLNK => Some(Self::Link),
			S_IFREG | 0 => Some(Self::Regular),
			S_IFBLK => Some(Self::BlockDevice),
			S_IFDIR => Some(Self::Directory),
			S_IFCHR => Some(Self::CharDevice),
			S_IFIFO => Some(Self::Fifo),

			_ => None,
		}
	}

	/// Returns the mode corresponding to the type.
	pub fn to_mode(&self) -> Mode {
		match self {
			Self::Socket => S_IFSOCK,
			Self::Link => S_IFLNK,
			Self::Regular => S_IFREG,
			Self::BlockDevice => S_IFBLK,
			Self::Directory => S_IFDIR,
			Self::CharDevice => S_IFCHR,
			Self::Fifo => S_IFIFO,
		}
	}

	/// Returns the directory entry type.
	pub fn to_dirent_type(&self) -> u8 {
		match self {
			Self::Socket => DT_SOCK,
			Self::Link => DT_LNK,
			Self::Regular => DT_REG,
			Self::BlockDevice => DT_BLK,
			Self::Directory => DT_DIR,
			Self::CharDevice => DT_CHR,
			Self::Fifo => DT_FIFO,
		}
	}
}

/// The location of a file on a disk.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum FileLocation {
	/// The file is located on a filesystem.
	Filesystem {
		/// The ID of the mountpoint of the file.
		mountpoint_id: u32,
		/// The file's inode.
		inode: INode,
	},

	/// The file is not located on a filesystem.
	Virtual {
		/// The ID of the file.
		id: u32,
	},
}

impl FileLocation {
	/// Returns the ID of the mountpoint.
	pub fn get_mountpoint_id(&self) -> Option<u32> {
		match self {
			Self::Filesystem {
				mountpoint_id, ..
			} => Some(*mountpoint_id),

			_ => None,
		}
	}

	/// Returns the mountpoint.
	pub fn get_mountpoint(&self) -> Option<Arc<Mutex<MountPoint>>> {
		mountpoint::from_id(self.get_mountpoint_id()?)
	}

	/// Returns the inode.
	pub fn get_inode(&self) -> INode {
		match self {
			Self::Filesystem {
				inode, ..
			} => *inode,

			Self::Virtual {
				id,
			} => *id as _,
		}
	}
}

/// Structure representing a directory entry.
#[derive(Clone, Debug)]
pub struct DirEntry {
	/// The entry's inode.
	pub inode: INode,
	/// The entry's type.
	pub entry_type: FileType,
}

/// Enumeration of all possible file contents for each file types.
#[derive(Debug)]
pub enum FileContent {
	/// The file is a regular file.
	Regular,
	/// The file is a directory.
	///
	/// The hashmap contains the list of entries. The key is the name of the entry and the value
	/// is the entry itself.
	Directory(HashMap<String, DirEntry>),
	/// The file is a link. The data is the link's target.
	Link(String),
	/// The file is a FIFO.
	Fifo,
	/// The file is a socket.
	Socket,

	/// The file is a block device.
	BlockDevice { major: u32, minor: u32 },

	/// The file is a char device.
	CharDevice { major: u32, minor: u32 },
}

impl FileContent {
	/// Returns the file type associated with the content type.
	pub fn as_type(&self) -> FileType {
		match self {
			Self::Regular => FileType::Regular,
			Self::Directory(_) => FileType::Directory,
			Self::Link(_) => FileType::Link,
			Self::Fifo => FileType::Fifo,
			Self::Socket => FileType::Socket,
			Self::BlockDevice {
				..
			} => FileType::BlockDevice,
			Self::CharDevice {
				..
			} => FileType::CharDevice,
		}
	}
}

impl TryClone for FileContent {
	fn try_clone(&self) -> Result<Self, Self::Error> {
		Ok(match self {
			Self::Regular => Self::Regular,
			Self::Directory(entries) => Self::Directory(entries.try_clone()?),
			Self::Link(path) => Self::Link(path.try_clone()?),
			Self::Fifo => Self::Fifo,
			Self::Socket => Self::Socket,

			Self::BlockDevice {
				major,
				minor,
			} => Self::BlockDevice {
				major: *major,
				minor: *minor,
			},

			Self::CharDevice {
				major,
				minor,
			} => Self::CharDevice {
				major: *major,
				minor: *minor,
			},
		})
	}
}

/// Structure representing a file.
#[derive(Debug)]
pub struct File {
	/// The name of the file.
	name: String,
	/// The path of the file's parent.
	parent_path: Path,

	/// The number of hard links associated with the file.
	hard_links_count: u16,

	/// The number of blocks allocated on the disk for the file.
	pub blocks_count: u64,
	/// The size of the file in bytes.
	size: u64,

	/// The ID of the owner user.
	uid: Uid,
	/// The ID of the owner group.
	gid: Gid,
	/// The mode of the file.
	mode: Mode,

	/// Timestamp of the last modification of the metadata.
	pub ctime: Timestamp,
	/// Timestamp of the last modification of the file's content.
	pub mtime: Timestamp,
	/// Timestamp of the last access to the file.
	pub atime: Timestamp,

	/// The location the file is stored on.
	location: FileLocation,
	/// The content of the file.
	content: FileContent,

	/// Tells whether remove has been deferred for the file. If `true`, then the file will be
	/// removed when the file is no longer used.
	deferred_remove: bool,
	/// Tells whether the file has been removed.
	removed: bool,
}

impl File {
	/// Creates a new instance.
	///
	/// Arguments:
	/// - `name` is the name of the file.
	/// - `uid` is the id of the owner user.
	/// - `gid` is the id of the owner group.
	/// - `mode` is the permission of the file.
	/// - `location` is the location of the file.
	/// - `content` is the content of the file. This value also determines the
	/// file type.
	fn new(
		name: String,
		uid: Uid,
		gid: Gid,
		mode: Mode,
		location: FileLocation,
		content: FileContent,
	) -> Result<Self, Errno> {
		let timestamp = clock::current_time(CLOCK_MONOTONIC, TimestampScale::Second).unwrap_or(0);

		Ok(Self {
			name,
			parent_path: Path::root(),

			hard_links_count: 1,

			blocks_count: 0,
			size: 0,

			uid,
			gid,
			mode,

			ctime: timestamp,
			mtime: timestamp,
			atime: timestamp,

			location,
			content,

			deferred_remove: false,
			removed: false,
		})
	}

	/// Returns the name of the file.
	pub fn get_name(&self) -> &String {
		&self.name
	}

	/// Returns the absolute path of the file's parent.
	pub fn get_parent_path(&self) -> &Path {
		&self.parent_path
	}

	/// Returns the absolute path of the file.
	pub fn get_path(&self) -> Result<Path, Errno> {
		let mut parent_path = self.parent_path.try_clone()?;
		if !self.name.is_empty() {
			parent_path.push(self.name.try_clone()?)?;
		}

		Ok(parent_path)
	}

	/// Sets the file's parent path.
	///
	/// If the path isn't absolute, the behaviour is undefined.
	pub fn set_parent_path(&mut self, parent_path: Path) {
		self.parent_path = parent_path;
	}

	/// Returns the file's mode.
	pub fn get_mode(&self) -> Mode {
		self.mode | self.content.as_type().to_mode()
	}

	/// Returns the permissions of the file.
	pub fn get_permissions(&self) -> Mode {
		self.mode & 0o7777
	}

	/// Sets the permissions of the file.
	pub fn set_permissions(&mut self, mode: Mode) {
		self.mode = mode & 0o7777;

		let timestamp = clock::current_time(CLOCK_MONOTONIC, TimestampScale::Second).unwrap_or(0);
		self.ctime = timestamp;
	}

	/// Returns an immutable reference to the location at which the file is
	/// stored.
	pub fn get_location(&self) -> &FileLocation {
		&self.location
	}

	/// Returns the number of hard links.
	pub fn get_hard_links_count(&self) -> u16 {
		self.hard_links_count
	}

	/// Sets the number of hard links.
	pub fn set_hard_links_count(&mut self, count: u16) {
		self.hard_links_count = count;

		let timestamp = clock::current_time(CLOCK_MONOTONIC, TimestampScale::Second).unwrap_or(0);
		self.ctime = timestamp;
	}

	/// Sets the file's size.
	pub fn set_size(&mut self, size: u64) {
		self.size = size;
	}

	/// Returns the owner user ID.
	pub fn get_uid(&self) -> Uid {
		self.uid
	}

	/// Sets the owner user ID.
	pub fn set_uid(&mut self, uid: Uid) {
		self.uid = uid;

		let timestamp = clock::current_time(CLOCK_MONOTONIC, TimestampScale::Second).unwrap_or(0);
		self.ctime = timestamp;
	}

	/// Returns the owner group ID.
	pub fn get_gid(&self) -> Gid {
		self.gid
	}

	/// Sets the owner group ID.
	pub fn set_gid(&mut self, gid: Gid) {
		self.gid = gid;

		let timestamp = clock::current_time(CLOCK_MONOTONIC, TimestampScale::Second).unwrap_or(0);
		self.ctime = timestamp;
	}

	/// Tells whether the directory is empty or not.
	///
	/// If the current file isn't a directory, the function returns an error.
	pub fn is_empty_directory(&self) -> Result<bool, Errno> {
		if let FileContent::Directory(entries) = &self.content {
			Ok(entries.is_empty())
		} else {
			Err(errno!(ENOTDIR))
		}
	}

	/// Adds the directory entry `entry` to the current directory's entries.
	///
	/// Arguments:
	/// - `name` is the name of the entry.
	///
	/// If the current file isn't a directory, the function returns an error.
	pub fn add_entry(&mut self, name: String, entry: DirEntry) -> Result<(), Errno> {
		if let FileContent::Directory(entries) = &mut self.content {
			entries.insert(name, entry)?;
			Ok(())
		} else {
			Err(errno!(ENOTDIR))
		}
	}

	/// Removes the file with name `name` from the current file's entries.
	///
	/// If the current file isn't a directory, the function returns an error.
	pub fn remove_entry(&mut self, name: &String) -> Result<(), Errno> {
		if let FileContent::Directory(entries) = &mut self.content {
			entries.remove(name);
			Ok(())
		} else {
			Err(errno!(ENOTDIR))
		}
	}

	/// Creates a directory entry corresponding to the current file.
	pub fn as_dir_entry(&self) -> DirEntry {
		DirEntry {
			inode: self.location.get_inode(),
			entry_type: self.get_type(),
		}
	}

	/// Returns the file's content.
	pub fn get_content(&self) -> &FileContent {
		&self.content
	}

	/// Returns the type of the file.
	pub fn get_type(&self) -> FileType {
		self.content.as_type()
	}

	/// Performs an ioctl operation on the file.
	///
	/// Arguments:
	/// - `mem_space` is the memory space on which pointers are to be dereferenced.
	/// - `request` is the ID of the request to perform.
	/// - `argp` is a pointer to the argument.
	pub fn ioctl(
		&mut self,
		mem_space: Arc<IntMutex<MemSpace>>,
		request: ioctl::Request,
		argp: *const c_void,
	) -> Result<u32, Errno> {
		match &self.content {
			FileContent::Fifo => {
				let buff_mutex = buffer::get_or_default::<PipeBuffer>(self.get_location())?;
				let mut buff = buff_mutex.lock();

				buff.ioctl(mem_space, request, argp)
			}

			FileContent::Socket => {
				let buff_mutex = buffer::get_or_default::<Socket>(self.get_location())?;
				let mut buff = buff_mutex.lock();

				buff.ioctl(mem_space, request, argp)
			}

			FileContent::BlockDevice {
				major,
				minor,
			} => {
				let dev_mutex = device::get(&DeviceID {
					type_: DeviceType::Block,
					major: *major,
					minor: *minor,
				})
				.ok_or_else(|| errno!(ENODEV))?;

				let mut dev = dev_mutex.lock();
				dev.get_handle().ioctl(mem_space, request, argp)
			}

			FileContent::CharDevice {
				major,
				minor,
			} => {
				let dev_mutex = device::get(&DeviceID {
					type_: DeviceType::Char,
					major: *major,
					minor: *minor,
				})
				.ok_or_else(|| errno!(ENODEV))?;

				let mut dev = dev_mutex.lock();
				dev.get_handle().ioctl(mem_space, request, argp)
			}

			_ => Err(errno!(ENOTTY)),
		}
	}

	/// Synchronizes the file with the device.
	///
	/// If no device is associated with the file, the function does nothing.
	pub fn sync(&self) -> Result<(), Errno> {
		if let Some(mountpoint_mutex) = self.location.get_mountpoint() {
			let mountpoint = mountpoint_mutex.lock();

			let io_mutex = mountpoint.get_source().get_io()?;
			let mut io = io_mutex.lock();

			let fs_mutex = mountpoint.get_filesystem();
			let mut fs = fs_mutex.lock();

			fs.update_inode(&mut *io, self)
		} else {
			Ok(())
		}
	}

	/// Wrapper for I/O operations on files.
	///
	/// For the current file, the function takes a closure which provides the following arguments:
	/// - The I/O interface to write the file, if any.
	/// - The filesystem of the file, if any.
	fn io_op<R, F>(&self, f: F) -> Result<R, Errno>
	where
		F: FnOnce(
			Option<Arc<Mutex<dyn IO>>>,
			Option<(Arc<Mutex<dyn Filesystem>>, INode)>,
		) -> Result<R, Errno>,
	{
		match &self.content {
			FileContent::Regular => match self.location {
				FileLocation::Filesystem {
					inode, ..
				} => {
					let (io, fs) = {
						let mountpoint_mutex =
							self.location.get_mountpoint().ok_or_else(|| errno!(EIO))?;
						let mountpoint = mountpoint_mutex.lock();

						let io = mountpoint.get_source().get_io()?;
						let fs = mountpoint.get_filesystem();

						(io, fs)
					};

					f(Some(io), Some((fs, inode)))
				}

				FileLocation::Virtual {
					..
				} => {
					let io = buffer::get(&self.location).map(|io| io as _);
					f(io, None)
				}
			},

			FileContent::Directory(_) => Err(errno!(EISDIR)),

			FileContent::Link(_) => Err(errno!(EINVAL)),

			FileContent::Fifo => {
				let io = buffer::get_or_default::<PipeBuffer>(self.get_location())?;
				f(Some(io as _), None)
			}

			FileContent::Socket => {
				let io = buffer::get_or_default::<Socket>(self.get_location())?;
				f(Some(io as _), None)
			}

			FileContent::BlockDevice {
				major,
				minor,
			} => {
				let io = device::get(&DeviceID {
					type_: DeviceType::Block,
					major: *major,
					minor: *minor,
				})
				.ok_or_else(|| errno!(ENODEV))?;

				f(Some(io as _), None)
			}

			FileContent::CharDevice {
				major,
				minor,
			} => {
				let io = device::get(&DeviceID {
					type_: DeviceType::Char,
					major: *major,
					minor: *minor,
				})
				.ok_or_else(|| errno!(ENODEV))?;

				f(Some(io as _), None)
			}
		}
	}

	/// Defers removal of the file, meaning the file will be removed when closed.
	pub fn defer_remove(&mut self) {
		self.deferred_remove = true;
	}

	/// Closes the file, removing it if removal has been deferred.
	pub fn close(mut self) -> EResult<()> {
		if !self.deferred_remove {
			return Ok(());
		}
		vfs::remove_file(&mut self, &AccessProfile::KERNEL)?;
		self.removed = true;
		Ok(())
	}
}

impl Drop for File {
	/// This function is used in case removal of the file has been deferred, but `close` has not
	/// been called.
	fn drop(&mut self) {
		if !self.deferred_remove || self.removed {
			return;
		}
		let _ = vfs::remove_file(self, &AccessProfile::KERNEL);
	}
}

impl AccessProfile {
	fn check_read_access_impl(uid: Uid, gid: Gid, file: &File) -> bool {
		// If root, bypass checks
		if uid == perm::ROOT_UID || gid == perm::ROOT_GID {
			return true;
		}

		if file.mode & perm::S_IRUSR != 0 && file.uid == uid {
			return true;
		}
		if file.mode & perm::S_IRGRP != 0 && file.gid == gid {
			return true;
		}
		file.mode & perm::S_IROTH != 0
	}

	/// Tells whether the agent can read the file.
	///
	/// `effective` tells whether to use effective IDs. If not, real IDs are used.
	pub fn check_read_access(&self, file: &File, effective: bool) -> bool {
		let (uid, gid) = if effective {
			(self.get_euid(), self.get_egid())
		} else {
			(self.get_uid(), self.get_gid())
		};
		Self::check_read_access_impl(uid, gid, file)
	}

	/// Tells whether the agent can read the file.
	///
	/// This function is the preferred from `check_read_access` for general cases.
	pub fn can_read_file(&self, file: &File) -> bool {
		self.check_read_access(file, true)
	}

	/// Tells whether the agent can list files of the directories, **not** including access to
	/// files' contents and metadata.
	#[inline]
	pub fn can_list_directory(&self, file: &File) -> bool {
		self.can_read_file(file)
	}

	fn check_write_access_impl(uid: Uid, gid: Gid, file: &File) -> bool {
		// If root, bypass checks
		if uid == perm::ROOT_UID || gid == perm::ROOT_GID {
			return true;
		}

		if file.mode & perm::S_IWUSR != 0 && file.uid == uid {
			return true;
		}
		if file.mode & perm::S_IWGRP != 0 && file.gid == gid {
			return true;
		}
		file.mode & perm::S_IWOTH != 0
	}

	/// Tells whether the agent can write the file.
	///
	/// `effective` tells whether to use effective IDs. If not, real IDs are used.
	pub fn check_write_access(&self, file: &File, effective: bool) -> bool {
		let (uid, gid) = if effective {
			(self.get_euid(), self.get_egid())
		} else {
			(self.get_uid(), self.get_gid())
		};
		Self::check_write_access_impl(uid, gid, file)
	}

	/// Tells whether the agent can write the file.
	pub fn can_write_file(&self, file: &File) -> bool {
		self.check_write_access(file, true)
	}

	/// Tells whether the agent can modify entries in the directory, including creating files,
	/// deleting files, and renaming files.
	#[inline]
	pub fn can_write_directory(&self, file: &File) -> bool {
		self.can_write_file(file) && self.can_execute_file(file)
	}

	fn check_execute_access_impl(uid: Uid, gid: Gid, file: &File) -> bool {
		// If root, bypass checks (unless the file is a regular file)
		if !matches!(file.content, FileContent::Regular)
			&& (uid == perm::ROOT_UID || gid == perm::ROOT_GID)
		{
			return true;
		}

		if file.mode & perm::S_IXUSR != 0 && file.uid == uid {
			return true;
		}
		if file.mode & perm::S_IXGRP != 0 && file.gid == gid {
			return true;
		}
		file.mode & perm::S_IXOTH != 0
	}

	/// Tells whether the agent can execute the file.
	///
	/// `effective` tells whether to use effective IDs. If not, real IDs are used.
	pub fn check_execute_access(&self, file: &File, effective: bool) -> bool {
		let (uid, gid) = if effective {
			(self.get_euid(), self.get_egid())
		} else {
			(self.get_uid(), self.get_gid())
		};
		Self::check_execute_access_impl(uid, gid, file)
	}

	/// Tells whether the agent can execute the file.
	pub fn can_execute_file(&self, file: &File) -> bool {
		self.check_execute_access(file, true)
	}

	/// Tells whether the agent can access files of the directory *if the name of the file is
	/// known*.
	#[inline]
	pub fn can_search_directory(&self, file: &File) -> bool {
		self.can_execute_file(file)
	}

	/// Tells whether the agent can set permissions for the given file.
	pub fn can_set_file_permissions(&self, file: &File) -> bool {
		let euid = self.get_euid();
		euid == perm::ROOT_UID || euid == file.get_uid()
	}
}

impl IO for File {
	fn get_size(&self) -> u64 {
		self.size
	}

	fn read(&mut self, off: u64, buff: &mut [u8]) -> Result<(u64, bool), Errno> {
		self.io_op(|io, fs| {
			let Some(io_mutex) = io else {
				return Ok((0, true));
			};
			let mut io = io_mutex.lock();

			if let Some((fs_mutex, inode)) = fs {
				let mut fs = fs_mutex.lock();
				let len = fs.read_node(&mut *io, inode, off, buff)?;
				let eof = off + len >= self.size;
				Ok((len, eof))
			} else {
				io.read(off, buff)
			}
		})
	}

	fn write(&mut self, off: u64, buff: &[u8]) -> Result<u64, Errno> {
		let len = self.io_op(|io, fs| {
			let Some(io_mutex) = io else {
				return Ok(0);
			};
			let mut io = io_mutex.lock();

			if let Some((fs_mutex, inode)) = fs {
				let mut fs = fs_mutex.lock();
				fs.write_node(&mut *io, inode, off, buff)?;
				Ok(buff.len() as _)
			} else {
				io.write(off, buff)
			}
		})?;
		// Update file's size
		self.size = max(off + len, self.size);
		Ok(len)
	}

	fn poll(&mut self, mask: u32) -> Result<u32, Errno> {
		self.io_op(|io, _| {
			let Some(io_mutex) = io else {
				return Ok(0);
			};

			let mut io = io_mutex.lock();
			io.poll(mask)
		})
	}
}

/// Initializes files management.
///
/// `root` is the set of major and minor numbers of the root device. If `None`, a tmpfs is used.
pub fn init(root: Option<(u32, u32)>) -> Result<(), Errno> {
	fs::register_defaults()?;

	// Create the root mountpoint
	let mount_source = match root {
		Some((major, minor)) => MountSource::Device {
			dev_type: DeviceType::Block,

			major,
			minor,
		},

		None => MountSource::NoDev(String::try_from(b"tmpfs")?),
	};
	mountpoint::create(mount_source, None, 0, Path::root())?;

	Ok(())
}

/// Tells whether files management has been initialized.
pub fn is_init() -> bool {
	!mountpoint::MOUNT_POINTS.lock().is_empty()
}

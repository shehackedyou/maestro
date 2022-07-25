//! This module handles filesystems. Every filesystems are unified by the Virtual FileSystem (VFS).
//! The root filesystem is passed to the kernel as an argument when booting. Other filesystems are
//! mounted into subdirectories.

pub mod fcache;
pub mod fd;
pub mod fs;
pub mod mountpoint;
pub mod open_file;
pub mod path;
pub mod pipe;
pub mod socket;
pub mod util;

use core::cmp::max;
use core::ffi::c_void;
use crate::device::DeviceType;
use crate::device;
use crate::errno::Errno;
use crate::errno;
use crate::file::fcache::FCache;
use crate::file::mountpoint::MountPoint;
use crate::file::mountpoint::MountSource;
use crate::process::mem_space::MemSpace;
use crate::time::unit::Timestamp;
use crate::time::unit::TimestampScale;
use crate::time;
use crate::util::FailableClone;
use crate::util::container::hashmap::HashMap;
use crate::util::container::string::String;
use crate::util::io::IO;
use crate::util::ptr::IntSharedPtr;
use crate::util::ptr::SharedPtr;
use path::Path;

/// Type representing a user ID.
pub type Uid = u16;
/// Type representing a group ID.
pub type Gid = u16;
/// Type representing a file mode.
pub type Mode = u32;

/// Type representing an inode.
pub type INode = u64;

/// The root user ID.
pub const ROOT_UID: Uid = 0;
/// The root group ID.
pub const ROOT_GID: Gid = 0;

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

/// User: Read, Write and Execute.
pub const S_IRWXU: Mode = 0o0700;
/// User: Read.
pub const S_IRUSR: Mode = 0o0400;
/// User: Write.
pub const S_IWUSR: Mode = 0o0200;
/// User: Execute.
pub const S_IXUSR: Mode = 0o0100;
/// Group: Read, Write and Execute.
pub const S_IRWXG: Mode = 0o0070;
/// Group: Read.
pub const S_IRGRP: Mode = 0o0040;
/// Group: Write.
pub const S_IWGRP: Mode = 0o0020;
/// Group: Execute.
pub const S_IXGRP: Mode = 0o0010;
/// Other: Read, Write and Execute.
pub const S_IRWXO: Mode = 0o0007;
/// Other: Read.
pub const S_IROTH: Mode = 0o0004;
/// Other: Write.
pub const S_IWOTH: Mode = 0o0002;
/// Other: Execute.
pub const S_IXOTH: Mode = 0o0001;
/// Setuid.
pub const S_ISUID: Mode = 0o4000;
/// Setgid.
pub const S_ISGID: Mode = 0o2000;
/// Sticky bit.
pub const S_ISVTX: Mode = 0o1000;

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
	/// If the type doesn't exist, the function returns None.
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

/// Structure representing the location of a file on a disk.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FileLocation {
	/// The ID of the mountpoint of the file.
	pub mountpoint_id: Option<u32>,

	/// The file's inode.
	pub inode: INode,
}

impl FileLocation {
	/// Returns the mountpoint associated with the file's location.
	pub fn get_mountpoint(&self) -> Option<SharedPtr<MountPoint>> {
		mountpoint::from_id(self.mountpoint_id?)
	}
}

/// Structure representing a directory entry.
#[derive(Debug)]
pub struct DirEntry {
	/// The entry's inode.
	pub inode: INode,
	/// The entry's type.
	pub entry_type: FileType,
}

impl FailableClone for DirEntry {
	fn failable_clone(&self) -> Result<Self, Errno> {
		Ok(Self {
			inode: self.inode,
			entry_type: self.entry_type,
		})
	}
}

/// Enumeration of all possible file contents for each file types.
#[derive(Debug)]
pub enum FileContent {
	/// The file is a regular file. No data.
	Regular,
	/// The file is a directory. The hashmap contains the list of entries. The key is the name of
	/// the entry and the value is the entry itself.
	Directory(HashMap<String, DirEntry>),
	/// The file is a link. The data is the link's target.
	Link(String),
	/// The file is a FIFO.
	Fifo,
	/// The file is a socket.
	Socket,

	/// The file is a block device.
	BlockDevice {
		major: u32,
		minor: u32,
	},

	/// The file is a char device.
	CharDevice {
		major: u32,
		minor: u32,
	},
}

impl FileContent {
	/// Returns the file type associated with the content type.
	pub fn get_file_type(&self) -> FileType {
		match self {
			Self::Regular => FileType::Regular,
			Self::Directory(_) => FileType::Directory,
			Self::Link(_) => FileType::Link,
			Self::Fifo => FileType::Fifo,
			Self::Socket => FileType::Socket,
			Self::BlockDevice { .. } => FileType::BlockDevice,
			Self::CharDevice { .. } => FileType::CharDevice,
		}
	}
}

impl FailableClone for FileContent {
	fn failable_clone(&self) -> Result<Self, Errno> {
		let s = match self {
			Self::Regular => Self::Regular,
			Self::Directory(entries) => Self::Directory(entries.failable_clone()?),
			Self::Link(path) => Self::Link(path.failable_clone()?),
			Self::Fifo => Self::Fifo,
			Self::Socket => Self::Socket,

			Self::BlockDevice { major, minor } => Self::BlockDevice {
				major: *major,
				minor: *minor,
			},

			Self::CharDevice { major, minor } => Self::CharDevice {
				major: *major,
				minor: *minor,
			},
		};

		Ok(s)
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
	blocks_count: u64,
	/// The size of the file in bytes.
	size: u64,

	/// The ID of the owner user.
	uid: Uid,
	/// The ID of the owner group.
	gid: Gid,
	/// The mode of the file.
	mode: Mode,

	/// Timestamp of the last modification of the metadata.
	ctime: Timestamp,
	/// Timestamp of the last modification of the file.
	mtime: Timestamp,
	/// Timestamp of the last access to the file.
	atime: Timestamp,

	/// The location the file is stored on.
	location: FileLocation,
	/// The content of the file.
	content: FileContent,

	/// The number of locations where this file being is used. If non-zero, the file is considered
	/// busy.
	ref_count: usize,
}

impl File {
	/// Creates a new instance.
	/// `name` is the name of the file.
	/// `uid` is the id of the owner user.
	/// `gid` is the id of the owner group.
	/// `mode` is the permission of the file.
	/// `location` is the location of the file.
	/// `content` is the content of the file. This value also determines the file type.
	fn new(name: String, uid: Uid, gid: Gid, mut mode: Mode, location: FileLocation,
		mut content: FileContent) -> Result<Self, Errno> {
		let timestamp = time::get(TimestampScale::Second, true).unwrap_or(0);

		match &mut content {
			// If the file is a directory that doesn't contain `.` and `..` entries, add them
			FileContent::Directory(entries) => {
				// Add `.`
				let name = String::from(b".")?;
				if entries.get(&name).is_none() {
					entries.insert(name, DirEntry {
						inode: location.inode,
						entry_type: FileType::Directory,
					})?;
				}

				// TODO Add `..`
			},

			// If the file is a symbolic link, permissions don't matter
			FileContent::Link(_) => {
				mode = 0o777;
			},

			_ => {},
		}

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

			ref_count: 0,
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
		let mut parent_path = self.parent_path.failable_clone()?;
		parent_path.push(self.name.failable_clone()?)?;

		Ok(parent_path)
	}

	/// Sets the file's parent path.
	/// If the path isn't absolute, the behaviour is undefined.
	pub fn set_parent_path(&mut self, parent_path: Path) {
		self.parent_path = parent_path;
	}

	/// Returns the type of the file.
	pub fn get_file_type(&self) -> FileType {
		self.content.get_file_type()
	}

	/// Returns the file's mode.
	pub fn get_mode(&self) -> Mode {
		self.mode | self.content.get_file_type().to_mode()
	}

	/// Tells if the file can be read from by the given UID and GID.
	pub fn can_read(&self, uid: Uid, gid: Gid) -> bool {
		// If root, bypass checks
		if uid == ROOT_UID || gid == ROOT_GID {
			return true;
		}

		if self.mode & S_IRUSR != 0 && self.uid == uid {
			return true;
		}
		if self.mode & S_IRGRP != 0 && self.gid == gid {
			return true;
		}
		self.mode & S_IROTH != 0
	}

	/// Tells if the file can be written to by the given UID and GID.
	pub fn can_write(&self, uid: Uid, gid: Gid) -> bool {
		// If root, bypass checks
		if uid == ROOT_UID || gid == ROOT_GID {
			return true;
		}

		if self.mode & S_IWUSR != 0 && self.uid == uid {
			return true;
		}
		if self.mode & S_IWGRP != 0 && self.gid == gid {
			return true;
		}
		self.mode & S_IWOTH != 0
	}

	/// Tells if the file can be executed by the given UID and GID.
	pub fn can_execute(&self, uid: Uid, gid: Gid) -> bool {
		// If root, bypass checks
		if uid == ROOT_UID || gid == ROOT_GID {
			return true;
		}

		if self.mode & S_IXUSR != 0 && self.uid == uid {
			return true;
		}
		if self.mode & S_IXGRP != 0 && self.gid == gid {
			return true;
		}
		self.mode & S_IXOTH != 0
	}

	/// Returns the permissions of the file.
	pub fn get_permissions(&self) -> Mode {
		self.mode & 0o7777
	}

	/// Sets the permissions of the file.
	pub fn set_permissions(&mut self, mode: Mode) {
		self.mode = mode & 0o7777;
	}

	/// Returns an immutable reference to the location at which the file is stored.
	pub fn get_location(&self) -> &FileLocation {
		&self.location
	}

	/// Returns a mutable reference to the location at which the file is stored.
	pub fn get_location_mut(&mut self) -> &mut FileLocation {
		&mut self.location
	}

	/// Returns the number of hard links.
	pub fn get_hard_links_count(&self) -> u16 {
		self.hard_links_count
	}

	/// Sets the number of hard links.
	pub fn set_hard_links_count(&mut self, count: u16) {
		self.hard_links_count = count;
	}

	/// Returns the number of blocks allocated for the file.
	pub fn get_blocks_count(&self) -> u64 {
		self.blocks_count
	}

	/// Sets the number of blocks allocated for the file.
	pub fn set_blocks_count(&mut self, blocks_count: u64) {
		self.blocks_count = blocks_count;
	}

	/// Sets the file's size.
	pub fn set_size(&mut self, size: u64) {
		self.size = size;
	}

	/// Returns the owner user ID.
	pub fn get_uid(&self) -> Uid {
		self.uid
	}

	/// Returns the owner group ID.
	pub fn get_gid(&self) -> Gid {
		self.gid
	}

	/// Returns the timestamp of the last modification of the file's metadata.
	pub fn get_ctime(&self) -> Timestamp {
		self.ctime
	}

	/// Sets the timestamp of the last modification of the file's metadata.
	pub fn set_ctime(&mut self, ctime: Timestamp) {
		self.ctime = ctime;
	}

	/// Returns the timestamp of the last modification to the file.
	pub fn get_mtime(&self) -> Timestamp {
		self.mtime
	}

	/// Sets the timestamp of the last modification to the file.
	pub fn set_mtime(&mut self, mtime: Timestamp) {
		self.mtime = mtime;
	}

	/// Returns the timestamp of the last access to the file.
	pub fn get_atime(&self) -> Timestamp {
		self.atime
	}

	/// Sets the timestamp of the last access to the file.
	pub fn set_atime(&mut self, atime: Timestamp) {
		self.atime = atime;
	}

	/// Tells whether the directory is empty or not.
	/// If the current file isn't a directory, the function returns an error.
	pub fn is_empty_directory(&self) -> Result<bool, Errno> {
		if let FileContent::Directory(entries) = &self.content {
			Ok(entries.is_empty())
		} else {
			Err(errno!(ENOTDIR))
		}
	}

	/// Adds the directory entry `entry` to the current directory's entries.
	/// `name` is the name of the entry.
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
	pub fn to_dir_entry(&self) -> DirEntry {
		DirEntry {
			inode: self.location.inode,
			entry_type: self.get_file_type(),
		}
	}

	/// Returns the file's content.
	pub fn get_file_content(&self) -> &FileContent {
		&self.content
	}

	/// Sets the file's content, changing the file's type accordingly.
	pub fn set_file_content(&mut self, content: FileContent) {
		self.content = content;
	}

	/// Performs an ioctl operation on the file.
	/// `mem_space` is the memory space on which pointers are to be dereferenced.
	/// `request` is the ID of the request to perform.
	/// `argp` is a pointer to the argument.
	pub fn ioctl(&mut self, mem_space: IntSharedPtr<MemSpace>, request: u32, argp: *const c_void)
		-> Result<u32, Errno> {
		if let FileContent::CharDevice {
			major,
			minor,
		} = self.content {
			let dev = device::get_device(DeviceType::Char, major, minor)
				.ok_or_else(|| errno!(ENODEV))?;
			let guard = dev.lock();
			guard.get_mut().get_handle().ioctl(mem_space, request, argp)
		} else {
			Err(errno!(ENOTTY))
		}
	}

	/// Increments the number of times the file is open.
	pub fn increment_open(&mut self) {
		self.ref_count += 1;
	}

	/// Decrements the number of times the file is open.
	pub fn decrement_open(&mut self) {
		self.ref_count -= 1;
	}

	/// Tells whether the file is busy.
	pub fn is_busy(&self) -> bool {
		self.ref_count > 0
	}

	/// Synchronizes the file with the device.
	pub fn sync(&self) -> Result<(), Errno> {
		let mountpoint_mutex = self.location.get_mountpoint().ok_or_else(|| errno!(EIO))?;
		let mountpoint_guard = mountpoint_mutex.lock();
		let mountpoint = mountpoint_guard.get_mut();

		let io_mutex = mountpoint.get_source().get_io()?;
		let io_guard = io_mutex.lock();
		let io = io_guard.get_mut();

		let fs_mutex = mountpoint.get_filesystem();
		let fs_guard = fs_mutex.lock();
		let fs = fs_guard.get_mut();

		fs.update_inode(io, self)
	}
}

impl IO for File {
	fn get_size(&self) -> u64 {
		self.size
	}

	fn read(&mut self, off: u64, buff: &mut [u8]) -> Result<(u64, bool), Errno> {
		match &self.content {
			FileContent::Regular => {
				let (io_mutex, fs_mutex) = {
					let mountpoint_mutex = self.location.get_mountpoint()
						.ok_or_else(|| errno!(EIO))?;
					let mountpoint_guard = mountpoint_mutex.lock();
					let mountpoint = mountpoint_guard.get_mut();

					let io_mutex = mountpoint.get_source().get_io()?;
					let fs_mutex = mountpoint.get_filesystem();
					(io_mutex, fs_mutex)
				};

				let io_guard = io_mutex.lock();
				let io = io_guard.get_mut();

				let fs_guard = fs_mutex.lock();
				let fs = fs_guard.get_mut();

				fs.read_node(io, self.location.inode, off, buff)
			},

			FileContent::Directory(_) => Err(errno!(EISDIR)),

			FileContent::Link(_) => Err(errno!(EINVAL)),

			FileContent::Fifo => {
				let fcache_mutex = fcache::get();
				let fcache_guard = fcache_mutex.lock();
				let fcache = fcache_guard.get_mut().as_mut().unwrap();

				let pipe_mutex = fcache.get_named_fifo(self.get_location())?;
				let pipe_guard = pipe_mutex.lock();
				let pipe = pipe_guard.get_mut();
				pipe.read(off as _, buff)
			},

			FileContent::Socket => {
				let fcache_mutex = fcache::get();
				let fcache_guard = fcache_mutex.lock();
				let fcache = fcache_guard.get_mut().as_mut().unwrap();

				let sock_mutex = fcache.get_named_socket(self.get_location())?;
				let sock_guard = sock_mutex.lock();
				let _sock = sock_guard.get_mut();

				// TODO
				todo!();
			},

			FileContent::BlockDevice { .. } | FileContent::CharDevice { .. } => {
				let dev = match self.content {
					FileContent::BlockDevice { major, minor } => {
						device::get_device(DeviceType::Block, major, minor)
					},

					FileContent::CharDevice { major, minor } => {
						device::get_device(DeviceType::Char, major, minor)
					},

					_ => unreachable!(),
				}.ok_or_else(|| errno!(ENODEV))?;

				let guard = dev.lock();
				guard.get_mut().get_handle().read(off as _, buff)
			},
		}
	}

	fn write(&mut self, off: u64, buff: &[u8]) -> Result<u64, Errno> {
		match &self.content {
			FileContent::Regular => {
				let (io_mutex, fs_mutex) = {
					let mountpoint_mutex = self.location.get_mountpoint()
						.ok_or_else(|| errno!(EIO))?;
					let mountpoint_guard = mountpoint_mutex.lock();
					let mountpoint = mountpoint_guard.get_mut();

					let io_mutex = mountpoint.get_source().get_io()?;
					let fs_mutex = mountpoint.get_filesystem();
					(io_mutex, fs_mutex)
				};

				let io_guard = io_mutex.lock();
				let io = io_guard.get_mut();

				let fs_guard = fs_mutex.lock();
				let fs = fs_guard.get_mut();

				fs.write_node(io, self.location.inode, off, buff)?;

				self.size = max(off + buff.len() as u64, self.size);
				Ok(buff.len() as _)
			},

			FileContent::Directory(_) => Err(errno!(EISDIR)),

			FileContent::Link(_) => Err(errno!(EINVAL)),

			FileContent::Fifo => {
				let fcache_mutex = fcache::get();
				let fcache_guard = fcache_mutex.lock();
				let fcache = fcache_guard.get_mut().as_mut().unwrap();

				let pipe_mutex = fcache.get_named_fifo(self.get_location())?;
				let pipe_guard = pipe_mutex.lock();
				let pipe = pipe_guard.get_mut();
				pipe.write(off as _, buff)
			},

			FileContent::Socket => {
				let fcache_mutex = fcache::get();
				let fcache_guard = fcache_mutex.lock();
				let fcache = fcache_guard.get_mut().as_mut().unwrap();

				let sock_mutex = fcache.get_named_socket(self.get_location())?;
				let sock_guard = sock_mutex.lock();
				let _sock = sock_guard.get_mut();

				// TODO
				todo!();
			},

			FileContent::BlockDevice { .. } | FileContent::CharDevice { .. } => {
				let dev = match self.content {
					FileContent::BlockDevice { major, minor } => {
						device::get_device(DeviceType::Block, major, minor)
					},

					FileContent::CharDevice { major, minor } => {
						device::get_device(DeviceType::Char, major, minor)
					},

					_ => unreachable!(),
				}.ok_or_else(|| errno!(ENODEV))?;

				let guard = dev.lock();
				guard.get_mut().get_handle().write(off as _, buff)
			},
		}
	}

	fn poll(&mut self, mask: u32) -> Result<u32, Errno> {
		match &self.content {
			FileContent::Regular => {
				let (io_mutex, fs_mutex) = {
					let mountpoint_mutex = self.location.get_mountpoint()
						.ok_or_else(|| errno!(EIO))?;
					let mountpoint_guard = mountpoint_mutex.lock();
					let mountpoint = mountpoint_guard.get_mut();

					let io_mutex = mountpoint.get_source().get_io()?;
					let fs_mutex = mountpoint.get_filesystem();
					(io_mutex, fs_mutex)
				};

				let io_guard = io_mutex.lock();
				let _io = io_guard.get_mut();

				let fs_guard = fs_mutex.lock();
				let _fs = fs_guard.get_mut();

				// TODO
				todo!();
			},

			FileContent::Directory(_) => Err(errno!(EISDIR)),

			FileContent::Link(_) => Err(errno!(EINVAL)),

			FileContent::Fifo => {
				let fcache_mutex = fcache::get();
				let fcache_guard = fcache_mutex.lock();
				let fcache = fcache_guard.get_mut().as_mut().unwrap();

				let pipe_mutex = fcache.get_named_fifo(self.get_location())?;
				let pipe_guard = pipe_mutex.lock();
				let pipe = pipe_guard.get_mut();
				pipe.poll(mask)
			},

			FileContent::Socket => {
				let fcache_mutex = fcache::get();
				let fcache_guard = fcache_mutex.lock();
				let fcache = fcache_guard.get_mut().as_mut().unwrap();

				let sock_mutex = fcache.get_named_socket(self.get_location())?;
				let sock_guard = sock_mutex.lock();
				let _sock = sock_guard.get_mut();

				// TODO
				todo!();
			},

			FileContent::BlockDevice { .. } | FileContent::CharDevice { .. } => {
				let dev = match self.content {
					FileContent::BlockDevice { major, minor } => {
						device::get_device(DeviceType::Block, major, minor)
					},

					FileContent::CharDevice { major, minor } => {
						device::get_device(DeviceType::Char, major, minor)
					},

					_ => unreachable!(),
				}.ok_or_else(|| errno!(ENODEV))?;

				let guard = dev.lock();
				guard.get_mut().get_handle().poll(mask)
			},
		}
	}
}

/// Initializes files management.
/// `root_device_type` is the type of the root device file. If not a device, the behaviour is
/// undefined.
/// `root_major` is the major number of the device at the root of the VFS.
/// `root_minor` is the minor number of the device at the root of the VFS.
pub fn init(root_device_type: DeviceType, root_major: u32, root_minor: u32) -> Result<(), Errno> {
	fs::register_defaults()?;

	// Creating the root mountpoint
	let mount_source = MountSource::Device {
		dev_type: root_device_type,

		major: root_major,
		minor: root_minor,
	};
	mountpoint::create(mount_source, None, 0, Path::root())?;

	// Creating the files cache
	let cache = FCache::new()?;
	let guard = fcache::get().lock();
	*guard.get_mut() = Some(cache);

	Ok(())
}

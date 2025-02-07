//! A filesystem is the representation of the file hierarchy on a storage
//! device.

pub mod ext2;
pub mod initramfs;
pub mod kernfs;
pub mod procfs;
pub mod tmp;

use super::path::Path;
use super::File;
use crate::errno;
use crate::errno::Errno;
use crate::file::perm::Gid;
use crate::file::perm::Uid;
use crate::file::FileContent;
use crate::file::INode;
use crate::file::Mode;
use crate::util::container::hashmap::HashMap;
use crate::util::container::string::String;
use crate::util::io::IO;
use crate::util::lock::Mutex;
use crate::util::ptr::arc::Arc;
use core::any::Any;

/// This structure is used in the f_fsid field of statfs. It is currently
/// unused.
#[repr(C)]
#[derive(Debug, Default)]
struct Fsid {
	/// Unused.
	_val: [i32; 2],
}

/// Structure storing statistics about a filesystem.
#[repr(C)]
#[derive(Debug)]
pub struct Statfs {
	/// Type of filesystem.
	f_type: u32,
	/// Optimal transfer block size.
	f_bsize: u32,
	/// Total data blocks in filesystem.
	f_blocks: i64,
	/// Free blocks in filesystem.
	f_bfree: i64,
	/// Free blocks available to unprivileged user.
	f_bavail: i64,
	/// Total inodes in filesystem.
	f_files: i64,
	/// Free inodes in filesystem.
	f_ffree: i64,
	/// Filesystem ID.
	f_fsid: Fsid,
	/// Maximum length of filenames.
	f_namelen: u32,
	/// Fragment size.
	f_frsize: u32,
	/// Mount flags of filesystem.
	f_flags: u32,
}

/// Trait representing a filesystem.
pub trait Filesystem: Any {
	/// Returns the name of the filesystem.
	fn get_name(&self) -> &[u8];

	/// Tells whether the filesystem is mounted in read-only.
	fn is_readonly(&self) -> bool;
	/// Tells the kernel whether it must cache files.
	fn must_cache(&self) -> bool;

	/// Returns statistics about the filesystem.
	fn get_stat(&self, io: &mut dyn IO) -> Result<Statfs, Errno>;

	/// Returns the root inode of the filesystem.
	fn get_root_inode(&self, io: &mut dyn IO) -> Result<INode, Errno>;

	/// Returns the inode of the file with name `name`, located in the directory
	/// with inode `parent`.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `parent` is the inode's parent. If `None`, the function uses the root of
	/// the filesystem.
	/// - `name` is the name of the file.
	///
	/// If the parent is not a directory, the function returns an error.
	fn get_inode(
		&mut self,
		io: &mut dyn IO,
		parent: Option<INode>,
		name: &[u8],
	) -> Result<INode, Errno>;

	/// Loads the file at inode `inode`.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `inode` is the file's inode.
	/// - `name` is the file's name.
	fn load_file(&mut self, io: &mut dyn IO, inode: INode, name: String) -> Result<File, Errno>;

	/// Adds a file to the filesystem at inode `inode`.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `parent_inode` is the parent file's inode.
	/// - `name` is the name of the file.
	/// - `uid` is the id of the owner user.
	/// - `gid` is the id of the owner group.
	/// - `mode` is the permission of the file.
	/// - `content` is the content of the file. This value also determines the
	/// file type.
	///
	/// On success, the function returns the newly created file.
	fn add_file(
		&mut self,
		io: &mut dyn IO,
		parent_inode: INode,
		name: String,
		uid: Uid,
		gid: Gid,
		mode: Mode,
		content: FileContent,
	) -> Result<File, Errno>;

	/// Adds a hard link to the filesystem.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `parent_inode` is the parent file's inode.
	/// - `name` is the name of the link.
	/// - `inode` is the inode the link points to.
	///
	/// If this feature is not supported by the filesystem, the function returns
	/// an error.
	fn add_link(
		&mut self,
		io: &mut dyn IO,
		parent_inode: INode,
		name: &[u8],
		inode: INode,
	) -> Result<(), Errno>;

	/// Updates the given inode.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `file` the file structure containing the new values for the inode.
	fn update_inode(&mut self, io: &mut dyn IO, file: &File) -> Result<(), Errno>;

	/// Removes a file from the filesystem. If the links count of the inode
	/// reaches zero, the inode is also removed.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `parent_inode` is the parent file's inode.
	/// - `name` is the file's name.
	///
	/// The function returns the number of hard links left on the inode.
	fn remove_file(
		&mut self,
		io: &mut dyn IO,
		parent_inode: INode,
		name: &[u8],
	) -> Result<u16, Errno>;

	/// Reads from the given inode `inode` into the buffer `buf`.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `inode` is the file's inode.
	/// - `off` is the offset from which the data will be read from the node.
	/// - `buf` is the buffer in which the data is the be written. The length of the buffer is the
	/// number of bytes to read.
	///
	/// The function returns the number of bytes read.
	fn read_node(
		&mut self,
		io: &mut dyn IO,
		inode: INode,
		off: u64,
		buf: &mut [u8],
	) -> Result<u64, Errno>;

	/// Writes to the given inode `inode` from the buffer `buf`.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `inode` is the file's inode.
	/// - `off` is the offset at which the data will be written in the node.
	/// - `buf` is the buffer in which the data is the be written. The length of the buffer is the
	/// number of bytes to read.
	fn write_node(
		&mut self,
		io: &mut dyn IO,
		inode: INode,
		off: u64,
		buf: &[u8],
	) -> Result<(), Errno>;
}

/// Trait representing a filesystem type.
pub trait FilesystemType {
	/// Returns the name of the filesystem.
	fn get_name(&self) -> &'static [u8];

	/// Tells whether the given IO interface has the current filesystem.
	///
	/// `io` is the IO interface.
	fn detect(&self, io: &mut dyn IO) -> Result<bool, Errno>;

	/// Creates a new instance of the filesystem to mount it.
	///
	/// Arguments:
	/// - `io` is the IO interface.
	/// - `mountpath` is the path on which the filesystem is mounted.
	/// - `readonly` tells whether the filesystem is mounted in read-only.
	fn load_filesystem(
		&self,
		io: &mut dyn IO,
		mountpath: Path,
		readonly: bool,
	) -> Result<Arc<Mutex<dyn Filesystem>>, Errno>;
}

/// The list of filesystem types.
static FS_TYPES: Mutex<HashMap<String, Arc<dyn FilesystemType>>> = Mutex::new(HashMap::new());

/// Registers a new filesystem type.
pub fn register<T: 'static + FilesystemType>(fs_type: T) -> Result<(), Errno> {
	let name = String::try_from(fs_type.get_name())?;

	let mut container = FS_TYPES.lock();
	container.insert(name, Arc::new(fs_type)?)?;

	Ok(())
}

/// Unregisters the filesystem type with the given name.
///
/// If the filesystem type doesn't exist, the function does nothing.
pub fn unregister(name: &[u8]) {
	let mut container = FS_TYPES.lock();
	container.remove(name);
}

/// Returns the filesystem type with name `name`.
pub fn get_type(name: &[u8]) -> Option<Arc<dyn FilesystemType>> {
	let container = FS_TYPES.lock();
	container.get(name).cloned()
}

/// Detects the filesystem type on the given IO interface `io`.
pub fn detect(io: &mut dyn IO) -> Result<Arc<dyn FilesystemType>, Errno> {
	let container = FS_TYPES.lock();

	for (_, fs_type) in container.iter() {
		if fs_type.detect(io)? {
			return Ok(fs_type.clone());
		}
	}

	Err(errno!(ENODEV))
}

/// Registers the filesystems that are implemented inside of the kernel itself.
///
/// This function must be called only once, at initialization.
pub fn register_defaults() -> Result<(), Errno> {
	register(ext2::Ext2FsType {})?;
	register(tmp::TmpFsType {})?;
	register(procfs::ProcFsType {})?;
	// TODO sysfs

	Ok(())
}

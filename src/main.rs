use std::env;
use std::mem::size_of;
use std::fs::{File, OpenOptions};
use std::io::{Read,Seek, SeekFrom};
use std::ffi::OsStr;
use std::str::from_utf8;
use libc::{c_int, ENOENT, EIO};
use fuse::{FileType, FileAttr, Filesystem, Request, ReplyData, ReplyEntry, ReplyAttr, ReplyDirectory};
use time::Timespec;

macro_rules! offset_of {
    ($ty:ty, $field:ident) => {
        unsafe { &(*(0 as *const $ty)).$field as *const _ as usize }
    }
}

const TTL: Timespec = Timespec { sec: 1, nsec: 0 };

const EPOCH: Timespec = Timespec { sec: 0, nsec: 0 };

const HELLO_DIR_ATTR: FileAttr = FileAttr {
    ino: 1,
    size: 0,
    blocks: 0,
    atime: EPOCH,                                  // 1970-01-01 00:00:00
    mtime: EPOCH,
    ctime: EPOCH,
    crtime: EPOCH,
    kind: FileType::Directory,
    perm: 0o755,
    nlink: 2,
    uid: 501,
    gid: 20,
    rdev: 0,
    flags: 0,
};

const HELLO_TXT_CONTENT: &str = "Hello World!\n";

const HELLO_TXT_ATTR: FileAttr = FileAttr {
    ino: 2,
    size: 13,
    blocks: 1,
    atime: EPOCH,                                  // 1970-01-01 00:00:00
    mtime: EPOCH,
    ctime: EPOCH,
    crtime: EPOCH,
    kind: FileType::RegularFile,
    perm: 0o644,
    nlink: 1,
    uid: 501,
    gid: 20,
    rdev: 0,
    flags: 0,
};

struct HelloFS {
    device: File,
    superblock: Superblock,
}

#[derive(Default, Debug)]
#[repr(C)]
struct Inode {
    mode: u16,
    uid: u16,
    size: u32,
    atime: u32,
    ctime: u32,
    mtime: u32,
    dtime: u32,
    gid: u16,
    links_count: u16,
    blocks: u32,
    flags: u32,
    osd1: u32,
    block: [u32; 15],
    generation: u32,
    file_acl: u32,
    dir_acl: u32,
    faddr: u32,
    osd2: [u8; 12],
}

#[repr(C)]
struct Superblock {
    inode_count: u32,
    blocks_count: u32,
    r_blocks_count: u32,
    free_blocks_count: u32,
    free_inodes_count: u32,
    first_data_block: u32,
    log_block_size: u32,
    log_frag_size: u32,
    blocks_per_group: u32,
    frags_per_group: u32,
    inodes_per_group: u32,
    mtime: u32,
    wtime: u32,

    mnt_count: u16,
    max_mnt_count: u16,
    magic: u16,
    state: u16,
    errors: u16,
    minor_rev_level: u16,

    lastcheck: u32,
    checkinterval: u32,
    creator_os: u32,
    rev_level: u32,

    def_resuid: u16,
    def_resgid: u16,

    first_ino: u32,

    inode_size: u16,
    block_group_nr: u16,

    feature_compat: u32,
    feature_incompat: u32,
    feature_ro_compat: u32,
    uuid: [u8; 16],
    volume_name: [u8; 16],
    last_mounted: [u8; 64],
    algo_bitmap: u32,

    prealloc_blocks: u8,
    prealloc_dir_blocks: u8,
    _align1: u16,

    journal_uuid: [u8; 16],
    journal_inum: u32,
    journal_dev: u32,
    last_orphan: u32,
    hash_seed: [u32; 4],
    def_hash_version: u8,
    _padding1: [u8; 3],
    default_mount_options: u32,
    first_meta_bg: u32
}
type RawSB = [u8; size_of::<Superblock>()];

impl Default for Superblock {
    fn default() -> Superblock {
        let buf = [0; size_of::<Superblock>()];

        unsafe {
            std::mem::transmute::<RawSB, Superblock>(buf)
        }
    }
}

impl HelloFS {
    fn load_superblock(&mut self) -> Result<(), c_int> {
        self.device.seek(SeekFrom::Start(1024)).map_err(|_| EIO)?;

        unsafe {
            let buf: *mut RawSB = std::mem::transmute::<*mut Superblock, *mut RawSB>(&mut self.superblock);
            self.device.read_exact(&mut *buf);
        }

        Ok(())
    }

    fn load_inode(&mut self, ino: u64) -> Result<Inode, c_int> {
        println!("inodes per group: {}", self.superblock.inodes_per_group);

        let group_num = ino / (self.superblock.inodes_per_group as u64);

        let mut inode_buf = [0; size_of::<Inode>()];
        self.device.read_exact(&mut inode_buf);

        let inode = unsafe {
            std::mem::transmute::<[u8; size_of::<Inode>()], Inode>(inode_buf)
        };

        Ok(inode)
    }
}

/* FUSE uses inode 1 for the root dir, ext2 uses inode 2. inode 1 in ext2 is the bad block inode */
impl Filesystem for HelloFS {
    fn init(&mut self, req: &Request) -> Result<(), c_int> {
        println!("device: {:?}", self.device);
        self.load_superblock()?;

        Ok(())
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent = match parent { 1 => 2, x => x };

        println!("lookup: parent: {}, name: {}", parent, name.to_str().unwrap());
        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let ino = match ino { 1 => 2, x => x };

        let inode = self.load_inode(ino);

        println!("getattr: ino: {}, inode {:?}", ino, inode);

        reply.error(ENOENT);
    }

    fn read(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, _size: u32, reply: ReplyData) {
        let ino = match ino { 1 => 2, x => x };

        println!("read: ino: {}, offset: {}", ino, offset);

        reply.error(ENOENT);
    }

    fn readdir(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let ino = match ino { 1 => 2, x => x };

        println!("readdir: ino: {}, offset: {}", ino, offset);
        reply.error(ENOENT);
    }
}

fn main() {
    env_logger::init();
    let mountpoint = env::args_os().nth(1).unwrap();
    let device = env::args_os().nth(2).unwrap();
    let options = ["-o", "ro", "-o", "fsname=hello"]
        .iter()
        .map(|o| o.as_ref())
        .collect::<Vec<&OsStr>>();

    let fs = HelloFS {
        device: OpenOptions::new().write(true).read(true).open(device).unwrap(), 
        superblock: Default::default() 
    };

    fuse::mount(fs, &mountpoint, &options).unwrap();
}

use std::env;
use std::mem::size_of;
use std::fs::{File, OpenOptions};
use std::io::{Read,Seek, SeekFrom};
use std::ffi::OsStr;
use std::str::from_utf8;
use libc::{c_int, ENOENT, EIO, ENOTDIR, EINVAL, ENOSPC};
use fuse::{FileType, FileAttr, Filesystem, Request, ReplyData, ReplyEntry, ReplyAttr, ReplyDirectory, ReplyCreate};
use time::Timespec;

macro_rules! ALIGN {
    (UP, $val:expr, $b:expr) => { ((($val + $b - 1) / $b) * $b) };
    (DOWN, $val:expr, $b:expr) => { ((($val - 1) / $b) * $b)};
}

macro_rules! offset_of {
    ($ty:ty, $field:ident) => {
        unsafe { &(*(0 as *const $ty)).$field as *const _ as usize }
    }
}

macro_rules! load_obj {
    ($dev:expr, $ty:ty, $addr:expr) => {{
        let mut buf = [0; size_of::<$ty>()];
        $dev.seek(SeekFrom::Start($addr)).map_err(|_| EIO)?;
        $dev.read_exact(&mut buf).map_err(|_| EIO)?;

        let obj = unsafe {
            std::mem::transmute::<[u8; size_of::<$ty>()], $ty>(buf)
        };

        Ok(obj)
    }}
}

macro_rules! try_option {
    ($e:expr,$reply:ident,$err:ident) => {
        match $e {
            Some(v) => v,
            None => {
                $reply.error($err);
                return;
            }
        }
    }
}
macro_rules! try_error {
    ($e:expr, $reply: ident) => {
        match $e {
            Ok(v) => v,
            Err(e) => {
                println!("got error {:?}", e);
                $reply.error(e);
                return;
            }
        }
    }
}
const BGDT_OFFSET: u32 = 1;

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

impl Inode {
    fn get_kind(&self) -> FileType {
        match self.mode & 0xf000 {
            0xc000 => FileType::Socket,
            0xa000 => FileType::Symlink,
            0x8000 => FileType::RegularFile,
            0x6000 => FileType::BlockDevice,
            0x4000 => FileType::Directory,
            0x2000 => FileType::CharDevice,
            0x1000 => FileType::NamedPipe,
            _ => panic!(),
        }
    }
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

#[repr(C)]
struct BlockGroupDescTable {
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    free_blocks_count: u16,
    free_inodes_count: u16,
    used_dir_count: u16,
    pad: u16,
    reserved: [u8; 12]
}

#[derive(Debug)]
struct FuseDirEntry {
    inode: u32,
    filetype: FileType,
    name: String,
}

fn ext2_dirent_filetype(file_type: u8) -> FileType {
    match file_type {
        1 => FileType::RegularFile,
        2 => FileType::Directory,
        3 => FileType::CharDevice,
        4 => FileType::BlockDevice,
        5 => FileType::NamedPipe,
        6 => FileType::Socket,
        7 => FileType::Symlink,
        _ => panic!(),
    }
}

const DIRENT_HEADER_LEN: u8 = 8;

impl HelloFS {
    fn load_superblock(&mut self) -> Result<(), c_int> {
        self.device.seek(SeekFrom::Start(1024)).map_err(|_| EIO)?;

        unsafe {
            let buf: *mut RawSB = std::mem::transmute::<*mut Superblock, *mut RawSB>(&mut self.superblock);
            self.device.read_exact(&mut *buf).map_err(|_| EIO)?;
        }

        Ok(())
    }

    fn block_size(&self) -> usize {
        1024 << self.superblock.log_block_size
    }

    fn block2addr(&self, block_num: u32) -> u64 {
        (block_num as u64) << ((self.superblock.log_block_size as u64) + 10)
    }

    fn get_group_first_block(&self, group_num: u32) -> u32 {
        (self.superblock.first_data_block + (self.superblock.blocks_per_group * group_num)).into()
    }

    fn load_bitmap_bit(&mut self, bitmap_blk: u32, offset: u32) -> Result<u8, c_int> {
        let offset_byte = offset / 8;
        let offset_bit = offset % 8;

        let bitmap_addr = self.block2addr(bitmap_blk);

        let addr = bitmap_addr + offset_byte as u64;
        self.seek(addr)?;

        let mut buf: [u8;1] = [0];
        self.device.read_exact(&mut buf).map_err(|_| EIO)?;

        Ok(buf[0] & (1 << offset_bit))
    }

    // XXX should it load the entire BGDT block or just the table for the given block group?
    fn load_bgdt(&mut self, group_num: u32) -> Result<BlockGroupDescTable, c_int> {
        let bdgt_block = self.superblock.first_data_block + BGDT_OFFSET;
        let bdgt_addr = self.block2addr(bdgt_block) + ((group_num as u64)*size_of::<BlockGroupDescTable>() as u64);

        load_obj!(self.device, BlockGroupDescTable, bdgt_addr)
    }

    fn load_inode(&mut self, ino: u64) -> Result<Inode, c_int> {
        let group_num = (ino - 1) / (self.superblock.inodes_per_group as u64);
        let inode_table_num = (ino - 1) % (self.superblock.inodes_per_group as u64);

        let bgdt = self.load_bgdt(group_num as u32)?;

        let is_inode_free = self.load_bitmap_bit(bgdt.inode_bitmap, inode_table_num as u32)? == 0;
        if is_inode_free {
            return Err(ENOENT);
        }

        let inode_table_blk = bgdt.inode_table;
        let inode_table_offset = (inode_table_num * size_of::<Inode>() as u64);
        let inode_addr = self.block2addr(inode_table_blk) + inode_table_offset;

        load_obj!(self.device, Inode, inode_addr)
    }

    fn load_from_blocks(&mut self, ino: u64, offset: usize, load_size: u32) -> Result<Vec<u8>, c_int> {
        let inode = self.load_inode(ino)?;

        let load_size = load_size as usize;
        let mut buf: Vec<u8> = Vec::with_capacity(load_size);
        unsafe { buf.set_len(load_size); }
        let mut buf_ptr = buf.as_mut_ptr();

        let block_size = self.block_size();
        let read_end = offset + load_size;
        let block_skip = offset / block_size;

        let mut cursor = block_skip * block_size;
        for block_num in inode.block.iter().skip(block_skip) {
            if *block_num == 0 {
                break;
            }

            let mut block_addr = self.block2addr(*block_num);

            let amt_left: usize = read_end - cursor;
            let mut read_size: usize = if amt_left < block_size {
                amt_left
            } else {
                block_size
            };

            println!("calc blk_offset");
            let blk_offset = std::cmp::max(offset as i64 - cursor as i64, 0) as usize;
            read_size -= blk_offset;
            block_addr += blk_offset as u64;
            println!("got blk_offset {}", blk_offset);

            let read_slice = unsafe { std::slice::from_raw_parts_mut(buf_ptr, read_size) };

            println!("load_from_blocks: reading block {} off {} [0x{:x}]", *block_num, blk_offset, block_addr);

            self.seek(block_addr)?;
            self.device.read_exact(read_slice).map_err(|_| EIO)?;

            if amt_left < block_size {
                break;
            }

            cursor += block_size;
            buf_ptr = unsafe { buf_ptr.add(block_size) };
        }

        Ok(buf)
    }

    fn load_dir_entries(&mut self, ino: u64) -> Result<Vec<FuseDirEntry>, c_int> {
        let inode = self.load_inode(ino)?;

        if inode.get_kind() != FileType::Directory {
            return Err(ENOTDIR);
        }

        let mut entries = Vec::new();
        let size = self.block_size();
        let mut buf: Vec<u8> = Vec::with_capacity(size);

        // read exact uses len to determine how much to read
        unsafe { buf.set_len(size); }

        for block_num in &inode.block {
            if *block_num == 0 {
                break;
            }

            let block_addr = self.block2addr(*block_num);
            println!("load_dir_entries: reading block {} [0x{:x}]", *block_num, block_addr);

            self.seek(block_addr)?;
            self.device.read_exact(&mut buf).map_err(|_| EIO)?;

            println!("buf: {:?}", buf.len());

            let mut i = 0;
            let mut buf_ptr = buf.as_ptr();
            while i < size {
                unsafe {
                    let inode: u32 = *(buf_ptr as *const u32);
                    let rec_len: usize = *(buf_ptr.offset(4) as *const u16) as usize;
                    let name_len: u8 = *(buf_ptr.offset(6) as *const u8);
                    let file_type: u8 = *(buf_ptr.offset(7) as *const u8);
                    let name = std::slice::from_raw_parts(buf_ptr.offset(8), name_len as usize);

                    println!("dirent: inode {} rec_len {} name_len {} ft {} {}",
                             inode, rec_len, name_len, file_type, String::from_utf8_lossy(name).to_string());

                    if rec_len == 0 {
                        break;
                    }

                    // normally there can only be blank dents at the start of a dir block
                    if inode == 0 && i != 0 {
                        println!("Warning: blank dentry found not at start of block.");
                    }

                    if inode != 0 {
                        entries.push(FuseDirEntry {
                            inode: inode,
                            filetype: ext2_dirent_filetype(file_type),
                            name: String::from_utf8_lossy(name).to_string()
                        });
                    }

                    buf_ptr = buf_ptr.add(rec_len);
                    i += rec_len;
                }
            }
        }

        Ok(entries)
    }

    fn seek(&mut self, addr: u64) -> Result<(), c_int> {
        self.device.seek(SeekFrom::Start(addr)).map_err(|_| EIO);
        Ok(())
    }

    /*
    fn find_create_dir_ent(&mut self, parent: u64, target_name: String, mode: u32) -> Result<(), c_int> {
        // get dir entries
        // find dir entry of <name>, or end of entries
        // if dir entry is found, open file
        // otherwise, create file
        //  - alloc inode
        //  - add dir entry

        let parent_inode = self.load_inode(parent)?;
        if parent_inode.get_kind() != FileType::Directory {
            return Err(ENOTDIR);
        }

        let target_rec_len = ALIGN!(UP, DIRENT_HEADER_LEN as usize + target_name.len(), 4);
        println!("target_rec_len {}", target_rec_len);

        let size = self.block_size();
        let mut buf: Vec<u8> = Vec::with_capacity(size);

        // read exact uses len to determine how much to read
        unsafe { buf.set_len(size); }

        // dirent with space to split
        let mut alloc_buf_addr = 0;
        let mut alloc_buf_len = 0;

        let block_idx = 0;
        for block_num in &inode.block {
            if *block_num == 0 {
                break;
            }
            block_idx += 1;

            let block_addr = self.block2addr(*block_num);
            println!("load_dir_entries: reading block {} [0x{:x}]", *block_num, block_addr);

            self.seek(block_addr)?;
            self.device.read_exact(&mut buf).map_err(|_| EIO)?;

            println!("buf: {:?}", buf.len());

            let mut i = 0;
            let mut buf_ptr = buf.as_ptr();
            while i < size {
                unsafe {
                    let inode: u32 = *(buf_ptr as *const u32);
                    let rec_len: usize = *(buf_ptr.offset(4) as *const u16) as usize;
                    let name_len: u8 = *(buf_ptr.offset(6) as *const u8);
                    let file_type: u8 = *(buf_ptr.offset(7) as *const u8);
                    let name = std::slice::from_raw_parts(buf_ptr.offset(8), name_len as usize);

                    if rec_len == 0 {
                        break;
                    }

                    if name == target_name {
                        return self.open_inode(inode);
                    }

                    let min_rec_len = ALIGN!(UP, DIRENT_HEADER_LEN as usize + name_len, 4);
                    if alloc_buf_len == 0 && min_rec_len + target_rec_len <= rec_len {
                        alloc_buf_len = rec_len;
                        alloc_buf_addr = block_addr + i;
                    }

                    buf_ptr = buf_ptr.add(rec_len);
                    i += rec_len;
                }
            }
        }

        // never found a dirent to alloc into, create a new block
        if alloc_buf_len == 0 {
            let block_num = self.alloc_block(parent, parent_inode)?;
            alloc_buf_len

        }



        Ok(())
    }
    */
}

fn inode2fuse(num: u64, i: Inode) -> FileAttr {
    let kind = i.get_kind();
    let rdev = match kind {
        FileType::BlockDevice | FileType::CharDevice => i.block[0],
        _ => 0
    };

    FileAttr {
        ino: map_inode(InodeMapDir::EXT2FUSE, num),
        size: i.size as u64,
        blocks: i.blocks as u64,
        atime: Timespec::new(i.atime as i64, 0),
        mtime: Timespec::new(i.mtime as i64, 0),
        ctime: Timespec::new(i.ctime as i64, 0),
        kind: kind,
        perm: i.mode & 0xfff,
        nlink: i.links_count as u32,
        uid: i.uid as u32,
        gid: i.gid as u32,
        rdev: rdev,

        // MacOS only (?)
        crtime: Timespec::new(0, 0),
        flags: 0,
    }
}

enum InodeMapDir {
    EXT2FUSE,
    FUSE2EXT,
}

/* FUSE uses inode 1 for the root dir, ext2 uses inode 2. inode 1 in ext2 is the bad block inode */
fn map_inode(dir: InodeMapDir, num: u64) -> u64 {
    match dir {
        InodeMapDir::EXT2FUSE => match num { 2 => 1, x => x },
        InodeMapDir::FUSE2EXT => match num { 1 => 2, x => x },
    }
}

impl Filesystem for HelloFS {
    fn init(&mut self, req: &Request) -> Result<(), c_int> {
        println!("device: {:?}", self.device);
        self.load_superblock()?;

        Ok(())
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent = map_inode(InodeMapDir::FUSE2EXT, parent);
        let name = try_option!(name.to_str(), reply, ENOENT);

        println!("lookup: parent: {}, name: {}", parent, name);
        let entries = try_error!(self.load_dir_entries(parent), reply);

        let ent = entries.iter().find(|e| e.name == name);
        let ent = try_option!(ent, reply, ENOENT);

        let ino_num: u64 = ent.inode.into();

        let inode = try_error!(self.load_inode(ino_num), reply);
        let attr = inode2fuse(ino_num, inode);
        reply.entry(&TTL, &attr, 0)
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let ino = map_inode(InodeMapDir::FUSE2EXT, ino);

        println!("getattr {}", ino);
        let inode = try_error!(self.load_inode(ino), reply);

        let attr = inode2fuse(ino, inode);
        reply.attr(&TTL, &attr)
    }

    fn read(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, size: u32, reply: ReplyData) {
        let ino = map_inode(InodeMapDir::FUSE2EXT, ino);


        println!("read: ino: {}, offset: {}", ino, offset);
        let buf = try_error!(self.load_from_blocks(ino, offset as usize, size), reply);

        println!("read {}", String::from_utf8_lossy(&buf).to_string());
        reply.data(&buf);
    }

    fn readdir(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let ino = map_inode(InodeMapDir::FUSE2EXT, ino);

        println!("readdir: ino: {}, offset: {}", ino, offset);
        let entries = try_error!(self.load_dir_entries(ino), reply);
        for (i, ent) in entries.iter().enumerate().skip(offset as usize) {
            reply.add(
                map_inode(InodeMapDir::EXT2FUSE, ent.inode as u64),
                (i + 1) as i64,
                ent.filetype,
                ent.name.clone()
            );
        }

        reply.ok();
    }

    fn create(&mut self, _req: &Request, parent: u64, name: &OsStr, mode: u32, _flags: u32, reply: ReplyCreate) {
        let parent = map_inode(InodeMapDir::FUSE2EXT, parent);
        let name = try_option!(name.to_str(), reply, EINVAL);

        println!("create: parent: {}, name: {}, mode {:o}", parent, name, mode);

        try_error!(self.find_create_dir_ent(parent, name.to_string(), mode), reply);

    }
}

fn main() {
    env_logger::init();
    let mountpoint = env::args_os().nth(1).unwrap();
    let device = env::args_os().nth(2).unwrap();
    //let options = ["-o", "ro", "-o", "fsname=hello"]
    let options = ["-o", "fsname=hello"]
        .iter()
        .map(|o| o.as_ref())
        .collect::<Vec<&OsStr>>();

    let fs = HelloFS {
        device: OpenOptions::new().write(true).read(true).open(device).unwrap(),
        superblock: Default::default()
    };

    fuse::mount(fs, &mountpoint, &options).unwrap();
}

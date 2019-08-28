use std::cmp;
use std::mem::{size_of};
use libc::{c_int, ENOENT, EIO, ENOTDIR, ENOSPC};
use std::time::SystemTime;
use std::convert::TryInto;

use fuse::{FileType};

use crate::device::Device;

mod filesystem;

macro_rules! ALIGN {
    (UP, $val:expr, $b:expr) => { (($val + $b - 1) / $b) * $b };
    (DOWN, $val:expr, $b:expr) => { (($val - 1) / $b) * $b };
}

const BGDT_OFFSET: u32 = 1;

pub struct HelloFS {
    device: Device,
    superblock: Superblock,
}

#[derive(Default, Debug)]
#[repr(C)]
pub struct Inode {
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
    pub fn get_kind(&self) -> Option<FileType> {
        Some(match self.mode & 0xf000 {
            0xc000 => FileType::Socket,
            0xa000 => FileType::Symlink,
            0x8000 => FileType::RegularFile,
            0x6000 => FileType::BlockDevice,
            0x4000 => FileType::Directory,
            0x2000 => FileType::CharDevice,
            0x1000 => FileType::NamedPipe,
            _ => return None,
        })
    }

    pub fn get_size(&self) -> usize {
        if self.get_kind() == Some(FileType::RegularFile) {
            (self.dir_acl as usize) << 32 | (self.size as usize)
        } else {
            self.size as usize
        }
    }

    pub fn get_rdev(&self) -> u32 {
        let kind = self.get_kind().unwrap_or(FileType::RegularFile);
        match kind {
            FileType::BlockDevice | FileType::CharDevice => self.block[0],
            _ => 0
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

struct ExtDirEnt<'a> {
    inode: u32,
    rec_len: usize,
    name_len: u8,
    file_type: u8,
    name: &'a [u8]
}

fn ext2_dirent_filetype(file_type: u8) -> Option<FileType> {
    Some(match file_type {
        1 => FileType::RegularFile,
        2 => FileType::Directory,
        3 => FileType::CharDevice,
        4 => FileType::BlockDevice,
        5 => FileType::NamedPipe,
        6 => FileType::Socket,
        7 => FileType::Symlink,
        _ => { println!("filetype {} unknown", file_type); return None }
    })
}

fn get_ext2_dirent_ft_from_kind(ft: FileType) -> u8 {
    match ft {
        FileType::RegularFile => 1,
        FileType::Directory => 2,
        FileType::CharDevice => 3,
        FileType::BlockDevice => 4,
        FileType::NamedPipe => 5,
        FileType::Socket => 6,
        FileType::Symlink => 7,
    }
}

const DIRENT_HEADER_LEN: u8 = 8;

struct InodeBlockIterator<'a> {
    block: &'a [u32],
    block_offset: u32,
    block_limit: u32,
    indirect_block_page: Vec<u32>,
    double_indirect_block_page: Vec<u32>,
    triple_indirect_block_page: Vec<u32>,
    fs: &'a HelloFS,
}

impl <'a> InodeBlockIterator<'a> {
    pub fn new(inode: &'a Inode, fs: &'a HelloFS) -> Self {
        Self {
            block: &inode.block,
            block_limit: (inode.get_size() / fs.block_size()) as u32,
            block_offset: 0,
            fs: fs,
            indirect_block_page: Vec::new(),
            double_indirect_block_page: Vec::new(),
            triple_indirect_block_page: Vec::new(),
        }
    }
}

const INDIRECT_BLOCK_INDEX: usize = 12;
const DOUBLE_INDIRECT_BLOCK_INDEX: usize = 13;
const TRIPLE_INDIRECT_BLOCK_INDEX: usize = 14;

impl Iterator for InodeBlockIterator<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<Self::Item> {
        if self.block_offset > self.block_limit {
            return None;
        }

        // does this fit in direct blocks?
        if self.block_offset < INDIRECT_BLOCK_INDEX  as u32 {
            let ret = self.block[self.block_offset as usize];

            self.block_offset += 1;
            return Some(ret);
        }

        let page_blocks = self.fs.block_size() / size_of::<u32>();
        let mut indirect_offset: usize = self.block_offset as usize - INDIRECT_BLOCK_INDEX as usize;

        // does this fit in indirect blocks?
        if indirect_offset < page_blocks {
            println!("using indirect blocks");
            if self.indirect_block_page.len() == 0 {
                self.fs.load_block(self.block[INDIRECT_BLOCK_INDEX], &mut self.indirect_block_page).unwrap();
            }
            let ret = self.indirect_block_page[indirect_offset];

            self.block_offset += 1;
            return Some(ret);
        }

        indirect_offset -= page_blocks;

        let double_page_blocks = page_blocks * page_blocks;

        // does this fit in double indirect blocks?
        if indirect_offset < double_page_blocks {
            println!("using double indirect blocks");
            let double_indirect_offset = indirect_offset / page_blocks;
            indirect_offset = indirect_offset % page_blocks;

            if self.double_indirect_block_page.len() == 0 {
                self.fs.load_block(self.block[DOUBLE_INDIRECT_BLOCK_INDEX], &mut self.double_indirect_block_page).unwrap();
            }
            if indirect_offset == 0 || self.indirect_block_page.len() == 0 {
                self.fs.load_block(self.double_indirect_block_page[double_indirect_offset], &mut self.indirect_block_page).unwrap();
            }

            let ret = self.indirect_block_page[indirect_offset];

            self.block_offset += 1;
            return Some(ret);
        }

        println!("using triple indirect blocks");
        indirect_offset -= double_page_blocks;
        let triple_indirect_offset = indirect_offset / double_page_blocks;
        let double_indirect_offset = (indirect_offset % double_page_blocks) / page_blocks;
        indirect_offset = (indirect_offset % double_indirect_offset) % page_blocks;

        if self.triple_indirect_block_page.len() == 0 {
            self.fs.load_block(self.block[TRIPLE_INDIRECT_BLOCK_INDEX], &mut self.triple_indirect_block_page).unwrap();
        }
        if double_indirect_offset == 0 || self.double_indirect_block_page.len() == 0 {
            self.fs.load_block(self.triple_indirect_block_page[triple_indirect_offset], &mut self.double_indirect_block_page).unwrap();
        }
        if indirect_offset == 0 || self.indirect_block_page.len() == 0 {
            self.fs.load_block(self.double_indirect_block_page[double_indirect_offset], &mut self.indirect_block_page).unwrap();
        }

        let ret = self.indirect_block_page[indirect_offset];

        self.block_offset += 1;
        return Some(ret);
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.block_offset += n as u32;

        // clear block page caches, could possibly save some if n is small enough
        self.indirect_block_page = Vec::new();
        self.double_indirect_block_page = Vec::new();
        self.triple_indirect_block_page = Vec::new();

        self.next()
    }
}

const SUPERBLOCK_ADDR: u64 = 1024;

impl HelloFS {
    pub fn new(device: Device) -> Self {
        Self {
            device: device,
            superblock: Default::default()
        }
    }
    fn load_superblock(&mut self) -> Result<(), c_int> {
        self.device.load_in(&mut self.superblock, SUPERBLOCK_ADDR)
    }

    // XXX should store backup blocks too
    fn store_superblock(&self) -> Result<(), c_int> {
        self.device.store::<Superblock>(&self.superblock, SUPERBLOCK_ADDR)
    }

    fn load_block<T>(&self, block_no: u32, buf: &mut Vec<T>) -> Result<(), c_int> {
        let expected_count = self.block_size() / size_of::<T>();

        println!("loading block {} into buf {:p} (len:{}), expected count: {}", block_no, buf, buf.len(), expected_count);

        if buf.len() == 0 {
            buf.reserve(expected_count);
            unsafe { buf.set_len(expected_count); }
            println!("resizing buf to {}, new len {}", expected_count, buf.len());
        } else if buf.len() != expected_count {
            panic!("unexpected buffer size");
        }

        let addr = self.block2addr(block_no);
        let load_size = expected_count * size_of::<T>();
        let buf_ptr = buf.as_mut_ptr() as *mut u8;
        let slice = unsafe { std::slice::from_raw_parts_mut(buf_ptr, load_size) };

        self.device.read_at(slice, addr)
    }

    fn get_inode_blocks<'a>(&'a self, inode: &'a Inode) -> InodeBlockIterator<'a> {
        InodeBlockIterator::new(inode, self)
    }

    fn block_size(&self) -> usize {
        1024 << self.superblock.log_block_size
    }

    fn block2addr(&self, block_num: u32) -> u64 {
        (block_num as u64) << ((self.superblock.log_block_size as u64) + 10)
    }

    /*
    fn get_group_first_block(&self, group_num: u32) -> u32 {
        (self.superblock.first_data_block + (self.superblock.blocks_per_group * group_num)).into()
    }
    */

    fn load_bitmap_bit(&mut self, bitmap_blk: u32, offset: u32) -> Result<u8, c_int> {
        let offset_byte = offset / 8;
        let offset_bit = offset % 8;

        let bitmap_addr = self.block2addr(bitmap_blk);

        let addr = bitmap_addr + offset_byte as u64;

        let mut buf: [u8;1] = [0];
        self.device.read_at(&mut buf, addr)?;

        Ok(buf[0] & (1 << offset_bit))
    }

    // XXX should it load the entire BGDT block or just the table for the given block group?
    fn load_bgdt(&mut self, group_num: u32) -> Result<Box<BlockGroupDescTable>, c_int> {
        let bgdt_block = self.superblock.first_data_block + BGDT_OFFSET;
        let bgdt_addr = self.block2addr(bgdt_block) + ((group_num as u64)*size_of::<BlockGroupDescTable>() as u64);

        self.device.load(bgdt_addr)
    }

    // XXX should store backup blocks too
    fn store_bgdt(&mut self, group_num: u32, bgdt: &BlockGroupDescTable) -> Result<(), c_int> {
        let bgdt_block = self.superblock.first_data_block + BGDT_OFFSET;
        let bgdt_addr = self.block2addr(bgdt_block) + ((group_num as u64)*size_of::<BlockGroupDescTable>() as u64);

        self.device.store(bgdt, bgdt_addr)
    }
    fn get_inode_group_info(&self, ino: u64) -> (u64, u64) {
        let group_num = (ino - 1) / (self.superblock.inodes_per_group as u64);
        let inode_table_num = (ino - 1) % (self.superblock.inodes_per_group as u64);

        (group_num, inode_table_num)
    }

    fn load_inode(&mut self, ino: u64) -> Result<Inode, c_int> {
        let (group_num, inode_table_num) = self.get_inode_group_info(ino);

        let bgdt = self.load_bgdt(group_num as u32)?;

        let is_inode_free = self.load_bitmap_bit(bgdt.inode_bitmap, inode_table_num as u32)? == 0;
        if is_inode_free {
            return Err(ENOENT);
        }

        let inode_table_blk = bgdt.inode_table;
        let inode_table_offset = inode_table_num * size_of::<Inode>() as u64;
        let inode_addr = self.block2addr(inode_table_blk) + inode_table_offset;

        let box_inode: Box<Inode> = self.device.load::<Inode>(inode_addr)?;

        Ok(*box_inode)
    }

    fn store_inode(&mut self, ino: u64, inode: &Inode) -> Result<(), c_int> {
        let (group_num, inode_table_num) = self.get_inode_group_info(ino);

        let bgdt = self.load_bgdt(group_num as u32)?;

        let inode_table_blk = bgdt.inode_table;
        let inode_table_offset = inode_table_num * size_of::<Inode>() as u64;
        let inode_addr = self.block2addr(inode_table_blk) + inode_table_offset;

        self.device.store::<Inode>(inode, inode_addr)
    }

    // try to allocate an inode close to the parent
    fn alloc_inode(&mut self, parent_ino: u64) -> Result<u32, c_int> {
        let (group_num, _) = self.get_inode_group_info(parent_ino);

        let mut bgdt = self.load_bgdt(group_num as u32)?;
        let total_group_count = self.superblock.blocks_count / self.superblock.blocks_per_group;

        let mut group_idx = group_num as u32;
        while bgdt.free_inodes_count == 0 {
            group_idx = (group_idx + 1) % total_group_count;
            if group_idx as u64 == group_num {
                return Err(ENOSPC);
            }

            bgdt = self.load_bgdt(group_idx)?;
        }

        let mut buf: Vec<u8> = Vec::new();
        println!("loading inode_bitmap into {:p}", &buf);

        self.load_block(bgdt.inode_bitmap, &mut buf)?;
        println!("loaded, new buf size: {}", buf.len());

        // find open inode in bitmap
        let mut inode_table_num = 0;
        for b in &mut buf {
            println!("b[{:p}]={:x}", b, *b);
            if *b == 0xff {
                inode_table_num += 8;
                continue;
            }
            let mut byte = *b;

            let mut shift = 0;
            while byte & 0x1 != 0 {
                byte = byte >> 1;
                shift += 1;
            }

            println!("flipping bitmap: b: {:x} shift: {}, int: {}", *b, shift, inode_table_num);
            *b = *b | (1 << shift);
            inode_table_num += shift;

            break;
        }

        println!("writing inode bitmap");
        let bitmap_addr = self.block2addr(bgdt.inode_bitmap);
        self.device.write_at(&buf, bitmap_addr)?;

        println!("writing bgdt");
        bgdt.free_inodes_count -= 1;
        self.store_bgdt(group_idx, &bgdt)?;

        println!("writing superblock");
        self.superblock.free_inodes_count -= 1;
        self.store_superblock()?;

        let inode_num = group_idx * self.superblock.inodes_per_group + inode_table_num + 1;

        println!("created inode {}, group_idx {}, inode_table_num {}", inode_num,group_idx, inode_table_num);

        Ok(inode_num)
    }

    fn load_from_blocks(&mut self, ino: u64, offset: usize, load_size: u32) -> Result<Vec<u8>, c_int> {
        let inode = self.load_inode(ino)?;

        let load_size = cmp::min(load_size as usize, inode.get_size());
        let mut buf: Vec<u8> = Vec::with_capacity(load_size);
        unsafe { buf.set_len(load_size); }
        let mut buf_ptr = buf.as_mut_ptr();

        let block_size = self.block_size();
        let read_end = offset + load_size;
        let block_skip = offset / block_size;

        let mut cursor = block_skip * block_size;
        let iter = self.get_inode_blocks(&inode);
        for block_num in iter.skip(block_skip) {
            if block_num == 0 {
                break;
            }

            let mut block_addr = self.block2addr(block_num);

            let amt_left: usize = read_end - cursor;
            let mut read_size: usize = if amt_left < block_size {
                amt_left
            } else {
                block_size
            };

            println!("calc blk_offset");
            let blk_offset = cmp::max(offset as i64 - cursor as i64, 0) as usize;
            read_size -= blk_offset;
            block_addr += blk_offset as u64;
            println!("got blk_offset {}", blk_offset);

            let read_slice = unsafe { std::slice::from_raw_parts_mut(buf_ptr, read_size) };

            println!("load_from_blocks: reading block {} off {} [0x{:x}]", block_num, blk_offset, block_addr);

            self.device.read_at(read_slice, block_addr)?;

            if amt_left < block_size {
                break;
            }

            cursor += block_size;
            buf_ptr = unsafe { buf_ptr.add(block_size) };
        }

        Ok(buf)
    }

    // callback returns true if should stop
    fn walk_dir_entries(&mut self, inode: &Inode, mut cb: impl FnMut(usize, usize, &ExtDirEnt) -> Result<bool, c_int> ) -> Result<bool, c_int> {
        if inode.get_kind() != Some(FileType::Directory) {
            return Err(ENOTDIR);
        }

        let size = self.block_size();
        let mut buf: Vec<u8> = Vec::with_capacity(size);

        // read exact uses len to determine how much to read
        unsafe { buf.set_len(size); }

        for block_num in &inode.block {
            if *block_num == 0 {
                continue;
            }

            let block_addr = self.block2addr(*block_num);
            println!("load_dir_entries: reading block {} [0x{:x}]", *block_num, block_addr);

            self.device.read_at(&mut buf, block_addr)?;

            println!("buf: {:?}", buf.len());

            let mut block_offset = 0;
            let mut buf_ptr = buf.as_ptr();
            while block_offset < size {
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

                    let dirent = ExtDirEnt {
                        inode: inode,
                        rec_len: rec_len,
                        name_len: name_len,
                        file_type: file_type,
                        name: name,
                    };

                    if cb(block_addr as usize, block_offset, &dirent)? {
                        return Ok(true);
                    }

                    buf_ptr = buf_ptr.add(rec_len);
                    block_offset += rec_len;
                }
            }
        }
        Ok(false)
    }

    fn load_dir_entries(&mut self, ino: u64) -> Result<Vec<FuseDirEntry>, c_int> {
        let mut entries = Vec::new();

        let inode = self.load_inode(ino)?;
        self.walk_dir_entries(&inode, |_, _block_offset, dirent| {
            let inode = dirent.inode;

            if inode != 0 {
                let filetype = match ext2_dirent_filetype(dirent.file_type) {
                    Some(ft) => ft,
                    None => return Ok(false),
                };

                entries.push(FuseDirEntry {
                    inode: inode,
                    filetype: filetype,
                    name: String::from_utf8_lossy(dirent.name).to_string()
                });
            }

            Ok(false)
        })?;

        Ok(entries)
    }

    /*
    fn unlink_dir_ent(&mut self, parent: u64, target_name: String) -> Result<(), c_int> {
        // find dir entry of <name>
        // expand dir entry of previous entry

        let mut prev_ent = std::ptr::null();

        let parent_inode = self.load_inode(parent)?;
        self.walk_dir_entries(&parent_inode, |block_addr, block_offset, dirent| {
            let dirent_name = String::from_utf8_lossy(dirent.name).to_string();
            if dirent_name == target_name {
                // XXX should we actually open this sometime?
                opened_inode = Some(dirent.inode);

                // this direntry was deleted, but we can reuse it
                if (dirent.inode == 0) {
                    alloc_buf_len = dirent.rec_len;
                    alloc_buf_addr = block_addr + block_offset;
                }

                return Ok(true);
            }

            let min_rec_len = ALIGN!(UP, DIRENT_HEADER_LEN as usize + dirent.name_len as usize, 4);
            if alloc_buf_len == 0 && min_rec_len + target_rec_len <= dirent.rec_len {
                alloc_buf_len = dirent.rec_len;
                alloc_buf_addr = block_addr + block_offset;
            }

            Ok(false)
        });
    */


    fn find_create_dir_ent(&mut self, parent: u64, target_name: String, mode: u32, rdev: u32) -> Result<(u32, Inode), c_int> {
        // get dir entries
        // find dir entry of <name>, or end of entries
        // if dir entry is found, open file
        // otherwise, create file
        //  - alloc inode
        //  - add dir entry

        let target_rec_len = ALIGN!(UP, DIRENT_HEADER_LEN as usize + target_name.len(), 4);
        println!("target_rec_len {}", target_rec_len);

        let mut alloc_buf_len = 0;
        let mut alloc_buf_addr = 0;
        let mut opened_inode = None;

        let parent_inode = self.load_inode(parent)?;
        self.walk_dir_entries(&parent_inode, |block_addr, block_offset, dirent| {
            let dirent_name = String::from_utf8_lossy(dirent.name).to_string();
            if dirent_name == target_name {
                // XXX should we actually open this sometime?
                opened_inode = Some(dirent.inode);

                // this direntry was deleted, but we can reuse it
                if dirent.inode == 0 {
                    alloc_buf_len = dirent.rec_len;
                    alloc_buf_addr = block_addr + block_offset;
                }

                return Ok(true);
            }

            let min_rec_len = ALIGN!(UP, DIRENT_HEADER_LEN as usize + dirent.name_len as usize, 4);
            if alloc_buf_len == 0 && min_rec_len + target_rec_len <= dirent.rec_len {
                alloc_buf_len = dirent.rec_len;
                alloc_buf_addr = block_addr + block_offset;
            }

            Ok(false)
        })?;

        match opened_inode {
            Some(0) | None => {},
            Some(inode_num) => {
                let inode = self.load_inode(inode_num as u64)?;
                return Ok((inode_num, inode));
            }
        };

        let new_directory_block = alloc_buf_len == 0;

        // never found a dirent to alloc into, create a new block
        /*
        if new_directory_block {
            let block_num = self.alloc_block(parent, parent_inode)?;
            alloc_buf_len

        }
        */

        let new_inode_num = self.alloc_inode(parent)?;
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_err(|_| EIO)?.as_secs();
        let mut new_inode = Inode {
            mode: mode as u16,
            atime: now as u32,
            ctime: now as u32,
            mtime: now as u32,
            links_count: 1,
            ..Default::default()
        };
        new_inode.block[0] = rdev;
        self.store_inode(new_inode_num as u64, &new_inode)?;

        let mut buf: Vec<u8> = Vec::with_capacity(alloc_buf_len);
        unsafe { buf.set_len(alloc_buf_len); }

        self.device.read_at(&mut buf, alloc_buf_addr as u64)?;

        let mut buf_ptr = buf.as_mut_ptr();
        unsafe {
            // if opened_inode is none, then need to create a new dirent
            if opened_inode.is_none() {
                let mut new_rec_len = alloc_buf_len;

                if !new_directory_block {
                    let name_len: u8 = *(buf_ptr.offset(6) as *const u8);

                    // resize existing dirent
                    let min_rec_len = ALIGN!(UP, DIRENT_HEADER_LEN as usize + name_len as usize, 4);
                    *(buf_ptr.offset(4) as *mut u16) = min_rec_len as u16;

                    new_rec_len -= min_rec_len;
                    buf_ptr = buf_ptr.add(min_rec_len);
                }

                // fill new dirent
                *(buf_ptr.offset(4) as *mut u16) = new_rec_len as u16;
                *(buf_ptr.offset(6) as *mut u8) = target_name.len().try_into().unwrap();

                let tgt_name_slice = target_name.as_bytes();
                let name_buf = std::slice::from_raw_parts_mut(buf_ptr.offset(8), tgt_name_slice.len());
                name_buf.copy_from_slice(tgt_name_slice);
            }

            // if opened_inode is not none, then we found a deleted inode. both new and deleted dirents
            // need to set the inode and ft nums of the dirent
            *(buf_ptr as *mut u32) = new_inode_num;
            *(buf_ptr.offset(7) as *mut u8) = get_ext2_dirent_ft_from_kind(new_inode.get_kind().ok_or(EIO)?);
        }

        self.device.write_at(&buf, alloc_buf_addr as u64)?;

        Ok((new_inode_num, new_inode))
    }
}


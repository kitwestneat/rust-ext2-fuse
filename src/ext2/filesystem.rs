use std::ffi::OsStr;
use std::convert::TryInto;
use time::Timespec;
use libc::{c_int, ENOENT, EINVAL };
use fuse::{FileType, FileAttr, Filesystem, Request, ReplyData, ReplyEntry, ReplyAttr, ReplyDirectory, ReplyCreate};
use crate::ext2::{HelloFS, Inode};

const TTL: Timespec = Timespec { sec: 1, nsec: 0 };

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

fn inode2fuse(num: u64, i: Inode) -> FileAttr {
    let kind = i.get_kind().unwrap_or(FileType::RegularFile); //XXX hack
    let rdev = i.get_rdev();
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
    fn init(&mut self, _req: &Request) -> Result<(), c_int> {
        println!("device: {:?}", self.device);
        self.load_superblock()?;

        println!("superblock loaded");
        println!("compat {:x} incompat {:x} ro_compat: {:x}",
                 self.superblock.feature_compat,
                 self.superblock.feature_incompat,
                 self.superblock.feature_ro_compat);

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


        println!("read: ino: {}, offset: {}, size {}", ino, offset, size);
        let buf = try_error!(self.load_from_blocks(ino, offset as usize, size), reply);

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

        let (ino, inode) = try_error!(self.find_create_dir_ent(parent, name.to_string(), mode, 0), reply);

        let attr = inode2fuse(ino as u64, inode);
        reply.created(&TTL, &attr, 0, 0, 0);
    }

    fn setattr(
        &mut self,
        _req: &Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<Timespec>,
        mtime: Option<Timespec>,
        _fh: Option<u64>,
        _crtime: Option<Timespec>,
        _chgtime: Option<Timespec>,
        _bkuptime: Option<Timespec>,
        _flags: Option<u32>,
        reply: ReplyAttr
    ) {
        let ino = map_inode(InodeMapDir::FUSE2EXT, ino);

        println!("setattr {}", ino);
        let mut inode = try_error!(self.load_inode(ino), reply);

        macro_rules! try_set {
            ($inode:ident,$x:ident) => {
                if let Some($x) = $x {
                    $inode.$x = $x.try_into().unwrap();
                }
            }
        }
        try_set!(inode, mode);
        try_set!(inode, uid);
        try_set!(inode, gid);
        try_set!(inode, size);

        let atime = atime.map(|ts| ts.sec as u32);
        try_set!(inode, atime);
        let mtime = mtime.map(|ts| ts.sec as u32);
        try_set!(inode, mtime);

        try_error!(self.store_inode(ino.clone(), &inode), reply);

        let attr = inode2fuse(ino, inode);
        reply.attr(&TTL, &attr)
    }

    /*
    fn unlink(
        &mut self, 
        _req: &Request, 
        parent: u64, 
        name: &OsStr, 
        reply: ReplyEmpty
    ) {
        let parent = map_inode(InodeMapDir::FUSE2EXT, parent);
        let name = try_option!(name.to_str(), reply, EINVAL);


        try_error!(self.unlink_dir_ent(parent, name.to_string());

        reply.ok();
    }
    */
}


use std::env;
use std::ffi::OsStr;

mod device;
mod ext2;

use device::Device;
use ext2::HelloFS;

fn main() {
    env_logger::init();
    let mountpoint = env::args_os().nth(1).unwrap();
    let device = env::args_os().nth(2).unwrap();
    //let options = ["-o", "ro", "-o", "fsname=hello"]
    let options = ["-o", "fsname=hello"]
        .iter()
        .map(|o| o.as_ref())
        .collect::<Vec<&OsStr>>();

    let fs = HelloFS::new(Device::open(device).unwrap());

    fuse::mount(fs, &mountpoint, &options).unwrap();
}

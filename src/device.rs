use libc::{c_int, EIO };
use std::path::Path;
use std::fs::{File, OpenOptions};
use std::io::{Read,Seek, SeekFrom, Write};
use std::mem::{size_of};
use std::sync::Mutex;

pub trait DeviceData {}

#[macro_export]
macro_rules! device_data_type {
    ($t:ty) => { impl DeviceData for $t {} }
}

#[derive(Debug)]
pub struct Device {
    file: Mutex<File>,
}

impl Device {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, c_int> {
        Ok(Self {
            file: Mutex::new(OpenOptions::new().write(true).read(true).open(path).map_err(|_| EIO)?),
        })
    }

    pub fn load<T>(&self, addr: u64) -> Result<Box<T>, c_int> where T: DeviceData {
        let alloc_buf_len = size_of::<T>();
        let mut buf: Vec<u8> = Vec::with_capacity(alloc_buf_len);
        unsafe { buf.set_len(alloc_buf_len); }

        self.read_at(&mut buf, addr)?;

        let buf_slice: Box<[u8]> = buf.into_boxed_slice();
        let buf_slice_ptr: *mut [u8] = Box::into_raw(buf_slice);

        unsafe {
            let t_ptr: *mut T = (*buf_slice_ptr).as_mut_ptr() as *mut T;

            Ok(Box::from_raw(t_ptr))
        }
    }

    pub fn load_in<T>(&self, obj: &mut T, addr: u64) -> Result<(), c_int> where T: DeviceData {
        let buf: &mut [u8] = unsafe {
            std::slice::from_raw_parts_mut((obj as *mut _) as *mut u8, size_of::<T>())
        };

        self.read_at(buf, addr)?;

        Ok(())
    }


    pub fn read_at(&self, buf: &mut [u8], addr: u64) -> Result<(), c_int> {
        let mut dev = self.file.lock().unwrap();

        println!("reading {} bytes from {:x} into {:p}", buf.len(), addr, buf);

        dev.seek(SeekFrom::Start(addr)).map_err(|_| EIO)?;
        dev.read_exact(buf).map_err(|_| EIO)?;

        Ok(())
    }

    pub fn store<T>(&self, obj: &T, addr: u64) -> Result<(), c_int> where T: DeviceData {
        let buf = unsafe {
            std::slice::from_raw_parts((obj as *const _) as *const u8, size_of::<T>())
        };

        self.write_at(buf, addr)
    }
    pub fn write_at(&self, buf: &[u8], addr: u64) ->Result<(), c_int> {
        let mut dev = self.file.lock().unwrap();

        dev.seek(SeekFrom::Start(addr)).map_err(|_| EIO)?;
        dev.write_all(buf).map_err(|_| EIO)?;

        Ok(())
    }

    /*
    pub fn seek(&self, addr: u64) -> Result<(), c_int> {
        self.file.lock().unwrap().seek(SeekFrom::Start(addr)).map_err(|_| EIO)?;

        Ok(())
    }
    */
}

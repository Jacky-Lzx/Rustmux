//! POSIX shared-memory transport for outer Kitty graphics uploads.
//!
//! The name stays linked until Kitty consumes it or the attachment ends.
//! Kitty unlinks a successfully read object; `Drop` cleans up failures and
//! unsent commands.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use nix::libc;
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::graphics_store::MAX_PANE_IMAGE_BYTES;

static NEXT_NAME: AtomicU64 = AtomicU64::new(0);

pub(crate) struct SharedPixels {
    name: CString,
    len: usize,
}

impl SharedPixels {
    pub(crate) fn create(pixels: &[u8]) -> io::Result<Self> {
        if pixels.is_empty() || pixels.len() > MAX_PANE_IMAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid shared image size",
            ));
        }
        for _ in 0..8 {
            let epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos() as u64;
            let nonce = NEXT_NAME.fetch_add(1, Ordering::Relaxed) ^ epoch;
            // Keep the name short enough for macOS's POSIX SHM namespace.
            let name = CString::new(format!(
                "/rmx-{:x}-{:08x}",
                std::process::id(),
                nonce as u32
            ))
            .expect("generated SHM name contains no NUL");
            // SAFETY: `name` is NUL-terminated and its descriptor is owned
            // immediately by `File` on success.
            let fd = unsafe {
                libc::shm_open(
                    name.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                    0o600,
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: successful `shm_open` returned an owned descriptor.
            let file = unsafe { File::from_raw_fd(fd) };
            let object = Self {
                name,
                len: pixels.len(),
            };
            // macOS does not accept O_CLOEXEC for shm_open.
            // SAFETY: fd is valid while `file` is live.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let size = libc::off_t::try_from(pixels.len()).map_err(io::Error::other)?;
            // SAFETY: fd is valid and size was checked for off_t overflow.
            if unsafe { libc::ftruncate(fd, size) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // Darwin POSIX SHM descriptors are mapped, not written like
            // regular files (`write` returns ENXIO on macOS).
            // SAFETY: fd is valid, nonempty length fits the sized object, and
            // the returned mapping is checked before copying and unmapped.
            let mapped = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    pixels.len(),
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            if mapped == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            unsafe {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), mapped.cast(), pixels.len());
            }
            if unsafe { libc::munmap(mapped, pixels.len()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            drop(file);
            return Ok(object);
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "unable to allocate a unique shared image name",
        ))
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn consumed(&self) -> bool {
        // Kitty unlinks the name as soon as it reads the object. Leave other
        // failures linked until the attachment drops and cleans them up.
        // SAFETY: the name is NUL-terminated; any opened fd is closed here.
        let fd = unsafe { libc::shm_open(self.name.as_ptr(), libc::O_RDONLY, 0) };
        if fd >= 0 {
            unsafe { libc::close(fd) };
            false
        } else {
            io::Error::last_os_error().kind() == io::ErrorKind::NotFound
        }
    }

    pub(crate) fn query_command(&self, image_id: u32) -> Vec<u8> {
        format!(
            "\x1b_Ga=q,t=s,f=24,s=1,v=1,S={},i={image_id};{}\x1b\\",
            self.len,
            STANDARD.encode(self.name.as_bytes())
        )
        .into_bytes()
    }

    pub(crate) fn placement_command(
        &self,
        format: u8,
        image_size: (u32, u32),
        cell_size: (u32, u32),
        image_id: u32,
        z_index: i32,
    ) -> Vec<u8> {
        let (width, height) = image_size;
        let (columns, rows) = cell_size;
        // A natural-size PNG must not be stretched to the inferred cell box.
        // Raw virtual previews retain their existing fit-to-cells behavior.
        let fit = if format == 100 {
            String::new()
        } else {
            format!(",c={columns},r={rows}")
        };
        format!(
            "\x1b_Ga=T,t=s,f={format},s={width},v={height},S={},i={image_id}{fit},z={z_index},C=1,q=2;{}\x1b\\",
            self.len,
            STANDARD.encode(self.name.as_bytes())
        )
        .into_bytes()
    }
}

impl Drop for SharedPixels {
    fn drop(&mut self) {
        // SAFETY: this exact name was created by this process. Kitty may
        // already have unlinked it after reading; ENOENT is harmless.
        unsafe { libc::shm_unlink(self.name.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_pixels_have_bounded_lifetime_and_encoded_name() {
        let object = SharedPixels::create(&[1, 2, 3]).unwrap();
        assert!(
            object
                .query_command(32)
                .starts_with(b"\x1b_Ga=q,t=s,f=24,s=1,v=1,S=3,i=32;")
        );
        assert!(
            object
                .placement_command(24, (1, 1), (1, 1), 42, 0)
                .starts_with(b"\x1b_Ga=T,t=s,f=24,s=1,v=1,S=3,i=42,c=1,r=1,z=0,C=1,q=2;")
        );
        let rgba = SharedPixels::create(&[1, 2, 3, 128]).unwrap();
        assert!(
            rgba.placement_command(32, (1, 1), (1, 1), 43, 0)
                .starts_with(b"\x1b_Ga=T,t=s,f=32,s=1,v=1,S=4,i=43,c=1,r=1,z=0,C=1,q=2;")
        );
        assert!(
            rgba.placement_command(100, (1, 1), (2, 2), 44, 0)
                .starts_with(b"\x1b_Ga=T,t=s,f=100,s=1,v=1,S=4,i=44,z=0,C=1,q=2;")
        );
        // SAFETY: the object name exists until consumed or dropped.
        let fd = unsafe { libc::shm_open(object.name.as_ptr(), libc::O_RDONLY, 0) };
        assert!(fd >= 0);
        // SAFETY: the mapping is bounded by the known three-byte object.
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                3,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(mapped, libc::MAP_FAILED);
        let mut bytes = [0u8; 3];
        unsafe {
            bytes.copy_from_slice(std::slice::from_raw_parts(mapped.cast::<u8>(), 3));
            libc::munmap(mapped, 3);
            libc::close(fd);
        }
        assert_eq!(bytes, [1, 2, 3]);
        let name = object.name.clone();
        drop(object);
        // SAFETY: this is only a read-only existence check.
        assert!(unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) } < 0);
    }

    #[test]
    fn consumed_object_is_detected_after_outer_unlink() {
        let object = SharedPixels::create(&[4, 5, 6]).unwrap();
        assert!(!object.consumed());
        // SAFETY: simulate the outer terminal consuming this exact object.
        assert_eq!(unsafe { libc::shm_unlink(object.name.as_ptr()) }, 0);
        assert!(object.consumed());
    }
}

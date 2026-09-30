//! Bounded POSIX shared-memory input for complete Kitty image transfers.
//! Name validation, mappings, descriptors, and unlinking stay within this module.

use super::{
    AssembledDirectTransfer, Pending, decode_base64, finish_local_transfer, max_transfer_bytes,
    parse_positive, valid_first,
};
use crate::graphics::command::parse_command;
use nix::libc;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::FromRawFd;

/// A complete POSIX shared-memory command, if this is the requested medium.
/// The name is never interpreted as a filesystem path. A failed read has one
/// public error regardless of whether the name exists or the object is short.
pub(crate) fn shared_memory_transfer(
    command: &[u8],
) -> Option<Result<AssembledDirectTransfer, ()>> {
    let (mut controls, encoded) = parse_command(command)?;
    if controls.get(&b't').map(Vec::as_slice) != Some(b"s") {
        return None;
    }
    Some((|| {
        let size = controls
            .remove(&b'S')
            .map(|value| parse_positive_u64(&value).ok_or(()))
            .transpose()?;
        let offset = controls
            .remove(&b'O')
            .map(|value| parse_decimal_u64(&value).ok_or(()))
            .transpose()?
            .unwrap_or(0);
        controls.insert(b't', b"d".to_vec());
        if !matches!(controls.get(&b'm').map(Vec::as_slice), None | Some(b"0"))
            || !valid_first(&controls)
        {
            return Err(());
        }
        let name = decode_base64(encoded).ok_or(())?;
        if name.len() < 2 || name.len() > 255 || name[0] != b'/' || name[1..].contains(&b'/') {
            return Err(());
        }
        let name = CString::new(name).map_err(|_| ())?;
        // SAFETY: `name` is a NUL-terminated POSIX SHM name and the returned
        // descriptor is owned immediately by `File`.
        let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
        if fd < 0 {
            return Err(());
        }
        // SAFETY: `shm_open` returned a new, valid descriptor owned here.
        let file = unsafe { File::from_raw_fd(fd) };
        // macOS shm_open rejects O_CLOEXEC, so set it on the returned fd.
        // SAFETY: `file` owns a valid descriptor.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(());
        }
        let result = (|| {
            let length = file.metadata().map_err(|_| ())?.len();
            let available = length.checked_sub(offset).ok_or(())?;
            // macOS reports page-rounded SHM lengths. For uncompressed raw
            // pixels the dimensions give the exact byte count without S.
            let raw_size = if !controls.contains_key(&b'o')
                && controls.get(&b'f').map(Vec::as_slice) != Some(b"100")
            {
                let width = parse_positive(controls.get(&b's').ok_or(())?).ok_or(())? as u64;
                let height = parse_positive(controls.get(&b'v').ok_or(())?).ok_or(())? as u64;
                let channels = if controls.get(&b'f').map(Vec::as_slice) == Some(b"24") {
                    3
                } else {
                    4
                };
                Some(
                    width
                        .checked_mul(height)
                        .and_then(|n| n.checked_mul(channels))
                        .ok_or(())?,
                )
            } else {
                None
            };
            let bytes = size.or(raw_size).unwrap_or(available);
            if bytes == 0 || bytes > available || bytes > max_transfer_bytes(&controls) as u64 {
                return Err(());
            }
            let data = read_shared_memory_slice(fd, offset, bytes)?;
            finish_local_transfer(Pending { controls, data }).ok_or(())
        })();
        // Kitty specifies that the terminal unlinks the object after reading.
        // SAFETY: `name` remains alive and NUL-terminated for this call.
        unsafe { libc::shm_unlink(name.as_ptr()) };
        result
    })())
}

fn read_shared_memory_slice(fd: libc::c_int, offset: u64, bytes: u64) -> Result<Vec<u8>, ()> {
    // mmap offsets must be page-aligned, while Kitty's O may be arbitrary.
    // SAFETY: sysconf has no memory-safety preconditions.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size)
        .ok()
        .filter(|&size| size > 0)
        .ok_or(())?;
    let aligned = offset / page_size * page_size;
    let prefix = usize::try_from(offset - aligned).map_err(|_| ())?;
    let size = usize::try_from(bytes).map_err(|_| ())?;
    let map_len = prefix.checked_add(size).ok_or(())?;
    let map_offset = libc::off_t::try_from(aligned).map_err(|_| ())?;
    // SAFETY: the caller checked the object's length and bounds. The mapping
    // stays alive until after its bytes have been copied into an owned Vec.
    let mapped = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            map_len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            map_offset,
        )
    };
    if mapped == libc::MAP_FAILED {
        return Err(());
    }
    // SAFETY: mmap succeeded for map_len bytes, including prefix + size.
    let data =
        unsafe { std::slice::from_raw_parts((mapped as *const u8).add(prefix), size).to_vec() };
    // SAFETY: this is the exact mapping returned by mmap.
    unsafe { libc::munmap(mapped, map_len) };
    Ok(data)
}

fn parse_positive_u64(value: &[u8]) -> Option<u64> {
    parse_decimal_u64(value).filter(|&number| number != 0)
}

fn parse_decimal_u64(value: &[u8]) -> Option<u64> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(value).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SHM: AtomicU64 = AtomicU64::new(0);

    fn make_shm(data: &[u8]) -> CString {
        let name = CString::new(format!(
            "/rustmux-test-{}-{}",
            std::process::id(),
            NEXT_SHM.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        // SAFETY: the name is a valid, NUL-terminated POSIX SHM name.
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        assert!(fd >= 0, "shm_open: {}", std::io::Error::last_os_error());
        // SAFETY: the newly created descriptor is owned by this File.
        let file = unsafe { File::from_raw_fd(fd) };
        // SAFETY: the descriptor is valid and the length fits off_t.
        assert_eq!(unsafe { libc::ftruncate(fd, data.len() as libc::off_t) }, 0);
        // SAFETY: the object was sized above and remains open until after copy.
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                data.len(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(mapped, libc::MAP_FAILED);
        // SAFETY: the mapping is writable for exactly data.len() bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast(), data.len());
            libc::munmap(mapped, data.len());
        }
        drop(file);
        name
    }

    fn shm_command(controls: &str, name: &CString) -> Vec<u8> {
        format!(
            "\x1b_G{controls};{}\x1b\\",
            STANDARD.encode(name.as_bytes())
        )
        .into_bytes()
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn assert_unlinked(name: &CString) {
        // SAFETY: name is NUL-terminated; close any unexpectedly opened fd.
        let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
        if fd >= 0 {
            unsafe {
                libc::close(fd);
                libc::shm_unlink(name.as_ptr());
            }
        }
        assert_eq!(fd, -1, "shared-memory source was not unlinked");
    }

    #[test]
    fn shared_memory_compressed_png_reads_selected_range_and_unlinks() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[1, 2, 3, 4])
                .unwrap();
        }
        let compressed = zlib(&png);
        let mut data = b"prefix".to_vec();
        data.extend_from_slice(&compressed);
        data.extend_from_slice(b"unselected trailing bytes");
        let name = make_shm(&data);
        let command = shm_command(
            &format!("a=q,t=s,f=100,o=z,S={},O=6,i=17", compressed.len()),
            &name,
        );
        let transfer = shared_memory_transfer(&command).unwrap().unwrap();
        assert_eq!(transfer.data, png);
        assert_eq!(transfer.control(b'f'), Some(b"100".as_slice()));
        for key in b"SoO" {
            assert_eq!(transfer.control(*key), None);
        }
        assert_eq!(transfer.control(b't'), Some(b"d".as_slice()));
        assert_unlinked(&name);
    }

    #[test]
    fn shared_memory_compressed_png_rejects_incomplete_corrupt_or_trailing_streams() {
        let good = zlib(b"PNG bytes are validated by the image decoder");
        let mut corrupt = good.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        let mut trailing = good.clone();
        trailing.push(0);
        for data in [
            good[..good.len() - 1].to_vec(),
            corrupt,
            trailing,
            zlib(b""),
        ] {
            let name = make_shm(&data);
            let command = shm_command(&format!("a=q,t=s,f=100,o=z,S={},i=17", data.len()), &name);
            assert!(shared_memory_transfer(&command).unwrap().is_err());
            assert_unlinked(&name);
        }
        // A requested slice outside the object also releases its source.
        let name = make_shm(&good);
        let command = shm_command("a=q,t=s,f=100,o=z,S=1,O=18446744073709551615,i=17", &name);
        assert!(shared_memory_transfer(&command).unwrap().is_err());
        assert_unlinked(&name);
    }

    #[test]
    fn shared_memory_compressed_png_bounds_expansion_and_unlinks_on_failure() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        let chunk = [0; 64 * 1024];
        for _ in 0..super::super::MAX_DIRECT_PNG_TRANSFER_BYTES / chunk.len() {
            encoder.write_all(&chunk).unwrap();
        }
        encoder.write_all(&[0]).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(compressed.len() < 1024 * 1024);
        let name = make_shm(&compressed);
        let command = shm_command(
            &format!("a=q,t=s,f=100,o=z,S={},i=17", compressed.len()),
            &name,
        );
        assert!(shared_memory_transfer(&command).unwrap().is_err());
        assert_unlinked(&name);
    }

    #[test]
    fn shared_memory_reads_bounded_slice_and_unlinks_afterwards() {
        let name = make_shm(b"zz\x01\x02\x03tail");
        let command = shm_command("a=q,t=s,f=24,s=1,v=1,S=3,O=2,i=17", &name);
        let transfer = shared_memory_transfer(&command).unwrap().unwrap();
        assert_eq!(transfer.data, b"\x01\x02\x03");
        assert_eq!(transfer.control(b't'), Some(b"d".as_slice()));
        assert_eq!(transfer.control(b'S'), None);
        assert_eq!(transfer.control(b'O'), None);
        // SAFETY: the name is NUL-terminated; a failed open returns no fd.
        assert_eq!(
            unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) },
            -1
        );
    }

    #[test]
    fn shared_memory_rejects_short_and_over_limit_objects() {
        let short = make_shm(b"\x01\x02");
        // macOS rounds object lengths to 16 KiB; request beyond that page.
        let command = shm_command("a=q,t=s,f=24,s=1,v=1,S=16385,i=18", &short);
        assert!(shared_memory_transfer(&command).unwrap().is_err());
        // SAFETY: the object was unlinked after the failed read.
        assert_eq!(
            unsafe { libc::shm_open(short.as_ptr(), libc::O_RDONLY, 0) },
            -1
        );

        let large = make_shm(b"\x01\x02\x03");
        let command = shm_command("a=q,t=s,f=24,s=1,v=1,S=16777217,i=19", &large);
        assert!(shared_memory_transfer(&command).unwrap().is_err());
        // SAFETY: the object was unlinked after the bounded read check.
        assert_eq!(
            unsafe { libc::shm_open(large.as_ptr(), libc::O_RDONLY, 0) },
            -1
        );
    }

    #[test]
    fn shared_memory_does_not_open_a_path_or_malformed_name() {
        let command = direct("a=q,t=s,f=24,s=1,v=1,i=20", b"/tmp/secret");
        assert!(shared_memory_transfer(&command).unwrap().is_err());
        let command = direct("a=q,t=s,f=24,s=1,v=1,i=20", b"missing-slash");
        assert!(shared_memory_transfer(&command).unwrap().is_err());
    }

    fn direct(controls: &str, data: &[u8]) -> Vec<u8> {
        format!("\x1b_G{controls};{}\x1b\\", STANDARD.encode(data)).into_bytes()
    }
}

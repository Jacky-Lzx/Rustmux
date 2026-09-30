//! Bounded regular-file input for Kitty `t=f` transfers. Source files are retained.

use super::{
    AssembledDirectTransfer, Pending, decode_base64, finish, max_transfer_bytes, valid_first,
};
use crate::graphics::command::parse_command;
use flate2::bufread::ZlibDecoder;
use nix::libc;
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::Path,
};

pub(crate) fn file_transfer(command: &[u8]) -> Option<Result<AssembledDirectTransfer, ()>> {
    let (mut controls, encoded) = parse_command(command)?;
    if controls.get(&b't').map(Vec::as_slice) != Some(b"f") {
        return None;
    }
    Some((|| {
        let size = controls
            .remove(&b'S')
            .map(|value| decimal(&value).filter(|&size| size > 0).ok_or(()))
            .transpose()?;
        let offset = controls
            .remove(&b'O')
            .map(|value| decimal(&value).ok_or(()))
            .transpose()?
            .unwrap_or(0);
        controls.insert(b't', b"d".to_vec());
        if !matches!(controls.get(&b'm').map(Vec::as_slice), None | Some(b"0"))
            || !valid_first(&controls)
        {
            return Err(());
        }
        let name = decode_base64(encoded).ok_or(())?;
        if name.is_empty() || name.len() > 4096 || name.contains(&0) {
            return Err(());
        }
        let path = fs::canonicalize(Path::new(OsStr::from_bytes(&name))).map_err(|_| ())?;
        // Follow symlinks, but reject special files before opening them: even
        // opening a device can have side effects, and a FIFO could block.
        if !fs::metadata(&path).map_err(|_| ())?.is_file() {
            return Err(());
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| ())?;
        let metadata = file.metadata().map_err(|_| ())?;
        // Recheck the opened descriptor in case the path changed during open.
        if !metadata.is_file() {
            return Err(());
        }
        let available = metadata.len().checked_sub(offset).ok_or(())?;
        let bytes = size.unwrap_or(available);
        let limit = max_transfer_bytes(&controls);
        if bytes == 0 || bytes > available || bytes > limit as u64 {
            return Err(());
        }
        file.seek(SeekFrom::Start(offset)).map_err(|_| ())?;
        let mut data = vec![0; usize::try_from(bytes).map_err(|_| ())?];
        file.read_exact(&mut data).map_err(|_| ())?;
        if controls.contains_key(&b'o') && controls.get(&b'f').map(Vec::as_slice) == Some(b"100") {
            // For file media S selects stored bytes, not the expanded PNG
            // length. Bound expansion independently before normal PNG validation.
            let mut decoder = ZlibDecoder::new(data.as_slice());
            let mut expanded = Vec::new();
            (&mut decoder)
                .take(limit as u64 + 1)
                .read_to_end(&mut expanded)
                .map_err(|_| ())?;
            if expanded.is_empty()
                || expanded.len() > limit
                || decoder.total_in() != data.len() as u64
            {
                return Err(());
            }
            data = expanded;
            controls.remove(&b'o');
        }
        finish(Pending { controls, data }).ok_or(())
    })())
}

fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::ZlibEncoder};
    use std::{
        io::Write,
        os::unix::{fs::symlink, net::UnixListener},
    };
    use tempfile::{NamedTempFile, tempdir};

    fn command(controls: &str, path: &Path) -> Vec<u8> {
        format!(
            "\x1b_G{controls};{}\x1b\\",
            STANDARD.encode(path.as_os_str().as_bytes())
        )
        .into_bytes()
    }

    #[test]
    fn file_reads_selected_range_without_removing_source() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"prefix\x01\x02\x03tail").unwrap();
        let transfer = file_transfer(&command("a=q,t=f,f=24,s=1,v=1,i=17,S=3,O=6", file.path()))
            .unwrap()
            .unwrap();
        assert_eq!(transfer.data, [1, 2, 3]);
        assert_eq!(transfer.control(b't'), Some(b"d".as_slice()));
        assert_eq!(transfer.control(b'S'), None);
        assert_eq!(transfer.control(b'O'), None);
        assert_eq!(fs::read(file.path()).unwrap(), b"prefix\x01\x02\x03tail");
        assert!(
            file_transfer(&command("a=q,t=f,f=24,s=1,v=1,i=17,O=6", file.path()))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn file_follows_symlinks_and_rejects_special_files_and_loops() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, [1, 2, 3]).unwrap();
        let link = root.path().join("link");
        symlink(&source, &link).unwrap();
        assert_eq!(
            file_transfer(&command("t=f,f=24,s=1,v=1", &link))
                .unwrap()
                .unwrap()
                .data,
            [1, 2, 3]
        );
        let fifo = root.path().join("fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
        let socket = root.path().join("socket");
        let _listener = UnixListener::bind(&socket).unwrap();
        let loop_path = root.path().join("loop");
        symlink(&loop_path, &loop_path).unwrap();
        for path in [
            root.path(),
            &fifo,
            &socket,
            &loop_path,
            Path::new("/dev/null"),
        ] {
            assert!(
                file_transfer(&command("t=f,f=24,s=1,v=1", path))
                    .unwrap()
                    .is_err()
            );
        }
    }

    #[test]
    fn file_rejects_invalid_ranges_and_bounds_reads_before_allocation() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&[1, 2, 3]).unwrap();
        for range in [
            "S=0",
            "S=4",
            "O=4",
            "O=18446744073709551615,S=1",
            "O=-1",
            "S=18446744073709551616",
        ] {
            assert!(
                file_transfer(&command(&format!("t=f,f=24,s=1,v=1,{range}"), file.path()))
                    .unwrap()
                    .is_err()
            );
        }
        file.as_file()
            .set_len(super::super::MAX_DIRECT_PNG_TRANSFER_BYTES as u64 + 1)
            .unwrap();
        for format in [24, 100] {
            assert!(
                file_transfer(&command(&format!("t=f,f={format},s=1,v=1"), file.path()))
                    .unwrap()
                    .is_err()
            );
        }
        assert_eq!(
            file_transfer(&command("t=f,f=24,s=1,v=1,S=3", file.path()))
                .unwrap()
                .unwrap()
                .data,
            [1, 2, 3]
        );
        assert!(file.path().exists());
    }

    #[test]
    fn file_raw_and_png_zlib_data_validate_complete_compressed_streams() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[1, 2, 3])
                .unwrap();
        }
        for (format, pixels) in [
            ("f=24,s=1,v=1", b"\x01\x02\x03".as_slice()),
            ("f=100", png.as_slice()),
        ] {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(pixels).unwrap();
            let compressed = encoder.finish().unwrap();
            let mut file = NamedTempFile::new().unwrap();
            file.write_all(&compressed).unwrap();
            let transfer = file_transfer(&command(
                &format!("t=f,o=z,{format},S={}", compressed.len()),
                file.path(),
            ))
            .unwrap()
            .unwrap();
            assert_eq!(transfer.data, pixels);
            file.write_all(b"trailing").unwrap();
            assert!(
                file_transfer(&command(&format!("t=f,o=z,{format}"), file.path()))
                    .unwrap()
                    .is_err()
            );
            fs::write(file.path(), &compressed[..compressed.len() - 1]).unwrap();
            assert!(
                file_transfer(&command(&format!("t=f,o=z,{format}"), file.path()))
                    .unwrap()
                    .is_err()
            );
        }
    }

    #[test]
    fn file_compressed_png_expansion_is_bounded_independently_of_file_size() {
        let mut file = NamedTempFile::new().unwrap();
        {
            let mut encoder = ZlibEncoder::new(file.as_file_mut(), Compression::fast());
            let chunk = [0; 64 * 1024];
            for _ in 0..super::super::MAX_DIRECT_PNG_TRANSFER_BYTES / chunk.len() {
                encoder.write_all(&chunk).unwrap();
            }
            encoder.write_all(&[0]).unwrap();
            encoder.finish().unwrap();
        }
        assert!(file.as_file().metadata().unwrap().len() < 1024 * 1024);
        assert!(
            file_transfer(&command("t=f,f=100,o=z", file.path()))
                .unwrap()
                .is_err()
        );
        assert!(file.path().exists());
    }
}

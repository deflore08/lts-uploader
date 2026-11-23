use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

pub const CHUNK_BYTES: u64 = 1_048_576;
const PREFIX: &[u8] = b"replay-source-fp-v1\0";

pub fn source_fingerprint(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|err| err.to_string())?;
    let size = meta.len();
    let mut file = open_read_shared(path)?;
    let (head, tail) = read_head_tail(&mut file, size)?;
    Ok(hash_parts(size, &head, &tail))
}

fn hash_parts(size: u64, head: &[u8], tail: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX);
    hasher.update(size.to_be_bytes());
    hasher.update(head);
    hasher.update(tail);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_head_tail(file: &mut File, size: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
    let read_len = size.min(CHUNK_BYTES);
    if read_len == 0 {
        return Err("empty file".into());
    }
    let mut head = vec![0u8; read_len as usize];
    file.seek(SeekFrom::Start(0)).map_err(|err| err.to_string())?;
    file.read_exact(&mut head).map_err(|err| err.to_string())?;
    let tail_start = size.saturating_sub(read_len);
    let mut tail = vec![0u8; read_len as usize];
    file.seek(SeekFrom::Start(tail_start))
        .map_err(|err| err.to_string())?;
    file.read_exact(&mut tail).map_err(|err| err.to_string())?;
    Ok((head, tail))
}

fn open_read_shared(path: &Path) -> Result<File, String> {
    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x00000001;
        const FILE_SHARE_WRITE: u32 = 0x00000002;
        const FILE_SHARE_DELETE: u32 = 0x00000004;
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .map_err(|err| err.to_string())
    }
    #[cfg(not(windows))]
    {
        File::open(path).map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn small_file_duplicates_head_and_tail() {
        let dir = std::env::temp_dir().join(format!(
            "lts-fp-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("tiny.bin");
        let payload = b"hello-replay-fp";
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(payload).unwrap();
        drop(file);
        let size = payload.len() as u64;
        let fp = source_fingerprint(&path).unwrap();
        let mut file = File::open(&path).unwrap();
        let (head, tail) = read_head_tail(&mut file, size).unwrap();
        assert_eq!(head, tail);
        assert_eq!(fp, hash_parts(size, &head, &tail));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_file_errors() {
        let dir = std::env::temp_dir().join(format!("lts-fp-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("empty.bin");
        std::fs::File::create(&path).unwrap();
        assert!(source_fingerprint(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
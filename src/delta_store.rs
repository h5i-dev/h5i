use crate::error::H5iError;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

pub struct DeltaStore {
    log_path: PathBuf,
}

impl DeltaStore {
    pub fn new(repo_root: PathBuf, file_path: &str) -> Self {
        let hash = sha256_hash(file_path); // ファイルパスをハッシュ化してファイル名に
        let log_path = repo_root.join(".h5i/delta").join(format!("{}.bin", hash));
        Self { log_path }
    }

    /// 自分の更新分を追記する
    pub fn append_update(&self, data: &[u8]) -> Result<(), H5iError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        // [データ長(u32)][バイナリデータ] の形式で保存
        let len = data.len() as u32;
        file.write_all(&len.to_le_bytes())?;
        file.write_all(data)?;
        Ok(())
    }

    /// 全ての操作ログを読み出す
    pub fn read_all_updates(&self) -> Result<Vec<Vec<u8>>, H5iError> {
        if !self.log_path.exists() {
            return Ok(vec![]);
        }
        let mut file = File::open(&self.log_path)?;
        let mut updates = Vec::new();

        loop {
            let mut len_buf = [0u8; 4];
            if file.read_exact(&mut len_buf).is_err() {
                break;
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut data = vec![0u8; len];
            file.read_exact(&mut data)?;
            updates.push(data);
        }
        Ok(updates)
    }
}

fn sha256_hash(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

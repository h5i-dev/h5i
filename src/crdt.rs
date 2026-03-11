use yrs::{Doc, GetString, Transact};

use crate::error::H5iError;

pub struct CrdtSession {
    doc: Doc,
    file_id: String,
}

impl CrdtSession {
    pub fn new(file_id: &str) -> Self {
        CrdtSession {
            doc: Doc::new(),
            file_id: file_id.to_string(),
        }
    }

    /// 現在のテキスト状態を取得
    pub fn get_content(&self) -> String {
        let text = self.doc.get_or_insert_text("content");
        text.get_string(&self.doc.transact())
    }

    /// 外部からの更新（他のAgentや人間）をマージ
    pub fn apply_update(&mut self, update: Vec<u8>) -> Result<(), H5iError> {
        use yrs::updates::decoder::Decode;
        use yrs::Update;

        let mut txn = self.doc.transact_mut();
        let update = Update::decode_v1(&update)?;
        txn.apply_update(update)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::LocalAgentSession;
    use std::fs;
    use tempfile::tempdir;
    use yrs::updates::decoder::Decode;
    use yrs::{Doc, Text, Transact};

    #[test]
    fn test_session_initialization_and_flush() -> crate::error::Result<()> {
        let dir = tempdir().unwrap();
        let temp_path = dir.path();
        let repo_root = temp_path.to_path_buf();
        let file_path = repo_root.join("test_file.py");

        // Setup: Source file MUST exist
        fs::write(&file_path, "print('hello')")?;

        // Fix: Pass the existing code path and the repo root
        let mut session = LocalAgentSession::new(repo_root.clone(), file_path.clone())?;

        // Edit and Flush
        {
            let mut txn = session.doc.transact_mut();
            session.text_ref.insert(&mut txn, 14, "\nprint('world')");
        }
        session.flush_and_sync_file()?;

        let updated_content = fs::read_to_string(&file_path)?;
        assert!(updated_content.contains("hello"));
        assert!(updated_content.contains("world"));

        let delta_path = repo_root.join(".h5i/delta");
        assert!(delta_path.exists());
        /*
        assert!(delta_path.exists());
        let delta_size = fs::metadata(&delta_path)?.len();
        assert!(
            delta_size > 0,
            "Delta store should have recorded the update"
        );*/

        Ok(())
    }

    #[test]
    fn test_concurrent_conflict_resolution_simulation() -> crate::error::Result<()> {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("code.rs");
        let delta_path = dir.path().join("log.bin");

        fs::write(&file_path, "fn main() {}")?;

        // Create session
        let mut session = LocalAgentSession::new(file_path, delta_path)?;

        // Create an external update (simulating another agent)
        let remote_doc = Doc::new();
        let remote_text = remote_doc.get_or_insert_text("content");
        let remote_update = {
            let mut txn = remote_doc.transact_mut();
            remote_text.insert(&mut txn, 0, "// Agent Alpha\n");
            txn.encode_update_v1()
        };

        // Apply remote update to our local session
        {
            let mut txn = session.doc.transact_mut();
            txn.apply_update(yrs::Update::decode_v1(&remote_update)?);
        }

        // Verify local merge
        let final_text = session.get_current_text();
        assert!(final_text.starts_with("// Agent Alpha"));
        assert!(final_text.contains("fn main()"));

        Ok(())
    }

    #[test]
    fn test_error_on_missing_file() {
        let dir = tempdir().unwrap();
        let non_existent = dir.path().join("ghost.txt");
        let delta = dir.path().join("delta.bin");

        let result = LocalAgentSession::new(non_existent, delta);

        // Verify that our H5iError::Io or InvalidPath is returned
        assert!(result.is_err());
    }
}

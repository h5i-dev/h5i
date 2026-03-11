use yrs::{Doc, GetString, Text, TextRef, Transact};

use crate::error::H5iError;
use crate::repository::H5iRepository;

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

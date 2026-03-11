use crate::delta_store::DeltaStore;
use std::fs;
use std::path::PathBuf;
use yrs::updates::decoder::Decode;
use yrs::{Doc, GetString, Text, TextRef, Transact, Update};

pub struct LocalAgentSession {
    doc: Doc,
    text_ref: TextRef,
    delta_store: DeltaStore,
    target_fs_path: PathBuf,
}

// src/session.rs

impl LocalAgentSession {
    /// コミット直前に呼び出し、未保存の CRDT 変更を強制的にログへ書き出す
    pub fn flush_and_sync_file(&mut self) -> Result<(), crate::error::H5iError> {
        // 現在のドキュメントの差分をエンコード
        let mut txn = self.doc.transact_mut(); // y-crdt のトランザクション
        let update = txn.encode_update_v1(); // 最新の更新分を取得

        // 共有バイナリログ (.h5i/delta/...) に追記
        self.delta_store.append_update(&update)?;

        // 最新のテキストを実際のファイルに反映
        let final_text = self.text_ref.get_string(&txn);
        std::fs::write(&self.target_fs_path, final_text)?;

        Ok(())
    }
}

impl LocalAgentSession {
    pub fn new(repo_root: PathBuf, target_path: PathBuf) -> Result<Self, crate::error::H5iError> {
        let doc = Doc::new();
        let text_ref = doc.get_or_insert_text("code");
        let delta_store = DeltaStore::new(repo_root, target_path.to_str().unwrap());

        let mut session = Self {
            doc,
            text_ref,
            delta_store,
            target_fs_path: target_path,
        };

        // 起動時に既存の操作ログを全て適用して最新状態にする
        session.sync_from_disk()?;
        Ok(session)
    }

    /// 他のエージェントの変更をディスクから読み取ってマージ
    pub fn sync_from_disk(&mut self) -> Result<(), crate::error::H5iError> {
        let updates = self.delta_store.read_all_updates()?;
        let mut txn = self.doc.transact_mut();
        for data in updates {
            let update = Update::decode_v1(&data)?;
            txn.apply_update(update)?;
        }
        Ok(())
    }

    /// 自分の編集を適用し、即座にディスクへ書き出す
    pub fn apply_local_edit(
        &mut self,
        offset: u32,
        content: &str,
    ) -> Result<(), crate::error::H5iError> {
        // 1. yrs 上で編集
        let mut txn = self.doc.transact_mut();

        // 編集前の状態ベクトルを取得（差分抽出用）
        // (yrs の v1 update を直接取得するために observe を使う手法も一般的)
        self.text_ref.insert(&mut txn, offset, content);

        // 2. 差分(Update)を抽出して共有ログに保存
        // 本来はトランザクション中に発生した差分だけを抽出
        let update = txn.encode_update_v1();
        self.delta_store.append_update(&update)?;

        // 3. 実際のソースコードファイルにマッピング（人間やLinterが見る場所）
        let merged_text = self.text_ref.get_string(&txn);
        fs::write(&self.target_fs_path, merged_text)?;

        Ok(())
    }
}

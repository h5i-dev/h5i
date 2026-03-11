use crate::error::Result;
use crate::session::LocalSession;
use notify::{Config, EventKind, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::Duration;

pub fn start_h5i_watcher(mut session: LocalSession) -> Result<()> {
    let (tx, rx) = channel();

    // デバウンス（頻繁な書き込みを抑制）のために 500ms のディレイを設定
    let mut watcher = notify::RecommendedWatcher::new(tx, Config::default())
        .map_err(|e| crate::error::H5iError::Internal(e.to_string()))?;

    watcher
        .watch(&session.target_fs_path, RecursiveMode::NonRecursive)
        .map_err(|e| crate::error::H5iError::Internal(e.to_string()))?;

    println!("Watcher started for {:?}", session.target_fs_path);

    for res in rx {
        match res {
            Ok(event) => {
                // ファイルの修正（保存）イベントのみを対象にする
                if let EventKind::Modify(_) = event.kind {
                    println!("Change detected. Syncing to h5i...");

                    // 1. ディスクの最新状態を CRDT にマージ
                    // (人間がエディタで保存した内容を取り込む)
                    session.sync_from_disk(&session.target_fs_path.clone())?;

                    // 2. デルタログを追記し、内部状態を整理
                    session.flush_and_sync_file()?;
                }
            }
            Err(e) => println!("watch error: {:?}", e),
        }
    }
    Ok(())
}

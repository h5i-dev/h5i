use crate::error::Result;
use crate::session::LocalSession;
use notify::{Config, EventKind, RecursiveMode, Watcher};
use std::io::Write;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

pub fn start_h5i_watcher(session: Arc<Mutex<LocalSession>>) -> Result<()> {
    let (tx, rx) = channel();

    // デバウンス（頻繁な書き込みを抑制）のために 500ms のディレイを設定
    let mut watcher = notify::RecommendedWatcher::new(tx, Config::default())
        .map_err(|e| crate::error::H5iError::Internal(e.to_string()))?;

    let target_path = {
        let sess = session.lock().unwrap();
        sess.target_fs_path.clone()
    };

    watcher
        .watch(&target_path, RecursiveMode::NonRecursive)
        .map_err(|e| crate::error::H5iError::Internal(e.to_string()))?;

    for res in rx {
        match res {
            Ok(event) => {
                // ファイルの修正（保存）イベントのみを対象にする
                {
                    // if let EventKind::Modify(_) = event.kind {
                    // println!("Change detected. Syncing to h5i...");
                    let mut sess = session.lock().unwrap();
                    let path = sess.target_fs_path.clone();
                    if let Err(e) = sess.force_ingest_from_disk() {
                        eprintln!("Sync error: {:?}", e);
                    }
                    // 2. デルタログを追記し、内部状態を整理
                    // sess.flush_and_sync_file()?;
                    //}
                }
            }
            Err(e) => println!("watch error: {:?}", e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod watcher_tests {
    use super::*;
    use crate::session::LocalSession;
    use std::fs;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    /// Helper to wait for the CRDT text to match an expected string within a timeout.
    fn wait_for_content(
        session: Arc<Mutex<LocalSession>>,
        expected: &str,
        timeout: Duration,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(s) = session.try_lock() {
                if s.get_current_text() == expected {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
        false
    }

    /*
    #[test]
    fn test_watcher_handles_rapid_consecutive_writes() -> crate::error::Result<()> {
        let dir = tempdir().unwrap();
        let repo_root = dir.path().to_path_buf();
        let file_path = repo_root.join("rapid.txt");

        fs::write(&file_path, "v0")?;
        let session = LocalSession::new(repo_root.clone(), file_path.clone(), 1)?;
        let session_arc = Arc::new(Mutex::new(session));

        let watcher_session = Arc::clone(&session_arc);
        std::thread::spawn(move || {
            let _ = start_h5i_watcher(watcher_session);
        });

        std::thread::sleep(Duration::from_millis(200));

        // Simulate rapid-fire saves from an IDE
        fs::write(&file_path, "v1")?;
        std::thread::sleep(Duration::from_millis(10));
        fs::write(&file_path, "v2")?;
        std::thread::sleep(Duration::from_millis(10));
        fs::write(&file_path, "v3 final")?;

        let success =
            wait_for_content(Arc::clone(&session_arc), "v3 final", Duration::from_secs(3));
        assert!(success, "Watcher dropped events during rapid writes.");
        Ok(())
    }*/

    #[test]
    fn test_watcher_ingests_external_edits() -> crate::error::Result<()> {
        let dir = tempdir().unwrap();
        let repo_root = dir.path().to_path_buf();
        let file_path = repo_root.join("code.py");
        fs::write(&file_path, "initial")?;

        let session = LocalSession::new(repo_root, file_path.clone(), 1)?;
        let session_arc = Arc::new(Mutex::new(session));

        // Watcherを別スレッドで起動 (Arcを渡す)
        let watcher_session = Arc::clone(&session_arc);
        std::thread::spawn(move || {
            let _ = start_h5i_watcher(watcher_session);
        });

        // 監視が開始されるのを待機
        std::thread::sleep(Duration::from_millis(100));

        // 外部エディタによる書き込みをシミュレート
        let f = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file_path)?;
        let mut writer = std::io::BufWriter::new(f);
        std::io::Write::write_all(&mut writer, b"updated content")?;
        writer.flush()?;
        // ディスクへのフラッシュを確実に
        writer.into_inner().unwrap().sync_all()?;

        fs::write(&file_path, "updated content")?;
        //writer.into_inner().unwrap().sync_all()?;

        // 検証: メインスレッドでロックを取得できるようになる！
        let success = wait_for_content(
            Arc::clone(&session_arc),
            "updated content",
            Duration::from_secs(1),
        );

        assert!(success, "Deadlock broken, but content sync failed.");
        Ok(())
    }

    #[cfg(test)]
    mod persistence_tests {
        use super::*;
        use std::fs;
        use tempfile::tempdir;

        #[test]
        fn test_save_current_state_persistence_and_recovery() -> crate::error::Result<()> {
            let dir = tempdir().unwrap();
            let repo_root = dir.path().to_path_buf();
            let file_path = repo_root.join("persist_test.txt");

            // 1. Initial creation
            fs::write(&file_path, "v1 original")?;
            let mut session_1 = LocalSession::new(repo_root.clone(), file_path.clone(), 1)?;

            // 2. Modify and Save to Delta
            session_1.apply_local_edit(11, " + v2 edited")?;
            session_1.save_current_state_to_delta()?;

            let expected_text = "v1 original + v2 edited";
            assert_eq!(session_1.get_current_text(), expected_text);

            // 3. Simulate "Crash/Restart" - Create a new session in the same path
            // It should NOT read the physical file (which is still "v1 original")
            // but instead hydrate from the DeltaStore we just saved.
            let session_2 = LocalSession::new(repo_root, file_path, 2)?;

            assert_eq!(
                session_2.get_current_text(),
                expected_text,
                "Recovery failed: Session did not reconstruct state from the saved delta log."
            );

            Ok(())
        }
    }
}

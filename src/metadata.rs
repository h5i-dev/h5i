use chrono::{TimeZone, Utc};
use git2::Oid;
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct H5iCommitRecord {
    pub git_oid: String,
    pub parent_oid: Option<String>,
    pub ai_metadata: Option<AiMetadata>,
    pub test_metrics: Option<TestMetrics>,
    /// ファイルパス -> 外部から提供された AST (S式) のハッシュ
    pub ast_hashes: Option<HashMap<String, String>>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AiMetadata {
    pub model_name: String,
    pub prompt_hash: String,
    pub agent_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TestMetrics {
    pub test_suite_hash: String,
    pub coverage: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommitProvenance {
    pub commit_oid: String,
    pub ai_metadata: Option<AiMetadata>,
    pub test_metrics: Option<TestMetrics>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl H5iCommitRecord {
    /// Git の標準情報から最小限のレコードを作成する。
    /// .h5i メタデータが存在しない古いコミットを表示する際のフォールバックとして使用。
    pub fn minimal_from_git(repo: &Repository, oid: Oid) -> Self {
        // コミットオブジェクトを取得
        // 実戦では find_commit が失敗する可能性（浅いクローン等）も考慮し、
        // 呼び出し元で Result を扱う設計にするのが理想的ですが、ここでは簡略化しています。
        let commit = repo.find_commit(oid).expect("Commit not found");

        // 親コミットの OID を取得 (最初の親のみを対象とする)
        let parent_oid = if commit.parent_count() > 0 {
            Some(commit.parent_id(0).unwrap_or(Oid::zero()).to_string())
        } else {
            None
        };

        // Git のタイムスタンプを chrono::DateTime<Utc> に変換
        let time = commit.time();
        let timestamp = Utc
            .timestamp_opt(time.seconds(), 0)
            .single()
            .unwrap_or_else(Utc::now);

        H5iCommitRecord {
            git_oid: oid.to_string(),
            parent_oid,
            ai_metadata: None,  // Git 標準コミットには AI 情報はない
            test_metrics: None, // Git 標準コミットには品質データはない
            ast_hashes: None,   // Git 標準コミットには AST ハッシュはない
            timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use git2::{Oid, Repository, Signature};
    use tempfile::tempdir;

    /// Gitリポジトリとコミットを作成するためのテストヘルパー
    fn setup_git_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().expect("Failed to create temp dir");
        let repo = Repository::init(dir.path()).expect("Failed to init repo");
        (dir, repo)
    }

    /// ダミーのコミットを作成するヘルパー
    fn create_dummy_commit(repo: &Repository, message: &str, parents: &[&git2::Commit]) -> Oid {
        let sig = Signature::now("H5i Test", "test@h5i.io").expect("Failed to create signature");
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, parents)
            .expect("Failed to create commit")
    }

    #[test]
    fn test_minimal_from_git_root_commit() {
        let (_dir, repo) = setup_git_repo();

        // 1. 親なしの最初（Root）のコミットを作成
        let root_oid = create_dummy_commit(&repo, "Initial commit", &[]);

        // 2. テスト対象関数の実行
        let record = H5iCommitRecord::minimal_from_git(&repo, root_oid);

        // 3. 検証
        assert_eq!(record.git_oid, root_oid.to_string());
        assert_eq!(
            record.parent_oid, None,
            "Root commit should not have a parent"
        );
        assert!(record.ai_metadata.is_none());
        assert!(record.test_metrics.is_none());
        assert!(record.ast_hashes.is_none());

        // タイムスタンプが極端に離れていないか（数秒以内の誤差を許容）
        let now = Utc::now().timestamp();
        assert!((record.timestamp.timestamp() - now).abs() < 5);
    }

    #[test]
    fn test_minimal_from_git_child_commit() {
        let (_dir, repo) = setup_git_repo();

        // 1. Rootコミットを作成
        let root_oid = create_dummy_commit(&repo, "Root", &[]);
        let root_commit = repo.find_commit(root_oid).unwrap();

        // 2. Childコミットを作成 (Rootを親に指定)
        let child_oid = create_dummy_commit(&repo, "Child", &[&root_commit]);

        // 3. テスト対象関数の実行
        let record = H5iCommitRecord::minimal_from_git(&repo, child_oid);

        // 4. 検証
        assert_eq!(record.git_oid, child_oid.to_string());
        assert_eq!(
            record.parent_oid,
            Some(root_oid.to_string()),
            "Child should correctly identify its first parent OID"
        );
    }

    #[test]
    fn test_timestamp_conversion_precision() {
        let (_dir, repo) = setup_git_repo();

        // 特定の時間を指定したシグネチャでコミット
        let fixed_time = 1700000000; // 2023-11-14頃
        let sig = Signature::new("Test", "test@h5i.io", &git2::Time::new(fixed_time, 0)).unwrap();

        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "Fixed time commit", &tree, &[])
            .unwrap();

        let record = H5iCommitRecord::minimal_from_git(&repo, oid);

        // chronoの変換が正確か検証
        assert_eq!(record.timestamp.timestamp(), fixed_time);
    }
}

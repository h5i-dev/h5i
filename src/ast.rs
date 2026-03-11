use sha2::{Digest, Sha256};

pub struct SemanticAst {
    pub raw_sexp: String,
    pub structure_hash: String,
}

impl SemanticAst {
    pub fn from_sexp(sexp: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(sexp.as_bytes());
        let structure_hash = format!("{:x}", hasher.finalize());

        SemanticAst {
            raw_sexp: sexp.to_string(),
            structure_hash,
        }
    }
}

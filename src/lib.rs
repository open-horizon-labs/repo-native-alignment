pub mod adr;
pub mod bootstrap;
pub mod bus;
pub mod business_context;
pub mod code;
pub mod consumers;
pub mod embed;
pub mod extract;
pub mod git;
pub mod graph;
pub mod markdown;
pub mod oh;
pub mod process;
pub mod query;
pub mod ranking;
#[cfg(feature = "embeddings")]
pub mod rerank;
#[cfg(not(feature = "embeddings"))]
pub mod rerank {
    use anyhow::{Result, anyhow};

    #[derive(Debug)]
    pub struct RerankCandidate {
        pub text: String,
        pub original_index: usize,
    }

    #[derive(Debug)]
    pub struct RerankedResult {
        pub original_index: usize,
        pub score: f32,
    }

    pub fn rerank_results(
        _query: &str,
        _candidates: &[RerankCandidate],
    ) -> Result<Vec<RerankedResult>> {
        Err(anyhow!(
            "rerank support is not compiled in; rebuild with --features embeddings"
        ))
    }

    pub fn rerank_results_strict(
        query: &str,
        candidates: &[RerankCandidate],
    ) -> Result<Vec<RerankedResult>> {
        rerank_results(query, candidates)
    }
}
pub mod roots;
pub mod scanner;
pub mod server;
pub mod service;
pub mod smoke_test;
pub mod structural_cache;
pub mod structural_cache_replay;
pub mod types;
pub mod walk;

pub mod lsp_completeness;
pub mod open_viewer;
pub mod setup;

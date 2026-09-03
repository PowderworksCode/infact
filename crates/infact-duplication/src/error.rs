use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Codebase(#[from] entl::codebase::Error),

    #[error(transparent)]
    Parser(#[from] entl_tree_sitter::Error),

    #[error("could not discover parser packs: {0}")]
    ParserCatalog(String),

    #[error("invalid exact-duplication configuration: {0}")]
    InvalidConfig(String),

    #[error("source file {path} is too large for token coordinates")]
    SourceTooLarge { path: PathBuf },
}

pub type Result<T> = std::result::Result<T, Error>;

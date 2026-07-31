//! Explicit command protocol for locally generated fact packs.

use std::path::Path;
use std::process::Command;

use infact_fact_pack::{CacheError, CachedFactPack, FactPackCache};
use thiserror::Error;

pub struct FactPackBuildRequest<'a> {
    pub ecosystem: &'a str,
    pub package: &'a str,
    pub version: &'a str,
    pub repository: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalFactPackBuilder {
    command: Vec<String>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("fact builder command cannot be empty")]
    EmptyCommand,
    #[error("could not run local fact builder `{command}`: {source}")]
    Execute {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("local fact builder `{command}` failed with status {status:?}: {stderr}")]
    Failed {
        command: String,
        status: Option<i32>,
        stderr: String,
    },
    #[error("creating fact-builder output directory: {0}")]
    Temporary(#[source] std::io::Error),
    #[error("importing local fact-builder OCI output: {0}")]
    Cache(#[from] CacheError),
}

impl ExternalFactPackBuilder {
    pub fn new(command: impl Into<Vec<String>>) -> Result<Self, Error> {
        let command = command.into();
        if command.is_empty() {
            return Err(Error::EmptyCommand);
        }
        Ok(Self { command })
    }

    pub fn command(&self) -> &[String] {
        &self.command
    }

    pub fn build(
        &self,
        request: &FactPackBuildRequest<'_>,
        cache: &FactPackCache,
    ) -> Result<CachedFactPack, Error> {
        let temporary = tempfile::tempdir().map_err(Error::Temporary)?;
        let output_path = temporary.path().join("oci");
        let output = Command::new(&self.command[0])
            .args(&self.command[1..])
            .arg("--ecosystem")
            .arg(request.ecosystem)
            .arg("--package")
            .arg(request.package)
            .arg("--version")
            .arg(request.version)
            .arg("--repository")
            .arg(request.repository)
            .arg("--output")
            .arg(&output_path)
            .current_dir(request.repository)
            .output()
            .map_err(|source| Error::Execute {
                command: self.command.join(" "),
                source,
            })?;
        if !output.status.success() {
            return Err(Error::Failed {
                command: self.command.join(" "),
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        cache.import_oci_layout(output_path).map_err(Into::into)
    }
}

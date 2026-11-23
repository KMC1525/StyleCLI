//! Download/localização do modelo base (GGUF) e do projetor de visão (mmproj).

use crate::{http, paths};
use anyhow::{Result, bail};
use clap::ValueEnum;
use std::path::{Path, PathBuf};

pub const REPO: &str = "darrellbest/Qwen3.5-4B-Heretic-GGUF";
const MMPROJ_FILE: &str = "Qwen3.5-4B-Heretic-mmproj-F16.gguf";
const MMPROJ_SIZE: u64 = 672_423_040;

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum Quant {
    /// 2,8 GB — recomendado para PCs fracos
    #[value(name = "q4_k_m")]
    Q4KM,
    /// 4,6 GB — mais fiel, exige mais RAM
    #[value(name = "q8_0")]
    Q80,
}

impl Quant {
    pub fn file(self) -> (&'static str, u64) {
        match self {
            Quant::Q4KM => ("Qwen3.5-4B-Heretic-Q4_K_M.gguf", 2_783_446_400),
            Quant::Q80 => ("Qwen3.5-4B-Heretic-Q8_0.gguf", 4_610_579_840),
        }
    }
}

pub struct ModelFiles {
    pub model: PathBuf,
    pub mmproj: PathBuf,
}

fn url(file: &str) -> String {
    format!("https://huggingface.co/{REPO}/resolve/main/{file}")
}

fn require(p: &Path) -> Result<PathBuf> {
    if !p.is_file() {
        bail!("arquivo não encontrado: {}", p.display());
    }
    Ok(p.to_path_buf())
}

/// Garante os dois arquivos do modelo (baixa só o que faltar; retoma downloads parciais).
pub fn ensure(quant: Quant, model: Option<&Path>, mmproj: Option<&Path>) -> Result<ModelFiles> {
    let dir = paths::models_dir()?;
    let model = match model {
        Some(p) => require(p)?,
        None => {
            let (file, size) = quant.file();
            let dest = dir.join(file);
            http::download(&url(file), &dest, Some(size), &format!("modelo {file}"))?;
            dest
        }
    };
    let mmproj = match mmproj {
        Some(p) => require(p)?,
        None => {
            let dest = dir.join(MMPROJ_FILE);
            http::download(
                &url(MMPROJ_FILE),
                &dest,
                Some(MMPROJ_SIZE),
                "projetor de visão",
            )?;
            dest
        }
    };
    Ok(ModelFiles { model, mmproj })
}

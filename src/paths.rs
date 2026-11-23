//! Diretórios de dados do StyleCLI (motor llama.cpp e modelos).

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Raiz de dados. Pode ser sobrescrita com `STYLE_HOME`.
pub fn home() -> Result<PathBuf> {
    let dir = match std::env::var_os("STYLE_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .context("não foi possível descobrir a pasta de dados do sistema; defina STYLE_HOME")?
            .join("stylecli"),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("criando {}", dir.display()))?;
    Ok(dir)
}

pub fn models_dir() -> Result<PathBuf> {
    let d = home()?.join("models");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

pub fn engine_dir() -> Result<PathBuf> {
    let d = home()?.join("engine");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

pub fn log_file() -> Result<PathBuf> {
    Ok(home()?.join("llama-server.log"))
}

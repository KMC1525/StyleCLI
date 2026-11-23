//! `style update`: atualiza o próprio executável a partir do último release no GitHub.

use crate::{engine, http};
use anyhow::{Context, Result, bail};
use semver::Version;
use std::path::PathBuf;

const REPO: &str = "KMC1525/StyleCLI";
const TARGET: &str = env!("STYLE_TARGET");
const BIN: &str = if cfg!(windows) { "style.exe" } else { "style" };

/// Nome do asset do release para este alvo de compilação.
pub fn asset_name(target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("style-{target}.{ext}")
}

fn parse_tag(tag: &str) -> Result<Version> {
    Version::parse(tag.trim().trim_start_matches('v'))
        .with_context(|| format!("versão inválida: {tag}"))
}

pub fn run(force: bool) -> Result<()> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    println!("style {current} ({TARGET})");
    println!("Procurando atualização...");

    // STYLE_RELEASE_API: só para testes (aponta para um servidor de releases falso)
    let api = std::env::var("STYLE_RELEASE_API")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let rel = http::get_json(&api)
        .context("não consegui consultar o último release (há release publicado e internet?)")?;
    let tag = rel["tag_name"].as_str().context("release sem tag_name")?;
    let latest = parse_tag(tag)?;

    if latest <= current && !force {
        println!("Você já está na versão mais recente ({current}).");
        return Ok(());
    }

    let want = asset_name(TARGET);
    let url = rel["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str() == Some(want.as_str()))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(str::to_string);
    let Some(url) = url else {
        bail!("o release {tag} não tem o arquivo '{want}' para a sua plataforma");
    };

    println!("Atualizando {current} → {latest}");
    let tmp = tempfile::tempdir()?;
    let archive = tmp.path().join(&want);
    http::download(&url, &archive, None, &format!("style {tag}"))?;
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out)?;
    engine::extract(&archive, &out)?;

    let new_bin: PathBuf = walkdir::WalkDir::new(&out)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name() == BIN)
        .map(|e| e.into_path())
        .with_context(|| format!("'{BIN}' não está em {want}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new_bin, std::fs::Permissions::from_mode(0o755))?;
    }
    self_replace::self_replace(&new_bin)
        .context("não consegui substituir o executável (permissão?)")?;
    println!("Pronto! style atualizado para {latest}.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_versions() {
        assert_eq!(
            asset_name("x86_64-pc-windows-msvc"),
            "style-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            asset_name("aarch64-apple-darwin"),
            "style-aarch64-apple-darwin.tar.gz"
        );
        assert!(parse_tag("v0.2.0").unwrap() > parse_tag("0.1.9").unwrap());
        assert!(parse_tag("lixo").is_err());
    }
}

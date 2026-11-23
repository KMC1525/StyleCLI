//! Cliente HTTP (ureq) + download com retomada e barra de progresso.

use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

pub const USER_AGENT: &str = concat!("stylecli/", env!("CARGO_PKG_VERSION"));

/// Agente para a internet (respeita HTTP(S)_PROXY, usa as CAs do sistema).
pub fn agent() -> Agent {
    Agent::config_builder()
        .user_agent(USER_AGENT)
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into()
}

/// Agente para o servidor local (127.0.0.1): nunca passa por proxy.
pub fn local_agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .proxy(None)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

pub fn get_json(url: &str) -> Result<serde_json::Value> {
    let mut resp = agent()
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("GET {url}"))?;
    resp.body_mut()
        .read_json()
        .with_context(|| format!("lendo JSON de {url}"))
}

/// Baixa `url` para `dest` (via `dest.part`), com retomada e progresso.
/// Se `expected` for informado, o tamanho final é verificado.
pub fn download(url: &str, dest: &Path, expected: Option<u64>, label: &str) -> Result<()> {
    if let (Some(exp), Ok(meta)) = (expected, std::fs::metadata(dest))
        && meta.len() == exp
    {
        return Ok(());
    }
    let part = dest.with_extension(
        dest.extension()
            .map(|e| format!("{}.part", e.to_string_lossy()))
            .unwrap_or_else(|| "part".into()),
    );
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if expected.is_some_and(|e| have >= e) {
        let _ = std::fs::remove_file(&part); // parcial corrompido
        have = 0;
    }

    let mut req = agent().get(url);
    if have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    let mut resp = req.call().with_context(|| format!("baixando {url}"))?;
    let status = resp.status().as_u16();
    let mut file: File = if status == 206 {
        OpenOptions::new().append(true).open(&part)?
    } else {
        have = 0; // servidor ignorou o Range: recomeça
        File::create(&part)?
    };
    let remaining: Option<u64> = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let total = remaining.map(|r| r + have).or(expected);

    let pb = match total {
        Some(t) => ProgressBar::new(t),
        None => ProgressBar::new_spinner(),
    };
    pb.set_style(
        ProgressStyle::with_template(
            "{msg:<28} [{bar:30.cyan/blue}] {bytes}/{total_bytes} {bytes_per_sec} ETA {eta}",
        )?
        .progress_chars("=> "),
    );
    pb.set_message(label.to_string());
    pb.set_position(have);

    let mut reader = resp.body_mut().as_reader();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .context("conexão interrompida (rode de novo para retomar)")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        pb.inc(n as u64);
    }
    file.flush()?;
    drop(file);
    pb.finish_and_clear();

    let size = std::fs::metadata(&part)?.len();
    if let Some(exp) = expected
        && size != exp
    {
        bail!("download de {label} incompleto ({size} de {exp} bytes); rode de novo para retomar");
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

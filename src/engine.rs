//! Motor de inferência: localiza/baixa o `llama-server` (llama.cpp) e o controla.
//! O llama.cpp roda o GGUF em CPU com pouca RAM e já traz suporte a visão (mmproj).

use crate::{http, paths};
use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LLAMA_RELEASE_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest";
pub const SERVER_BIN: &str = if cfg!(windows) {
    "llama-server.exe"
} else {
    "llama-server"
};

// ───────────────────────── localizar / instalar ─────────────────────────

/// Ordem: flag/env → instalação gerenciada → PATH → baixa a última release do llama.cpp.
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        bail!("--llama-server '{}' não existe", p.display());
    }
    let current = paths::engine_dir()?.join("current.txt");
    if let Ok(s) = std::fs::read_to_string(&current) {
        let p = PathBuf::from(s.trim());
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Some(p) = find_in_path(SERVER_BIN) {
        return Ok(p);
    }
    install_latest()
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

#[derive(Debug, Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// Escolhe o build **CPU** do llama.cpp para este SO/arquitetura (roda em qualquer PC).
/// Ignora builds de GPU/aceleradores (cuda, vulkan, hip, sycl...).
pub fn select_asset<'a>(os: &str, arch: &str, assets: &'a [Asset]) -> Option<&'a Asset> {
    const BAD: &[&str] = &[
        "cuda",
        "cudart",
        "vulkan",
        "hip",
        "rocm",
        "sycl",
        "opencl",
        "kleidiai",
        "openvino",
        "musa",
        "cann",
        "s390x",
        "ppc64",
        "riscv",
        "android",
        "xcframework",
        "ios",
        "arm64-v8",
    ];
    let os_tok = match os {
        "linux" => "ubuntu",
        "macos" => "macos",
        "windows" => "win",
        _ => return None,
    };
    let arch_tok = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return None,
    };
    assets
        .iter()
        .filter(|a| {
            let n = a.name.to_lowercase();
            n.starts_with("llama-")
                && n.contains("-bin-")
                && (n.ends_with(".zip") || n.ends_with(".tar.gz"))
                && n.contains(&format!("-{os_tok}-"))
                && n.contains(&format!("-{arch_tok}."))
                && !BAD.iter().any(|b| n.contains(b))
        })
        .min_by_key(|a| {
            let n = a.name.to_lowercase();
            // Windows: o build "cpu" despacha a instrução certa em tempo de execução;
            // os antigos (noavx < avx < avx2) vêm depois, do mais compatível ao mais exigente.
            let variant = if n.contains("-cpu-") {
                0
            } else if n.contains("noavx") {
                1
            } else if n.contains("-avx-") {
                2
            } else if n.contains("avx2") {
                3
            } else if n.contains("avx512") {
                9
            } else {
                0
            };
            (variant, n.len())
        })
}

fn install_latest() -> Result<PathBuf> {
    eprintln!("Motor de inferência (llama.cpp) não encontrado — baixando a versão mais recente...");
    let rel = http::get_json(LLAMA_RELEASE_API)?;
    let tag = rel["tag_name"]
        .as_str()
        .context("release sem tag_name")?
        .to_string();
    let assets: Vec<Asset> = rel["assets"]
        .as_array()
        .context("release sem assets")?
        .iter()
        .filter_map(|a| {
            Some(Asset {
                name: a["name"].as_str()?.to_string(),
                url: a["browser_download_url"].as_str()?.to_string(),
            })
        })
        .collect();
    let asset = select_asset(std::env::consts::OS, std::env::consts::ARCH, &assets)
        .with_context(|| {
            format!(
                "não achei build do llama.cpp {tag} para {}/{}. Instale o llama.cpp e use --llama-server",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;

    let dest_dir = paths::engine_dir()?.join(&tag);
    let archive = paths::engine_dir()?.join(&asset.name);
    http::download(&asset.url, &archive, None, &format!("llama.cpp {tag}"))?;
    let _ = std::fs::remove_dir_all(&dest_dir);
    std::fs::create_dir_all(&dest_dir)?;
    extract(&archive, &dest_dir).with_context(|| format!("extraindo {}", asset.name))?;
    let _ = std::fs::remove_file(&archive);

    let server = walkdir::WalkDir::new(&dest_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name() == SERVER_BIN)
        .map(|e| e.into_path())
        .with_context(|| format!("{SERVER_BIN} não veio no pacote {}", asset.name))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::write(
        paths::engine_dir()?.join("current.txt"),
        server.to_string_lossy().as_bytes(),
    )?;
    eprintln!("llama.cpp {tag} instalado.");
    Ok(server)
}

pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive.to_string_lossy().to_lowercase();
    if name.ends_with(".zip") {
        zip::ZipArchive::new(File::open(archive)?)?.extract(dest)?;
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let gz = flate2::read::GzDecoder::new(File::open(archive)?);
        tar::Archive::new(gz).unpack(dest)?;
    } else {
        bail!("formato de pacote não suportado: {}", archive.display());
    }
    Ok(())
}

// ───────────────────────── processo do servidor ─────────────────────────

pub struct ServerCfg {
    pub exe: PathBuf,
    pub model: PathBuf,
    pub mmproj: PathBuf,
    pub ctx: u32,
    pub threads: usize,
    pub gpu_layers: u32,
    pub startup_timeout: Duration,
}

pub struct Server {
    child: Arc<Mutex<Child>>,
    pub base: String,
}

impl Server {
    /// Sobe o llama-server e espera o modelo carregar. Se a versão do llama.cpp for
    /// antiga e recusar flags opcionais, tenta de novo sem elas.
    pub fn start(cfg: &ServerCfg) -> Result<Server> {
        match Self::spawn(cfg, true) {
            Ok(s) => Ok(s),
            Err(StartError::EarlyExit(_)) => Self::spawn(cfg, false).map_err(Into::into),
            Err(e) => Err(e.into()),
        }
    }

    pub fn child_handle(&self) -> Arc<Mutex<Child>> {
        self.child.clone()
    }

    /// `Some(status)` se o processo já morreu.
    pub fn exited(&self) -> Option<std::process::ExitStatus> {
        self.child.lock().ok()?.try_wait().ok().flatten()
    }

    fn spawn(cfg: &ServerCfg, optional: bool) -> std::result::Result<Server, StartError> {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0")
                .map_err(|e| StartError::Other(e.into()))?;
            l.local_addr()
                .map_err(|e| StartError::Other(e.into()))?
                .port()
        };
        let log_path = paths::log_file().map_err(StartError::Other)?;
        let log = File::create(&log_path).map_err(|e| StartError::Other(e.into()))?;

        let mut cmd = Command::new(&cfg.exe);
        cmd.arg("-m")
            .arg(&cfg.model)
            .arg("--mmproj")
            .arg(&cfg.mmproj)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-c", &cfg.ctx.to_string()])
            .args(["-t", &cfg.threads.to_string()])
            .args(["-ngl", &cfg.gpu_layers.to_string()])
            .args(["-np", "1", "--jinja"]);
        if optional {
            if cfg.gpu_layers == 0 {
                cmd.arg("--no-mmproj-offload"); // visão também na CPU, sem tentar GPU
            }
            // sem cache de prompt em RAM e sem aquecimento: menos memória e partida mais rápida
            cmd.args(["--cache-ram", "0", "--no-warmup"]);
        }
        if let Some(dir) = cfg.exe.parent() {
            let var = if cfg!(target_os = "macos") {
                "DYLD_LIBRARY_PATH"
            } else {
                "LD_LIBRARY_PATH"
            };
            if !cfg!(windows) {
                let prev = std::env::var_os(var).unwrap_or_default();
                let mut v = std::env::split_paths(&prev).collect::<Vec<_>>();
                v.insert(0, dir.to_path_buf());
                if let Ok(joined) = std::env::join_paths(v) {
                    cmd.env(var, joined);
                }
            }
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|e| StartError::Other(e.into()))?,
            ))
            .stderr(Stdio::from(log));
        detach(&mut cmd);

        let child = cmd
            .spawn()
            .with_context(|| format!("iniciando {}", cfg.exe.display()))
            .map_err(StartError::Other)?;
        let server = Server {
            child: Arc::new(Mutex::new(child)),
            base: format!("http://127.0.0.1:{port}"),
        };

        let spin = ProgressBar::new_spinner();
        spin.set_style(ProgressStyle::with_template("{spinner} {msg} ({elapsed})").unwrap());
        spin.set_message("Carregando o modelo na memória (só na primeira imagem é lento)...");
        spin.enable_steady_tick(Duration::from_millis(120));

        let agent = http::local_agent(Duration::from_secs(5));
        let t0 = Instant::now();
        let result = loop {
            if let Some(st) = server.exited() {
                break Err(StartError::EarlyExit(format!(
                    "llama-server encerrou ({st}).\n{}",
                    tail(&log_path, 15)
                )));
            }
            if let Ok(r) = agent.get(&format!("{}/health", server.base)).call()
                && r.status().as_u16() == 200
            {
                break Ok(());
            }
            if t0.elapsed() > cfg.startup_timeout {
                break Err(StartError::Other(anyhow::anyhow!(
                    "o modelo não ficou pronto em {}s.\n{}",
                    cfg.startup_timeout.as_secs(),
                    tail(&log_path, 15)
                )));
            }
            std::thread::sleep(Duration::from_millis(400));
        };
        spin.finish_and_clear();
        result.map(|_| server) // em erro, `server` é descartado e o Drop mata o processo
    }

    /// Últimas linhas do log do servidor (para mensagens de erro).
    pub fn log_tail(&self, n: usize) -> String {
        paths::log_file().map(|p| tail(&p, n)).unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

enum StartError {
    EarlyExit(String),
    Other(anyhow::Error),
}

impl From<StartError> for anyhow::Error {
    fn from(e: StartError) -> Self {
        match e {
            StartError::EarlyExit(s) => anyhow::anyhow!(s),
            StartError::Other(e) => e,
        }
    }
}

/// O servidor fica em grupo de processos próprio (Ctrl+C no terminal não o mata no meio
/// de uma imagem) e, no Linux, morre junto se o `style` for encerrado à força.
fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt as _;
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0200); // NO_WINDOW | NEW_PROCESS_GROUP
    }
}

/// Últimas `n` linhas de um arquivo (lê só o final).
pub fn tail(path: &Path, n: usize) -> String {
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(16 * 1024);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let s = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Convenience para o JSON de resposta de erro do servidor.
pub fn error_message(v: &Value) -> String {
    v.pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(names: &[&str]) -> Vec<Asset> {
        names
            .iter()
            .map(|n| Asset {
                name: n.to_string(),
                url: format!("https://x/{n}"),
            })
            .collect()
    }

    const LIST: &[&str] = &[
        "cudart-llama-bin-win-cuda-12.4-x64.zip",
        "llama-b9000-bin-macos-arm64.tar.gz",
        "llama-b9000-bin-macos-x64.tar.gz",
        "llama-b9000-bin-ubuntu-arm64.tar.gz",
        "llama-b9000-bin-ubuntu-vulkan-x64.tar.gz",
        "llama-b9000-bin-ubuntu-x64.tar.gz",
        "llama-b9000-bin-win-cpu-arm64.zip",
        "llama-b9000-bin-win-cpu-x64.zip",
        "llama-b9000-bin-win-cuda-12.4-x64.zip",
        "llama-b9000-bin-win-hip-radeon-x64.zip",
        "llama-b9000-bin-win-vulkan-x64.zip",
        "llama-b9000-xcframework.zip",
    ];

    fn pick(os: &str, arch: &str, list: &[&str]) -> Option<String> {
        select_asset(os, arch, &assets(list)).map(|a| a.name.clone())
    }

    #[test]
    fn picks_cpu_builds() {
        assert_eq!(
            pick("linux", "x86_64", LIST).unwrap(),
            "llama-b9000-bin-ubuntu-x64.tar.gz"
        );
        assert_eq!(
            pick("linux", "aarch64", LIST).unwrap(),
            "llama-b9000-bin-ubuntu-arm64.tar.gz"
        );
        assert_eq!(
            pick("macos", "aarch64", LIST).unwrap(),
            "llama-b9000-bin-macos-arm64.tar.gz"
        );
        assert_eq!(
            pick("macos", "x86_64", LIST).unwrap(),
            "llama-b9000-bin-macos-x64.tar.gz"
        );
        assert_eq!(
            pick("windows", "x86_64", LIST).unwrap(),
            "llama-b9000-bin-win-cpu-x64.zip"
        );
        assert_eq!(
            pick("windows", "aarch64", LIST).unwrap(),
            "llama-b9000-bin-win-cpu-arm64.zip"
        );
    }

    #[test]
    fn old_naming_prefers_most_compatible_windows_build() {
        let old = [
            "llama-b6000-bin-win-avx2-x64.zip",
            "llama-b6000-bin-win-avx512-x64.zip",
            "llama-b6000-bin-win-noavx-x64.zip",
            "llama-b6000-bin-win-avx-x64.zip",
            "llama-b6000-bin-ubuntu-x64.zip",
            "llama-b6000-bin-macos-arm64.zip",
        ];
        assert_eq!(
            pick("windows", "x86_64", &old).unwrap(),
            "llama-b6000-bin-win-noavx-x64.zip"
        );
        assert_eq!(
            pick("linux", "x86_64", &old).unwrap(),
            "llama-b6000-bin-ubuntu-x64.zip"
        );
        assert_eq!(
            pick("macos", "aarch64", &old).unwrap(),
            "llama-b6000-bin-macos-arm64.zip"
        );
    }

    #[test]
    fn unsupported_platform() {
        assert!(pick("freebsd", "x86_64", LIST).is_none());
        assert!(pick("linux", "riscv64", LIST).is_none());
        assert!(pick("macos", "x86_64", &["llama-b1-bin-macos-arm64.tar.gz"]).is_none());
    }
}

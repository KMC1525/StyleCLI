//! Pipeline: lê imagem por imagem → prepara → anota com o modelo → grava 1 linha JSONL.

use crate::annotate::{self, Annotation, Record, Sampling};
use crate::drop::parse_dropped;
use crate::engine::{self, Server, ServerCfg};
use crate::source::{ImageEntry, Source};
use crate::{http, imgprep, model};
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);
/// true enquanto estamos anotando (Ctrl+C então espera a imagem atual terminar).
static PROCESSING: AtomicBool = AtomicBool::new(false);
static CHILD: Mutex<Option<Arc<Mutex<Child>>>> = Mutex::new(None);

#[derive(Debug, Clone)]
pub struct Options {
    pub input: Vec<PathBuf>,
    pub output: Option<PathBuf>,
    pub overwrite: bool,
    pub limit: Option<usize>,
    pub quant: model::Quant,
    pub model: Option<PathBuf>,
    pub mmproj: Option<PathBuf>,
    pub llama_server: Option<PathBuf>,
    pub threads: Option<usize>,
    pub ctx: u32,
    pub gpu_layers: u32,
    pub image_size: u32,
    pub max_tokens: u32,
    pub timeout: u64,
    pub retries: u32,
}

/// Id estável da imagem: 16 primeiros hex do SHA-256 do conteúdo.
pub fn image_id(data: &[u8]) -> String {
    let d = Sha256::digest(data);
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Aceita o caminho como veio; se não existir, tenta interpretá-lo como texto "arrastado"
/// (aspas, `\ `, `file://`...), útil quando o terminal não tratou o caminho.
fn normalize_arg(p: &Path) -> PathBuf {
    if p.exists() {
        return p.to_path_buf();
    }
    match parse_dropped(&p.to_string_lossy()).as_slice() {
        [one] if one.exists() => one.clone(),
        _ => p.to_path_buf(),
    }
}

/// Pergunta as entradas. O usuário pode **arrastar e soltar** pastas/arquivos na janela do
/// terminal (isso cola o caminho) e apertar Enter. Repete até receber algo válido.
fn ask_inputs() -> Result<Vec<Source>> {
    if !std::io::stdin().is_terminal() {
        bail!("informe a pasta ou o arquivo .zip/.rar:  style <ENTRADA>   (veja: style --help)");
    }
    eprintln!("Arraste a PASTA de imagens ou o arquivo .zip/.rar para esta janela e aperte Enter");
    eprintln!("(ou digite o caminho; pode soltar vários de uma vez; Enter vazio sai).");
    loop {
        eprint!("> ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            bail!("nenhuma entrada informada");
        }
        let paths = parse_dropped(&line);
        if paths.is_empty() {
            bail!("nenhuma entrada informada");
        }
        match detect_all(&paths) {
            Ok(sources) => return Ok(sources),
            Err(e) => eprintln!("{e:#}\nTente de novo.\n"),
        }
    }
}

fn detect_all(paths: &[PathBuf]) -> Result<Vec<Source>> {
    paths.iter().map(|p| Source::detect(p)).collect()
}

/// Lê ids já gravados e descarta uma última linha incompleta (queda no meio da escrita).
pub fn load_done(path: &Path) -> Result<HashSet<String>> {
    let mut done = HashSet::new();
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(done);
    };
    let mut valid_len = 0usize;
    let mut pos = 0usize;
    while pos < bytes.len() {
        let end = match bytes[pos..].iter().position(|&b| b == b'\n') {
            Some(i) => pos + i,
            None => break, // sem \n final = linha incompleta
        };
        let line = &bytes[pos..end];
        if !line.iter().all(u8::is_ascii_whitespace) {
            match serde_json::from_slice::<Value>(line) {
                Ok(v) => {
                    if let Some(id) = v.get("id").and_then(Value::as_str) {
                        done.insert(id.to_string());
                    }
                }
                Err(_) => break, // linha corrompida: corta daqui
            }
        }
        pos = end + 1;
        valid_len = pos;
    }
    if valid_len < bytes.len() {
        OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(valid_len as u64)?;
    }
    Ok(done)
}

fn annotate_once(
    server: &Server,
    jpeg_b64: &str,
    s: &Sampling,
    timeout: Duration,
) -> Result<Annotation> {
    let body = annotate::build_request(jpeg_b64, s);
    let agent = http::local_agent(timeout);
    let url = format!("{}/v1/chat/completions", server.base);
    let mut resp = match agent.post(&url).send_json(&body) {
        Ok(r) => r,
        Err(e) => {
            if server.exited().is_some() {
                bail!(
                    "llama-server encerrou inesperadamente:\n{}",
                    server.log_tail(15)
                );
            }
            return Err(anyhow::anyhow!("falha ao falar com o servidor: {e}"));
        }
    };
    let status = resp.status().as_u16();
    let v: Value = resp
        .body_mut()
        .read_json()
        .context("resposta do servidor ilegível")?;
    if status != 200 {
        bail!("servidor respondeu {status}: {}", engine::error_message(&v));
    }
    annotate::parse_response(&v)
}

/// `pb.println` não imprime quando a saída não é um terminal; nesse caso usa stderr.
fn warn(pb: &ProgressBar, msg: String) {
    if pb.is_hidden() {
        eprintln!("{msg}");
    } else {
        pb.println(msg);
    }
}

fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Garante modelo + motor e sobe o llama-server (só baixa o que faltar).
fn start_server(o: &Options) -> Result<Server> {
    let files = model::ensure(o.quant, o.model.as_deref(), o.mmproj.as_deref())?;
    let exe = engine::locate(o.llama_server.as_deref())?;
    let threads = o.threads.unwrap_or_else(|| num_cpus::get_physical().max(1));
    Server::start(&ServerCfg {
        exe,
        model: files.model,
        mmproj: files.mmproj,
        ctx: o.ctx,
        threads,
        gpu_layers: o.gpu_layers,
        startup_timeout: Duration::from_secs(1200),
    })
}

pub fn execute(o: Options) -> Result<()> {
    let sources = if o.input.is_empty() {
        ask_inputs()?
    } else {
        let paths: Vec<PathBuf> = o.input.iter().map(|p| normalize_arg(p)).collect();
        detect_all(&paths)?
    };
    let mut counts = Vec::new();
    for src in &sources {
        let n = src.count()?;
        if n == 0 {
            bail!(
                "nenhuma imagem (jpg, png, webp, gif, bmp) encontrada em '{}'",
                src.path().display()
            );
        }
        counts.push(n);
    }
    let total: usize = counts.iter().sum();
    let out_path = o.output.clone().unwrap_or_else(|| {
        let stem = if sources.len() == 1 {
            sources[0].stem()
        } else {
            "dataset".to_string()
        };
        PathBuf::from(format!("{stem}.jsonl"))
    });
    if o.overwrite && out_path.exists() {
        File::create(&out_path)?;
    }
    let done = load_done(&out_path)?;

    for (src, n) in sources.iter().zip(&counts) {
        eprintln!(
            "Entrada : {} ({}, {} imagens)",
            src.path().display(),
            src.kind(),
            n
        );
    }
    eprintln!(
        "Saída   : {}{}",
        out_path.display(),
        if done.is_empty() {
            String::new()
        } else {
            format!("  (retomando: {} já prontas)", done.len())
        }
    );

    // Ctrl+C: durante a anotação, 1ª vez termina a imagem atual e salva; 2ª vez força.
    // Fora dela (baixando/carregando o modelo) sai na hora.
    ctrlc::set_handler(|| {
        if !PROCESSING.load(Ordering::SeqCst) || STOP.swap(true, Ordering::SeqCst) {
            if let Some(child) = CHILD.lock().ok().and_then(|g| g.clone())
                && let Ok(mut c) = child.lock()
            {
                let _ = c.kill();
            }
            std::process::exit(130);
        }
        eprintln!("\nParando após a imagem atual... (Ctrl+C de novo para forçar)");
    })
    .context("instalando handler de Ctrl+C")?;

    let mut out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
        .with_context(|| format!("abrindo {}", out_path.display()))?;

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template("{pos}/{len} [{bar:30.cyan/blue}] {msg}")?
            .progress_chars("=> "),
    );

    let timeout = Duration::from_secs(o.timeout);
    let (mut ok, mut skipped, mut failed) = (0usize, 0usize, 0usize);
    let mut spent = 0.0f64;
    let mut seen = 0usize;
    let started = Instant::now();
    let mut seen_ids: HashSet<String> = done;
    let mut server: Option<Server> = None;

    let halt = Cell::new(false);
    let mut handle = |entry: ImageEntry| -> Result<bool> {
        if STOP.load(Ordering::SeqCst) || o.limit.is_some_and(|l| ok >= l) {
            halt.set(true);
            return Ok(false);
        }
        seen += 1;
        let name = entry.name;
        let data = match entry.data {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                warn(&pb, format!("! {name}: {e:#}"));
                pb.inc(1);
                return Ok(true);
            }
        };
        let id = image_id(&data);
        if !seen_ids.insert(id.clone()) {
            skipped += 1; // já no dataset (retomada) ou imagem duplicada
            pb.inc(1);
            return Ok(true);
        }
        let t0 = Instant::now();
        pb.set_message(name.clone());

        let prepared = match imgprep::prepare(&data, o.image_size) {
            Ok(p) => p,
            Err(e) => {
                failed += 1;
                warn(&pb, format!("! {name}: {e:#}"));
                pb.inc(1);
                return Ok(true);
            }
        };
        drop(data);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&prepared.jpeg);

        // primeira imagem que realmente precisa do modelo: baixa o que falta e sobe o servidor
        if server.is_none() {
            let s = pb.suspend(|| start_server(&o))?;
            *CHILD.lock().unwrap() = Some(s.child_handle());
            PROCESSING.store(true, Ordering::SeqCst);
            server = Some(s);
        }
        let srv = server.as_ref().unwrap();

        let mut last_err = None;
        let mut result = None;
        for attempt in 0..=o.retries {
            let s = Sampling {
                max_tokens: o.max_tokens,
                temperature: (0.2 + 0.3 * attempt as f64).min(1.0),
                seed: 1000 + attempt,
            };
            match annotate_once(srv, &b64, &s, timeout) {
                Ok(a) => {
                    result = Some(a);
                    break;
                }
                Err(e) => {
                    if srv.exited().is_some() {
                        return Err(e); // servidor morreu: não adianta continuar
                    }
                    last_err = Some(e);
                }
            }
        }
        match result {
            Some(a) => {
                let rec = Record {
                    id,
                    image: name.clone(),
                    kind: a.kind,
                    tags: a.tags,
                    rating: a.rating,
                    chars: a.chars,
                    scenes: a.scenes,
                };
                let mut line = serde_json::to_string(&rec)?;
                line.push('\n');
                out.write_all(line.as_bytes())?;
                out.flush()?;
                ok += 1;
                spent += t0.elapsed().as_secs_f64();
            }
            None => {
                seen_ids.remove(&id);
                failed += 1;
                warn(
                    &pb,
                    format!(
                        "! {name}: falhou após {} tentativas: {:#}",
                        o.retries + 1,
                        last_err.unwrap()
                    ),
                );
            }
        }
        pb.inc(1);
        if ok > 0 {
            let left = total.saturating_sub(seen);
            pb.set_message(format!(
                "{:.1}s/img · faltam ~{} · {name}",
                spent / ok as f64,
                fmt_dur(spent / ok as f64 * left as f64)
            ));
        }
        Ok(true)
    };
    for src in &sources {
        if halt.get() {
            break;
        }
        src.for_each(&mut handle)?;
    }
    pb.finish_and_clear();

    eprintln!(
        "\n{} anotadas, {} puladas (já existentes/duplicadas), {} com erro — {} — {}",
        ok,
        skipped,
        failed,
        fmt_dur(started.elapsed().as_secs_f64()),
        out_path.display()
    );
    if STOP.load(Ordering::SeqCst) {
        eprintln!("Interrompido. Rode o mesmo comando para continuar de onde parou.");
    }
    if ok == 0 && skipped == 0 && failed > 0 {
        bail!("nenhuma imagem foi anotada");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_hex16() {
        let a = image_id(b"abc");
        assert_eq!(a.len(), 16);
        assert_eq!(a, "ba7816bf8f01cfea");
        assert_ne!(a, image_id(b"abd"));
    }

    #[test]
    fn resume_trims_partial_line() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("o.jsonl");
        std::fs::write(&p, "{\"id\":\"aa\"}\n{\"id\":\"bb\"}\n{\"id\":\"c").unwrap();
        let done = load_done(&p).unwrap();
        assert_eq!(done.len(), 2);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "{\"id\":\"aa\"}\n{\"id\":\"bb\"}\n"
        );
        assert!(load_done(&d.path().join("nao-existe")).unwrap().is_empty());
    }
}

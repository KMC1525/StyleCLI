//! Entrada: pasta solta, `.zip` ou `.rar`. As imagens são entregues uma a uma
//! (nunca o conjunto inteiro na memória).

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];
/// Limite de segurança por imagem (evita zip bomb / arquivos absurdos).
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

pub struct ImageEntry {
    /// Nome do arquivo (sem pastas).
    pub name: String,
    /// Bytes da imagem, ou o erro de leitura/extração.
    pub data: Result<Vec<u8>>,
}

#[derive(Debug)]
pub enum Source {
    Dir(PathBuf),
    Zip(PathBuf),
    Rar(PathBuf),
}

pub fn is_image_name(name: &str) -> bool {
    let norm = name.replace('\\', "/");
    let file = norm.rsplit('/').next().unwrap_or("");
    // ignora lixo de macOS (`__MACOSX/`, `._foto.jpg`) e arquivos ocultos
    if file.starts_with('.') || norm.split('/').any(|c| c == "__MACOSX") {
        return false;
    }
    match file.rsplit_once('.') {
        Some((_, ext)) => IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

fn file_name(path: &str) -> String {
    path.replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Ordenação "natural": `2.png` antes de `10.png`.
fn natural_key(s: &str) -> Vec<(u8, String, u64)> {
    let mut out = Vec::new();
    let lower = s.to_lowercase();
    let mut chars = lower.chars().peekable();
    while let Some(&c) = chars.peek() {
        let mut tok = String::new();
        if c.is_ascii_digit() {
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    tok.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push((0, String::new(), tok.parse().unwrap_or(u64::MAX)));
        } else {
            while let Some(&d) = chars.peek() {
                if !d.is_ascii_digit() {
                    tok.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push((1, tok, 0));
        }
    }
    out
}

impl Source {
    pub fn detect(path: &Path) -> Result<Source> {
        let meta = std::fs::metadata(path)
            .with_context(|| format!("não encontrei '{}'", path.display()))?;
        if meta.is_dir() {
            return Ok(Source::Dir(path.to_path_buf()));
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        // confere pela assinatura, não só pela extensão
        let mut magic = [0u8; 8];
        let n = File::open(path)?.read(&mut magic)?;
        let magic = &magic[..n];
        if magic.starts_with(b"PK") {
            Ok(Source::Zip(path.to_path_buf()))
        } else if magic.starts_with(b"Rar!") {
            Ok(Source::Rar(path.to_path_buf()))
        } else {
            bail!(
                "'{}' não é uma pasta nem um arquivo .zip/.rar válido (extensão '{}')",
                path.display(),
                ext
            )
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Source::Dir(_) => "pasta",
            Source::Zip(_) => "zip",
            Source::Rar(_) => "rar",
        }
    }

    /// Nome base usado para sugerir o arquivo de saída.
    pub fn stem(&self) -> String {
        let p = match self {
            Source::Dir(p) | Source::Zip(p) | Source::Rar(p) => p,
        };
        let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        let s = match self {
            Source::Dir(_) => abs.file_name(),
            _ => abs.file_stem(),
        };
        s.map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "dataset".into())
    }

    fn dir_files(root: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| is_image_name(&p.to_string_lossy()))
            .collect();
        files.sort_by_key(|p| natural_key(&p.to_string_lossy()));
        files
    }

    /// Quantidade de imagens (só lê cabeçalhos).
    pub fn count(&self) -> Result<usize> {
        match self {
            Source::Dir(root) => Ok(Self::dir_files(root).len()),
            Source::Zip(p) => {
                let zip = zip::ZipArchive::new(File::open(p)?).context("abrindo zip")?;
                Ok(zip
                    .file_names()
                    .filter_map(|n| n.ok())
                    .filter(|n| !n.ends_with('/') && is_image_name(n))
                    .count())
            }
            Source::Rar(p) => {
                let mut n = 0;
                for e in unrar::Archive::new(p).open_for_listing().map_err(rar_err)? {
                    let e = e.map_err(rar_err)?;
                    if e.is_file() && is_image_name(&e.filename.to_string_lossy()) {
                        n += 1;
                    }
                }
                Ok(n)
            }
        }
    }

    /// Entrega as imagens uma por vez. `f` devolve `Ok(false)` para parar.
    pub fn for_each(&self, mut f: impl FnMut(ImageEntry) -> Result<bool>) -> Result<()> {
        match self {
            Source::Dir(root) => {
                for path in Self::dir_files(root) {
                    let name = file_name(&path.to_string_lossy());
                    let data = std::fs::metadata(&path)
                        .map_err(anyhow::Error::from)
                        .and_then(|m| {
                            if m.len() > MAX_IMAGE_BYTES {
                                bail!("arquivo grande demais ({} bytes)", m.len())
                            }
                            std::fs::read(&path).map_err(Into::into)
                        });
                    if !f(ImageEntry { name, data })? {
                        break;
                    }
                }
                Ok(())
            }
            Source::Zip(p) => {
                let mut zip = zip::ZipArchive::new(File::open(p)?).context("abrindo zip")?;
                let mut idx: Vec<(usize, String)> = (0..zip.len())
                    .filter_map(|i| {
                        let e = zip.by_index_raw(i).ok()?;
                        let n = e.name().ok()?.into_owned();
                        (e.is_file() && is_image_name(&n)).then_some((i, n))
                    })
                    .collect();
                idx.sort_by_key(|(_, n)| natural_key(n));
                for (i, n) in idx {
                    let data = (|| -> Result<Vec<u8>> {
                        let mut e = zip.by_index(i)?; // erra se criptografado
                        if e.size() > MAX_IMAGE_BYTES {
                            bail!("arquivo grande demais ({} bytes)", e.size());
                        }
                        let mut buf = Vec::with_capacity(e.size() as usize);
                        e.by_ref().take(MAX_IMAGE_BYTES).read_to_end(&mut buf)?;
                        Ok(buf)
                    })();
                    if !f(ImageEntry {
                        name: file_name(&n),
                        data,
                    })? {
                        break;
                    }
                }
                Ok(())
            }
            Source::Rar(p) => {
                let mut cur = Some(
                    unrar::Archive::new(p)
                        .open_for_processing()
                        .map_err(rar_err)?,
                );
                while let Some(arch) = cur.take() {
                    let Some(before_file) = arch.read_header().map_err(rar_err)? else {
                        break;
                    };
                    let entry = before_file.entry();
                    let name = entry.filename.to_string_lossy().into_owned();
                    if entry.is_file() && is_image_name(&name) {
                        let too_big = entry.unpacked_size > MAX_IMAGE_BYTES;
                        let (data, next) = if too_big || entry.is_encrypted() {
                            let why = if too_big {
                                "arquivo grande demais"
                            } else {
                                "arquivo criptografado"
                            };
                            (
                                Err(anyhow::anyhow!(why)),
                                before_file.skip().map_err(rar_err)?,
                            )
                        } else {
                            match before_file.read() {
                                Ok((bytes, next)) => (Ok(bytes), next),
                                // erro no meio do fluxo: não dá para continuar com segurança
                                Err(e) => return Err(rar_err(e)),
                            }
                        };
                        let keep = f(ImageEntry {
                            name: file_name(&name),
                            data,
                        })?;
                        if !keep {
                            break;
                        }
                        cur = Some(next);
                    } else {
                        cur = Some(before_file.skip().map_err(rar_err)?);
                    }
                }
                Ok(())
            }
        }
    }
}

fn rar_err(e: unrar::error::UnrarError) -> anyhow::Error {
    anyhow::anyhow!("erro ao ler o .rar: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_names() {
        assert!(is_image_name("a/b/foto.JPG"));
        assert!(is_image_name("x\\y\\z.webp"));
        assert!(!is_image_name("__MACOSX/a.jpg"));
        assert!(!is_image_name("pasta/._a.jpg"));
        assert!(!is_image_name("leiame.txt"));
        assert!(!is_image_name("semext"));
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["p10.png", "p2.png", "p1.png"];
        v.sort_by_key(|s| natural_key(s));
        assert_eq!(v, ["p1.png", "p2.png", "p10.png"]);
    }

    #[test]
    fn zip_roundtrip() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let zp = dir.path().join("t.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            for n in ["b/2.png", "b/10.png", "a.jpg", "nota.txt", "__MACOSX/x.png"] {
                w.start_file(n, o).unwrap();
                w.write_all(n.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        let src = Source::detect(&zp).unwrap();
        assert!(matches!(src, Source::Zip(_)));
        assert_eq!(src.count().unwrap(), 3);
        let mut got = vec![];
        src.for_each(|e| {
            got.push((e.name, String::from_utf8(e.data.unwrap()).unwrap()));
            Ok(true)
        })
        .unwrap();
        let expect = [
            ("a.jpg", "a.jpg"),
            ("2.png", "b/2.png"),
            ("10.png", "b/10.png"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()));
        assert_eq!(got, expect);
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// Monta um .rar (RAR4, método "store") sem depender do programa `rar`.
    fn rar4(files: &[(&str, &[u8])]) -> Vec<u8> {
        fn block(typ: u8, flags: u16, body: &[u8]) -> Vec<u8> {
            let mut raw = vec![typ];
            raw.extend(flags.to_le_bytes());
            raw.extend(((7 + body.len()) as u16).to_le_bytes());
            raw.extend(body);
            let mut out = ((crc32(&raw) & 0xFFFF) as u16).to_le_bytes().to_vec();
            out.extend(raw);
            out
        }
        let mut out = b"Rar!\x1a\x07\x00".to_vec();
        out.extend(block(0x73, 0, &[0; 6]));
        for (name, data) in files {
            let mut b = Vec::new();
            b.extend((data.len() as u32).to_le_bytes()); // pack size
            b.extend((data.len() as u32).to_le_bytes()); // unpack size
            b.push(2); // host os
            b.extend(crc32(data).to_le_bytes());
            b.extend(0x5A21_0000u32.to_le_bytes()); // ftime
            b.extend([20, 0x30]); // unpack ver, método store
            b.extend((name.len() as u16).to_le_bytes());
            b.extend(0x20u32.to_le_bytes()); // attr
            b.extend(name.as_bytes());
            out.extend(block(0x74, 0x8000, &b));
            out.extend(*data);
        }
        out.extend([0xC4, 0x3D, 0x7B, 0x00, 0x40, 0x07, 0x00]);
        out
    }

    #[test]
    fn rar_streams_only_images() {
        let dir = tempfile::tempdir().unwrap();
        let rp = dir.path().join("t.rar");
        std::fs::write(
            &rp,
            rar4(&[("p/1.png", b"um"), ("leia.txt", b"x"), ("p/2.jpg", b"dois")]),
        )
        .unwrap();
        let src = Source::detect(&rp).unwrap();
        assert!(matches!(src, Source::Rar(_)));
        assert_eq!(src.count().unwrap(), 2);
        let mut got = vec![];
        src.for_each(|e| {
            got.push((e.name, e.data.unwrap()));
            Ok(true)
        })
        .unwrap();
        assert_eq!(
            got,
            [
                ("1.png".to_string(), b"um".to_vec()),
                ("2.jpg".to_string(), b"dois".to_vec())
            ]
        );
        // parar no meio funciona
        let mut n = 0;
        src.for_each(|_| {
            n += 1;
            Ok(false)
        })
        .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn folder_order_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["p10.png", "p2.png", "p1.png", "x.txt"] {
            std::fs::write(dir.path().join(n), n).unwrap();
        }
        let src = Source::detect(dir.path()).unwrap();
        assert_eq!(src.count().unwrap(), 3);
        let mut names = vec![];
        src.for_each(|e| {
            names.push(e.name);
            Ok(true)
        })
        .unwrap();
        assert_eq!(names, ["p1.png", "p2.png", "p10.png"]);
    }
}

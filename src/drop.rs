//! Arrastar e soltar: ao soltar uma pasta/arquivo na janela do terminal, o terminal apenas
//! "cola" o caminho como texto, e cada um formata de um jeito:
//!
//! | terminal                    | texto colado                          |
//! |-----------------------------|---------------------------------------|
//! | GNOME, Konsole, Windows Term| `'/home/u/Minhas Imagens'` ou `"C:\x y"`|
//! | macOS Terminal / iTerm      | `/Users/u/Minhas\ Imagens`            |
//! | PowerShell                  | `& 'C:\Users\u\Minhas Imagens'`       |
//! | alguns gerenciadores        | `file:///home/u/Minhas%20Imagens`     |
//!
//! Vários itens soltos de uma vez vêm separados por espaço.

use std::path::{Path, PathBuf};

/// Interpreta o texto colado e devolve os caminhos (existam ou não; quem chama valida).
pub fn parse_dropped(raw: &str) -> Vec<PathBuf> {
    parse(raw, &|p| p.exists(), cfg!(windows))
}

fn parse(raw: &str, exists: &dyn Fn(&Path) -> bool, windows: bool) -> Vec<PathBuf> {
    let clean = sanitize(raw);
    if clean.is_empty() {
        return vec![];
    }
    // Caminho único sem aspas nem escapes (ex.: espaços sem tratamento): usa a linha inteira.
    let whole = convert(strip_quotes(&clean), windows);
    if exists(&whole) {
        return vec![whole];
    }
    tokenize(&clean, !windows)
        .into_iter()
        .map(|t| convert(&t, windows))
        .collect()
}

/// Remove caracteres de controle (inclui marcas de "bracketed paste"), BOM e o `& ` do PowerShell.
fn sanitize(raw: &str) -> String {
    let s = raw
        .replace("\u{1b}[200~", "")
        .replace("\u{1b}[201~", "")
        .replace('\u{feff}', "");
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    let s = s.trim();
    s.strip_prefix("& ").unwrap_or(s).trim().to_string()
}

fn strip_quotes(s: &str) -> &str {
    for q in ['\'', '"'] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Separa em itens respeitando aspas; `\` escapa o próximo caractere só quando
/// `escapes` (Unix) — no Windows a barra invertida faz parte do caminho.
fn tokenize(s: &str, escapes: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false; // distingue item vazio entre aspas de "sem item"
    let mut quote: Option<char> = None;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if escapes && c == '\\' && matches!(it.peek(), Some('"') | Some('\\')) => {
                cur.push(it.next().unwrap());
            }
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has = true;
                }
                '\\' if escapes => {
                    if let Some(n) = it.next() {
                        cur.push(n);
                        has = true;
                    }
                }
                c if c.is_whitespace() => {
                    if has || !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                }
                c => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|t| !t.is_empty());
    out
}

/// `file://` → caminho, `~` → pasta pessoal.
fn convert(token: &str, windows: bool) -> PathBuf {
    if let Some(rest) = token
        .strip_prefix("file://")
        .or_else(|| token.strip_prefix("FILE://"))
    {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let mut p = percent_decode(rest);
        // `/C:/pasta` → `C:/pasta`
        if windows && p.len() > 2 && p.starts_with('/') && p.as_bytes()[2] == b':' {
            p.remove(0);
        }
        return PathBuf::from(p);
    }
    if (token == "~" || token.starts_with("~/"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(token.trim_start_matches('~').trim_start_matches('/'));
    }
    PathBuf::from(token)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unix(s: &str) -> Vec<String> {
        parse(s, &|_| false, false)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }
    fn win(s: &str) -> Vec<String> {
        parse(s, &|_| false, true)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn quoted_paths() {
        assert_eq!(unix("'/home/u/Minhas Imagens'"), ["/home/u/Minhas Imagens"]);
        assert_eq!(unix("\"/home/u/a b/x.zip\"  \n"), ["/home/u/a b/x.zip"]);
    }

    #[test]
    fn backslash_escaped_spaces_macos_style() {
        assert_eq!(
            unix(r"/Users/u/Minhas\ Imagens/pack.rar "),
            ["/Users/u/Minhas Imagens/pack.rar"]
        );
        assert_eq!(unix(r"/a/b\ \(1\)"), ["/a/b (1)"]);
    }

    #[test]
    fn windows_paths_keep_backslashes() {
        assert_eq!(
            win(r#""C:\Users\u\Minhas Imagens""#),
            [r"C:\Users\u\Minhas Imagens"]
        );
        assert_eq!(
            win(r"& 'C:\Users\u\Minhas Imagens'"),
            [r"C:\Users\u\Minhas Imagens"]
        );
        assert_eq!(win(r"C:\pics\a.zip"), [r"C:\pics\a.zip"]);
    }

    #[test]
    fn several_items() {
        assert_eq!(
            unix(r#"'/a b' /c\ d "/e f" /g"#),
            ["/a b", "/c d", "/e f", "/g"]
        );
        assert_eq!(win(r#""C:\a b" "D:\c""#), [r"C:\a b", r"D:\c"]);
    }

    #[test]
    fn file_uris() {
        assert_eq!(
            unix("file:///home/u/Minhas%20Imagens"),
            ["/home/u/Minhas Imagens"]
        );
        assert_eq!(unix("file://localhost/tmp/a%C3%A7.zip"), ["/tmp/aç.zip"]);
        assert_eq!(win("file:///C:/Users/u/a%20b"), ["C:/Users/u/a b"]);
    }

    #[test]
    fn unquoted_path_with_spaces_uses_whole_line_if_it_exists() {
        let exists = |p: &Path| p == Path::new("/tmp/minha pasta/fotos");
        let got = parse("/tmp/minha pasta/fotos\n", &exists, false);
        assert_eq!(got, [PathBuf::from("/tmp/minha pasta/fotos")]);
    }

    #[test]
    fn control_chars_and_paste_markers() {
        assert_eq!(unix("\u{1b}[200~'/a b'\u{1b}[201~\r\n"), ["/a b"]);
        assert!(unix("   \r\n").is_empty());
        assert!(unix("").is_empty());
    }

    #[test]
    fn tilde_is_expanded() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(
                parse("~/Imagens", &|_| false, false),
                [home.join("Imagens")]
            );
        }
    }
}

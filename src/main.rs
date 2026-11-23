//! StyleCLI — gera datasets JSONL de estilo (tags, personagens, cenários, sfw/nsfw)
//! a partir de imagens, artes e quadrinhos, rodando um modelo de visão local (GGUF).

mod annotate;
mod drop;
mod engine;
mod http;
mod imgprep;
mod model;
mod paths;
mod run;
mod source;
mod update;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "style",
    version,
    about = "Gera datasets JSONL de estilo a partir de imagens, artes e quadrinhos (SFW/NSFW), \
             analisando uma imagem por vez com um modelo local leve.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// Pastas com imagens e/ou arquivos .zip / .rar. Sem isso o programa pergunta:
    /// aí é só arrastar e soltar a pasta ou o arquivo na janela do terminal
    #[arg(num_args = 0..)]
    input: Vec<PathBuf>,

    /// Arquivo de saída .jsonl (padrão: <nome-da-entrada>.jsonl). Se já existir, continua de onde parou
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Recomeça do zero, apagando o conteúdo atual do arquivo de saída
    #[arg(long)]
    overwrite: bool,

    /// Anota no máximo N imagens novas (útil para testar)
    #[arg(long, value_name = "N")]
    limit: Option<usize>,

    /// Quantização do modelo base
    #[arg(long, value_enum, default_value = "q4_k_m")]
    quant: model::Quant,

    /// Usa este arquivo GGUF em vez de baixar o modelo
    #[arg(long, value_name = "ARQUIVO")]
    model: Option<PathBuf>,

    /// Usa este projetor de visão (mmproj) em vez de baixar
    #[arg(long, value_name = "ARQUIVO")]
    mmproj: Option<PathBuf>,

    /// Caminho do llama-server (padrão: baixa o llama.cpp automaticamente)
    #[arg(long, value_name = "ARQUIVO", env = "STYLE_LLAMA_SERVER")]
    llama_server: Option<PathBuf>,

    /// Threads de CPU (padrão: núcleos físicos). Reduza para o PC não travar
    #[arg(short = 't', long)]
    threads: Option<usize>,

    /// Tamanho do contexto do modelo
    #[arg(long, default_value_t = 4096)]
    ctx: u32,

    /// Camadas na GPU (0 = tudo na CPU; só funciona com um llama-server com GPU)
    #[arg(long, default_value_t = 0)]
    gpu_layers: u32,

    /// Lado da imagem enviada ao modelo: a área é limitada a LADO×LADO pixels.
    /// Menor = mais rápido e menos RAM, com menos detalhe
    #[arg(long, value_name = "PX", default_value_t = 640)]
    image_size: u32,

    /// Máximo de tokens por resposta
    #[arg(long, default_value_t = 900)]
    max_tokens: u32,

    /// Tempo máximo por imagem, em segundos
    #[arg(long, value_name = "SEG", default_value_t = 900)]
    timeout: u64,

    /// Novas tentativas quando o modelo devolve algo inválido
    #[arg(long, default_value_t = 2)]
    retries: u32,
}

#[derive(Subcommand)]
enum Cmd {
    /// Atualiza o style para a última versão publicada
    Update {
        /// Reinstala mesmo se já estiver na última versão
        #[arg(long)]
        force: bool,
    },
}

/// No Windows, se o `style` abriu a própria janela de console (duplo clique ou item solto
/// sobre o .exe), ela fecharia ao terminar e o usuário não leria o resultado.
#[cfg(windows)]
fn console_is_ours() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleProcessList;
    let mut ids = [0u32; 4];
    // número de processos ligados ao console; 1 = só nós
    let n = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
    n == 1
}

#[cfg(not(windows))]
fn console_is_ours() -> bool {
    false
}

fn pause_if_needed() {
    use std::io::IsTerminal;
    if console_is_ours() && std::io::stdin().is_terminal() {
        eprintln!("\nPressione Enter para fechar...");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Cmd::Update { force }) => update::run(force),
        None => run::execute(run::Options {
            input: cli.input,
            output: cli.output,
            overwrite: cli.overwrite,
            limit: cli.limit,
            quant: cli.quant,
            model: cli.model,
            mmproj: cli.mmproj,
            llama_server: cli.llama_server,
            threads: cli.threads,
            ctx: cli.ctx,
            gpu_layers: cli.gpu_layers,
            image_size: cli.image_size.clamp(64, 2048),
            max_tokens: cli.max_tokens,
            timeout: cli.timeout,
            retries: cli.retries,
        }),
    };
    if let Err(e) = result {
        eprintln!("erro: {e:#}");
        pause_if_needed();
        std::process::exit(1);
    }
    pause_if_needed();
}

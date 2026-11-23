# StyleCLI

CLI em Rust que gera **datasets de estilo em JSONL** a partir de imagens, artes e quadrinhos
(SFW e NSFW), feita para **computadores fracos**: roda um modelo de visão pequeno
([Qwen3.5‑4B‑Heretic](https://huggingface.co/darrellbest/Qwen3.5-4B-Heretic-GGUF), quantizado em
`Q4_K_M`, ~2,8 GB) 100% local, na CPU, e analisa **uma imagem por vez** — nada de carregar tudo
na memória.

```text
style                    # executa (pergunta a pasta/arquivo)
style <pasta|zip|rar>    # executa direto
style update             # atualiza o próprio style
```

## Instalação

Baixe o binário da sua plataforma na [última release](https://github.com/KMC1525/StyleCLI/releases/latest),
extraia e coloque o `style` no seu `PATH`. Depois, mantenha atualizado com `style update`.

| Plataforma | Arquivo |
|---|---|
| Linux x86_64 / ARM64 | `style-x86_64-unknown-linux-gnu.tar.gz` / `style-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Intel / Apple Silicon | `style-x86_64-apple-darwin.tar.gz` / `style-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `style-x86_64-pc-windows-msvc.zip` |

Ou compile: `cargo install --git https://github.com/KMC1525/StyleCLI` (precisa de Rust 1.88+ e de um compilador C++, usado pela leitura de `.rar`).

## Uso

```bash
style ./minhas-imagens                 # pasta (inclui subpastas) → minhas-imagens.jsonl
style colecao.zip -o dataset.jsonl     # arquivo compactado
style quadrinhos.rar --limit 5         # testa com 5 imagens
style ./fotos -t 2 --image-size 512    # PC bem fraco: 2 threads, imagens menores
```

Na **primeira execução** o `style` baixa sozinho (com retomada se a conexão cair):

1. o modelo `Qwen3.5-4B-Heretic-Q4_K_M.gguf` (2,8 GB) e o projetor de visão (0,7 GB), do Hugging Face;
2. o `llama-server` (build **CPU** do [llama.cpp](https://github.com/ggml-org/llama.cpp)), que executa o modelo.

Os arquivos ficam em `~/.local/share/stylecli` (Linux), `~/Library/Application Support/stylecli`
(macOS) ou `%LOCALAPPDATA%\stylecli` (Windows); mude com a variável `STYLE_HOME`.

### Entradas aceitas

Pasta solta (recursiva), `.zip` ou `.rar`; imagens `jpg`, `png`, `webp`, `gif`, `bmp`.
Arquivos compactados são lidos em *streaming*: cada imagem é extraída, anotada e descartada.

### Retomar e interromper

O resultado é gravado linha a linha. **Ctrl+C** termina a imagem atual e salva; rodar o mesmo comando
de novo **continua de onde parou** (imagens já presentes no `.jsonl`, ou duplicadas, são puladas).
Use `--overwrite` para recomeçar do zero.

### Opções

| Opção | Padrão | Descrição |
|---|---|---|
| `-o, --output` | `<entrada>.jsonl` | arquivo de saída |
| `--overwrite` | — | apaga a saída existente |
| `--limit N` | — | anota no máximo N imagens novas |
| `-t, --threads` | núcleos físicos | menos threads = PC mais responsivo |
| `--image-size PX` | `640` | área máx. enviada ao modelo (PX×PX); menor = mais rápido e leve |
| `--ctx` | `4096` | contexto do modelo |
| `--quant` | `q4_k_m` | `q8_0` (4,6 GB) é mais fiel e exige mais RAM |
| `--gpu-layers` | `0` | camadas na GPU (precisa de um `llama-server` com GPU) |
| `--llama-server` | auto | usa seu próprio `llama-server` (ou `STYLE_LLAMA_SERVER`) |
| `--model`, `--mmproj` | auto | usa GGUFs locais em vez de baixar |
| `--retries` | `2` | novas tentativas se o modelo responder algo inválido |
| `--timeout`, `--max-tokens` | `900`, `900` | limites por imagem |

## Formato do dataset

Um objeto JSON por linha:

```json
{"id":"5b8d06a88ded781a","image":"pagina-03.png","type":"comic","tags":["manga","black and white","ink lineart","screentone","dynamic composition","action","high contrast","urban","dramatic lighting","shonen"],"rating":"sfw","chars":["short spiky black hair, sharp eyes, school uniform, determined expression"],"scenes":["city street at night, neon signs, wet asphalt"]}
```

| Campo | Descrição |
|---|---|
| `id` | id estável da imagem (16 hex do SHA‑256 do conteúdo; a mesma imagem tem sempre o mesmo id) |
| `image` | nome do arquivo da imagem |
| `type` | `art` (arte/ilustração) ou `comic` (quadrinho/mangá/webtoon) |
| `tags` | de 8 a 10 tags de estilo, minúsculas e sem repetição |
| `rating` | `sfw` ou `nsfw` |
| `chars` | uma string por personagem com suas características visuais (cabelo, olhos, roupa, expressão...) |
| `scenes` | características visuais do cenário (local, hora, iluminação, objetos...) |

O texto é gerado em inglês (o modelo é mais preciso assim e é o idioma usual de datasets de estilo).
A saída do modelo é **forçada por gramática JSON** (schema) e ainda validada: respostas inválidas
são refeitas (`--retries`) e imagens que falham não entram no arquivo.

## Dicas para PCs fracos

- Feche outros programas: o modelo + projetor de visão ocupam cerca de 4 GB de RAM.
- Reduza `--image-size` (ex.: `448`) e `-t` se o PC ficar lento. O tempo por imagem
  depende da CPU; a barra de progresso mostra o tempo médio e a estimativa restante.
- Deixe rodando em lotes: pode interromper com Ctrl+C e retomar quando quiser.

## Desenvolvimento

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

Releases: o workflow `Release` compila para Linux, macOS e Windows e publica os binários quando uma tag `v*` é enviada
(ou manualmente em *Actions → Release*).

> O modelo base tem salvaguardas reduzidas por design (versão *abliterated*). Você é responsável pelo conteúdo
> que processa e pelo uso do dataset gerado.

## Licença

MIT — veja [LICENSE](LICENSE).

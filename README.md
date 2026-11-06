# FilmCraft + AutoCut

Fork pessoal do [FilmCraft](https://github.com/storytold/filmcraft), um editor de vídeo não linear de código
aberto escrito em Rust, com o **AutoCut** acoplado: corte automático de silêncios, highlights de gameplay,
zoom automático e legendas com Whisper local, direto numa sequência do editor.

Feito por [usagi](https://github.com/usaginoyami) (UsagiNoYami) para editar os vídeos do canal mais rápido.
Baseado no FilmCraft, da equipe ArtCraft. Projeto independente, sem relação oficial com ela.

## O que este fork adiciona

| | |
|---|---|
| **Cortar silêncios** | Remove pausas com limiar automático (mede o volume do vídeo) ou manual |
| **Highlights de gameplay** | Pega os momentos mais altos do áudio, com tempo antes e depois configurável |
| **Todas as faixas de áudio** | Mantém mistura, microfone e jogo, cada uma na sua trilha |
| **Zoom automático** | Alterna a Escala entre 100% e X% (padrão 112%) a cada corte |
| **Legendas** | Whisper local ([faster-whisper](https://github.com/SYSTRAN/faster-whisper)), 100% offline, importadas como trilha de legendas |

O vídeo original nunca é alterado: o AutoCut cria uma sequência nova (`<video>_autocut`) no projeto, e cada
etapa é desfazível com **Edit > Undo**.

O motor é o [AutoCut Free](https://github.com/usaginoyami/autocut-free) (MIT). O código do FilmCraft não foi
modificado: tudo fica em [`integrations/autocut/`](integrations/autocut/).

## Como funciona

O FilmCraft não tem sistema de plugins, mas todo comando do app (menu, botão, atalho) também é acessível
pelo **canal de controle** ([`docs/control-protocol.md`](docs/control-protocol.md)). A ponte usa esse canal:

```
vídeo ──► autocut_free.py ──► XML FCP7 (+ .srt)
                                  │
          filmcraft_autocut.py ───┴─► FilmCraft (aberto com --control)
                                       1. file.import        importa o XML
                                       2. sequence.open      abre a sequência
                                       3. effects.setParam   zoom (motion / scale)
                                       4. captions.import    legendas
```

## Começando

**Requisitos:** [Rust](https://rustup.rs) (para compilar), Python 3.9+, [FFmpeg](https://ffmpeg.org) no PATH e,
para legendas, `pip install faster-whisper`. Para o instalador de uma versão pronta do FilmCraft, veja o
[repositório original](https://github.com/storytold/filmcraft/releases).

```sh
git clone https://github.com/usaginoyami/filmcraft_Autocut
cd filmcraft_Autocut

# 1) abre o FilmCraft com o canal de controle e um token novo (64 hex)
export FILMCRAFT_CONTROL_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
cargo run --release -p filmcraft -- --control 9876

# 2) em outro terminal, com o mesmo token, rode o AutoCut num vídeo
python3 integrations/autocut/filmcraft_autocut.py video.mp4 --mode silence --zoom --captions
python3 integrations/autocut/filmcraft_autocut.py gameplay.mp4 --mode highlights --top 12 --before 6 --after 4
```

Detalhes, opções, exemplo para Windows (PowerShell) e limitações: [`integrations/autocut/README.md`](integrations/autocut/README.md).

## Testes

```sh
python3 -m unittest discover -s integrations/autocut/tests -v
```

Os testes da ponte rodam contra um servidor de controle simulado e gravam um vídeo de teste com o FFmpeg.
O restante do FilmCraft segue as regras de qualidade do projeto original ([`CONTRIBUTING.md`](CONTRIBUTING.md),
`cargo xtask ci`).

## Acompanhando o FilmCraft original

```sh
git remote add upstream https://github.com/storytold/filmcraft   # uma vez
git fetch upstream && git merge upstream/main
```

## Licença e créditos

- **FilmCraft:** MIT OU Apache-2.0, à sua escolha ([`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE),
  [`NOTICE`](NOTICE)). Direitos autorais da equipe ArtCraft e dos colaboradores do FilmCraft.
- **AutoCut:** MIT, © 2026 usaginoyami ([`integrations/autocut/LICENSE-AUTOCUT`](integrations/autocut/LICENSE-AUTOCUT)).
- Fontes, ícones e imagens mantêm as licenças próprias listadas em [`ATTRIBUTION.md`](ATTRIBUTION.md).

> **Aviso sobre marca:** o nome e as logos da ArtCraft são marcas da equipe ArtCraft e não são de código aberto
> ([`docs/brand/LICENSE-brand.txt`](docs/brand/LICENSE-brand.txt)). Elas ainda constam no código e nos arquivos
> deste fork e precisam ser removidas ou substituídas antes de distribuir builds ou publicar o app.

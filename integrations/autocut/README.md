# AutoCut para o FilmCraft

Corta silêncios, acha highlights de gameplay, aplica zoom automático e importa legendas (Whisper local)
direto numa sequência do FilmCraft. É o motor do [AutoCut Free](https://github.com/usaginoyami/autocut-free)
(MIT) ligado ao FilmCraft pelo canal de controle.

O FilmCraft não tem sistema de plugins, então a ponte usa o mesmo canal que a UI, a CLI e o servidor MCP
usam ([`docs/control-protocol.md`](../../docs/control-protocol.md)). Nada do código Rust foi alterado.

## Como funciona

1. `autocut_free.py` analisa o áudio com FFmpeg e gera um XML FCP7 (`<video>_autocut.xml`) e, com `--captions`, um `.srt`.
2. `filmcraft_autocut.py` conecta no FilmCraft e executa, como comandos desfazíveis:
   - `file.import` do XML (o FilmCraft detecta FCP7 XML sozinho);
   - `sequence.open` na sequência `<video>_autocut`;
   - `effects.setParam` (`motion` / `scale`) alternando 100% e 112% a cada corte, com `--zoom`;
   - `captions.import` do `.srt`.

O vídeo original não é alterado.

## Requisitos

- FilmCraft (desktop) rodando com o canal de controle ligado
- Python 3.9+ e [FFmpeg](https://ffmpeg.org) no PATH
- Legendas: `pip install faster-whisper`

## Uso

```sh
# 1) abra o FilmCraft com um token novo (64 hex), e use o mesmo token no passo 2
export FILMCRAFT_CONTROL_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
filmcraft --control 9876 &

# 2) rode a ponte
python3 integrations/autocut/filmcraft_autocut.py video.mp4 --mode silence --zoom --captions
python3 integrations/autocut/filmcraft_autocut.py gameplay.mp4 --mode highlights --top 12 --before 6 --after 4
```

No Windows (PowerShell):

```powershell
$env:FILMCRAFT_CONTROL_TOKEN = -join ((1..32) | ForEach-Object { '{0:x2}' -f (Get-Random -Maximum 256) })
Start-Process filmcraft -ArgumentList '--control','9876'
python integrations\autocut\filmcraft_autocut.py video.mp4 --mode silence --zoom --captions
```

Opções da ponte: `--port` (padrão 9876 ou `FILMCRAFT_CONTROL_PORT`), `--host`, `--zoom`, `--zoom-scale 112`,
`--captions`, `--lang pt`. Todas as outras opções vão direto para o motor: `--mode silence|highlights`,
`--track N`, `--noise auto|-35`, `--min-silence 0.4`, `--pad 0.08`, `--top`, `--before`, `--after`,
`--model`, `--device cpu|cuda`.

`--animated` (legenda animada em `.mov`) não é aceito aqui: use `--captions`.

## Limitações

- Cria uma sequência nova; não corta uma sequência que você já editou.
- Highlights usa só o volume do áudio, e o zoom é fixo e centralizado (como no AutoCut Free).
- O token fica só na variável de ambiente; não o coloque em argumentos nem em arquivos versionados.
- A ponte foi testada contra um servidor de controle simulado (`tests/`) e contra o motor de importação do FilmCraft
  pelo CLI headless; ainda não foi exercitada ponta a ponta com a janela do app aberta.

## Testes

```sh
python3 -m unittest discover -s integrations/autocut/tests -v
```

## Licença

Motor e ponte: MIT (`LICENSE-AUTOCUT`), © 2026 usaginoyami. O FilmCraft segue MIT OR Apache-2.0.
Se publicar o fork, remova as logos e o nome ArtCraft de `docs/brand/` e do README (licença de marca em
`docs/brand/LICENSE-brand.txt`).

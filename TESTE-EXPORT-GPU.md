# Teste manual de exportação por GPU (AMD VA-API e NVIDIA NVENC)

Este roteiro roda na sua máquina, com a GPU real. Os testes automatizados do repositório
(`cargo test`) não exercitam o encoder de hardware: validam a seleção de backend e os limites
com fakes. Esta página registra o que ainda falta medir em hardware.

Os comandos usam o CLI headless. Troque `<projeto>` pelo seu `.filmcraft` e `<saida>` por um caminho
de saída. Cada teste deve terminar com código de saída 0 e um arquivo MP4 reproduzível.

## Pré-requisitos

- Linux com driver VA-API para AMD (Mesa radeonsi/VCN) ou driver NVIDIA com NVENC.
- Build release: `cargo build --release -p filmcraft-cli`.
- Verifique o encoder disponível:
  - AMD: `vainfo | grep -i "EncSlice"` deve listar `VAProfileH264Main/High` e, para H.265, `VAProfileHEVCMain`.
  - NVIDIA: `nvidia-smi` deve listar a GPU; o NVENC aparece em `ffmpeg -encoders | grep nvenc` (só para conferir o driver).

## Resolução: cada teste em 1080p e em 4K

Crie (ou abra) uma sequência de 1920x1080 e outra de 3840x2160 no projeto de teste.

### AMD (VA-API, via DMA-BUF)

```sh
target/release/filmcraft-cli export <saida>-amd-1080p.mp4 --project <projeto> --format h264 --hardware-export
target/release/filmcraft-cli export <saida>-amd-4k.mp4 --project <projeto> --format h264 --hardware-export
target/release/filmcraft-cli export <saida>-amd-4k-hevc.mp4 --project <projeto> --format h265 --hardware-export
```

O AMD usa `--hardware-export` (rota residente, com DMA-BUF no mesmo nó DRM da GPU AMD). Para NVIDIA use `--hardware-encoding nvencRequired`; para aceitar qualquer GPU use `--hardware-encoding gpuRequired`.

Esperado: exportação concluída; `ffprobe -v error -show_streams <saida>-amd-4k.mp4` mostra 3840x2160 e
`codec_name=h264`. Se o driver recusar, a exportação deve falhar com erro explícito. Nunca deve
concluir por CPU.

### NVIDIA (NVENC)

```sh
target/release/filmcraft-cli export <saida>-nv-1080p.mp4 --project <projeto> --format h264 --hardware-encoding nvencRequired
target/release/filmcraft-cli export <saida>-nv-4k.mp4 --project <projeto> --format h264 --hardware-encoding nvencRequired
```

Esperado: concluída; `ffprobe` mostra 3840x2160. NVENC H.265 no Linux não é suportado nesta versão:
a exportação deve falhar com erro explícito (não cair para VA-API nem CPU).

### Sem GPU requerida (referência)

```sh
target/release/filmcraft-cli export <saida>-cpu-4k.mp4 --project <projeto> --format h264
```

## O que confirmar

- Nenhuma exportação com `*Required` terminou por encoder de software (verifique o log: o caminho
  de CPU não deve aparecer).
- 4K e 8K não são recusados por teto artificial. Se o driver recusar, a mensagem deve citar o
  tamanho e o máximo declarado pelo driver.
- Repita 4K com um projeto que tenha transição de desfoque (blur) e com transição de luz
  (light multipass): são os estágios com orçamento de memória derivado do adaptador.

## O que ficou sem medição

- Medição real de desempenho (quadros por segundo) em 4K e 8K, AMD e NVIDIA.
- Limite real do driver para 8K em H.264 (o driver declara; não foi medido aqui).
- Alocação de memória de 8K com transições de desfoque em GPU de 8 GB ou menos.
- VA-API H.265 em 8K: o caminho pela CTU alinhada (4352 linhas para 4320) precisa ser confirmado no driver.

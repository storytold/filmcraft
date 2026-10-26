# TESTE-TUDO — integration/all na GPU AMD

Branch: `integration/all` (base `pr/amd-gpu-resident-export-main` @ 1eb86c45, com merge de `feat/export-amd-nvidia`, `feat/timeline-render-cpu-gpu` e `feat/gpu-multipass-all` @ 8601b983).

Regra: linha `skipping`, `SKIPPED`, `0 tests` ou teste ignorado NÃO conta como aprovado. Qualquer um deles = não medido.

## 0. Pré-requisitos

- Linux com GPU AMD (vendor 0x1002), driver RADV (Mesa) ou amdgpu-pro.
- VA-API com `EncSlice` para H.264 e HEVC: `vainfo | grep -i EncSlice` deve listar `VAProfileH264Main` / `VAProfileH264High` e `VAProfileHEVCMain`.
- Pacotes de sistema: `libasound2-dev libudev-dev pkg-config libxkbcommon-dev libwayland-dev libgtk-3-dev libvulkan-dev`.
- Opcional (só para testes de texto): `export CRAFT_FONTS_DIR="$PWD/../craft-fonts"`.
- Ferramentas de oráculo: `ffmpeg`, `ffprobe` no PATH (ou `FILMCRAFT_FFMPEG` / `FILMCRAFT_FFPROBE`).

```bash
git clone https://github.com/storytold/filmcraft && cd filmcraft
git checkout <commit-under-test>
```

## 1. Gates sem GPU (rodar primeiro, na mesma máquina)

Ordem exata:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask layers
cargo xtask assets
cargo xtask wasm
cargo test --workspace --no-fail-fast
```

Estado medido no container de integração (sem GPU AMD, sem oráculos completos):
- fmt, clippy, layers, assets, wasm: exit 0.
- `cargo test --workspace --no-fail-fast`: 3883 passaram, 32 falharam, 51 ignorados. As falhas estão em `filmcraft-gpu` (exigem adaptador AMD; o container só tem llvmpipe), `filmcraft-av1`, `filmcraft-codecs`, `filmcraft-media` (oráculos ffmpeg), `filmcraft-engine --lib` e `filmcraft-ui-egui` (em verificação contra a base). Na AMD o gate precisa ficar verde; qualquer falha aqui bloqueia.

## 2. Export por VA-API (AMD) — `TESTE-EXPORT-GPU.md`

```bash
cargo build --release -p filmcraft-cli
target/release/filmcraft-cli export <saida>-amd-1080p.mp4 --project <projeto> --format h264 --hardware-export
target/release/filmcraft-cli export <saida>-amd-4k.mp4 --project <projeto> --format h264 --hardware-export
target/release/filmcraft-cli export <saida>-amd-4k-hevc.mp4 --project <projeto> --format h265 --hardware-export
ffprobe -v error -show_streams <saida>-amd-4k.mp4
```

Critério: exit 0 nos três; `ffprobe` mostra `width=3840`, `height=2160`, `codec_name=h264` (no 4K H.264). O arquivo HEVC deve ser `hevc`.
Falha fechada: se a GPU/VA-API não estiver disponível, o comando deve falhar. Se concluir por CPU, é falha do teste.
Lacuna conhecida: o roteiro não define a string de log que prova ausência de CPU. Verificar manualmente que nenhuma linha do caminho CPU aparece no log.

## 3. Roteiro Enter / seletor de renderização — `TESTE-RENDER-ENTER.md`

Sem GPU obrigatória para o teste automático:

```bash
cargo test -p filmcraft-engine render_backend -- --nocapture
```

Esperado: três casos passam (admitido, recusado com quadro e motivo, sem rota).

Manual na UI: `Sequence ▸ Render Effects In to Out`, seletor `Enter renderiza: CPU / GPU` na barra de status. Com `GPU`, a mensagem esperada é `GPU render refused at frame N ... no GPU renderer is registered` e nenhum trecho fica verde. Nesta integração ainda não há renderer GPU registrado: o resultado esperado é recusa, não render.

## 4. Vários passes (multipass) na AMD — `TESTE-GPU-MULTIPASS.md`

Gates de unidade (sem adaptador, devem passar):

```bash
cargo test -p filmcraft-render
cargo test -p filmcraft-gpu multipass
cargo test -p filmcraft-gpu -- noise_u64 hash_emul
cargo test -p filmcraft-render -- row_tests vr_wgsl gpufx_roughen gpufx_spacer
```

Medição na AMD (obrigatório, nesta ordem):

```bash
cargo test -p filmcraft-gpu multipass_routes_match_cpu_oracle_on_adapter -- --nocapture
cargo test -p filmcraft-gpu gpu_vr_effects_report_pixel_deltas_against_independent_and_typed_cpu -- --nocapture
```

Devem aparecer como `ok` (não `skipped`):
- `multipass_routes_match_cpu_oracle_on_adapter` para vr_blur, vr_sharpen, vr_denoise, roughen_edges e spacer, com `0 values over` na tolerância EXACT = 1e-4.
- A linha `Required FX parity adapter: <nome> vendor=0x1002`. Se o vendor não for 0x1002, o teste não vale.
- `gpu_vr_effects_report_pixel_deltas_against_independent_and_typed_cpu` com deltas reportados.

Sem GPU, esses testes falham com `FX parity gate requires a GPU adapter`. Isso é esperado e não é aprovação.
Os ids multipass seguem em `UNMEASURED_GPU_ROUTES`: passar aqui não muda a rota de render; eles continuam na CPU até a medição ser registrada.

## 5. Registro de liberação

Anotar no próprio PR ou no `TESTE-TUDO.md` (seção 6): commit testado, nome e vendor da GPU, driver, versão Mesa, saída de `vainfo`, e a saída completa dos comandos da seção 4 e 2.

## 6. Resultados (preencher na AMD)

| Passo | Comando | Resultado | Observação |
|---|---|---|---|
| Gates | seção 1 | | |
| Export AMD 1080p H.264 | seção 2 | | |
| Export AMD 4K H.264 | seção 2 | | |
| Export AMD 4K HEVC | seção 2 | | |
| Enter render_backend | seção 3 | | |
| Multipass unit | seção 4 | | |
| Multipass na AMD | seção 4 | | |
| VR deltas na AMD | seção 4 | | |

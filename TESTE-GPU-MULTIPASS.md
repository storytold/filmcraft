# Teste GPU: rota de vários passes (AMD), cinco efeitos

Status: **nenhum dos cinco efeitos foi medido na GPU.** Todos estão em `UNMEASURED_GPU_ROUTES`
(`crates/render/src/gpufx.rs`), e `evaluate_gpu_effect` os recusa. Seguem na CPU. Passar aqui
não muda rota: só mede. Sem GPU AMD, os testes de paridade falham de propósito. Isso não é paridade.

| id | arquivo de kernel | plano | oráculo CPU |
| --- | --- | --- | --- |
| vr_blur | `crates/gpu/src/vr.wgsl` | `vfx::immersive::vr_plan` | `cpu_reference` em `fx_tests.rs` |
| vr_sharpen | `vr.wgsl` | `vr_plan` | idem |
| vr_denoise | `vr.wgsl` | `vr_plan` | idem |
| roughen_edges | `crates/gpu/src/fx_roughen.wgsl` | `gpufx_roughen::roughen_effect` | idem |
| spacer | `crates/gpu/src/fx_spacer.wgsl` | `gpufx_spacer::spacer_effect` | idem |

Harness: `MULTIPASS_ROUTES` (`crates/render/src/gpufx_multipass.rs`) lista os cinco ids.
`multipass_effect_for(id, w, h)` monta o plano no tamanho do quadro. Parâmetros de medição em
`multipass_case` (`fx_tests.rs` usa `multipass_cx`). `measure_route` recusa qualquer id fora de
`UNMEASURED_GPU_ROUTES`.

## 1. Pré-requisitos

- Linux com GPU AMD (vendor 0x1002), Mesa/RADV ou amdgpu-pro.
- Rust estável recente e `cargo`.
- Branch `feat/gpu-multipass-all`.

```bash
git clone https://github.com/Silvazrlq/filmcraft ~/filmcraft-multipass
cd ~/filmcraft-multipass
git checkout feat/gpu-multipass-all
```

Fontes opcionais (só para os testes de texto): `git clone https://github.com/storytold/craft-fonts ../craft-fonts`
e `export CRAFT_FONTS_DIR="$PWD/../craft-fonts"`.

## 2. Gates sem GPU (devem passar antes da medição)

```bash
cargo fmt --all -- --check
cargo clippy -p filmcraft-render -p filmcraft-gpu --all-targets -- -D warnings
cargo xtask layers
cargo xtask assets
cargo xtask wasm
cargo test -p filmcraft-render
cargo test -p filmcraft-gpu multipass
cargo test -p filmcraft-gpu -- noise_u64 hash_emul
cargo test -p filmcraft-render -- row_tests vr_wgsl gpufx_roughen gpufx_spacer
```

Critério: tudo verde. Sem GPU, `multipass_routes_match_cpu_oracle_on_adapter` e os testes de
paridade também falham com `FX parity gate requires a GPU adapter`. Isso é esperado; é a
diferença entre "não mediu" e "passou".

O que cada teste sem GPU prova:
- `row_tests::vr_plans_match_the_cpu_reference_for_every_layout`: plano VR com semântica de kernel na CPU, por vista, layouts 0, 1, 2. Prova montagem e matemática, não o shader.
- `vr_wgsl_validates` e `kernel_contract_shader_validates_with_naga`: naga valida os WGSL.
- `noise_u64`: naga valida o hash dos kernels de borda; `hash_emul`: u32 bate com `hash_u` em 4M entradas.
- `unmeasured_routes_are_never_in_the_gpu_allowlist` e `multipass_harness_refuses_ids_outside_unmeasured_list`: os cinco ids seguem recusados.

## 3. Medição na GPU AMD

Harness geral (os cinco ids, mesma tolerância `EXACT` = 1e-4):

```bash
cargo test -p filmcraft-gpu multipass_routes_match_cpu_oracle_on_adapter -- --nocapture
```

Roughen por etapas (diagnóstico de divergência; falha nomeando a primeira etapa fora de `EXACT`):

```bash
cargo test -p filmcraft-gpu roughen_edges_stages_match_cpu_on_adapter -- --nocapture
```

Imprime uma linha por etapa: as seis passadas de caixa (canal vermelho contra `depth_stages`, a
réplica CPU da profundidade) e a etapa final (apply, todos os canais contra o oráculo CPU), com o erro
máximo e o índice do pior valor. Não há etapa de fbm separada: o ruído é calculado dentro do apply, então
ele só aparece como diferença na etapa final.

Testes específicos de VR (já existiam):

```bash
cargo test -p filmcraft-gpu gpu_vr_effects_report_pixel_deltas_against_independent_and_typed_cpu -- --nocapture
```

Saída que importa: `<id>: max rel diff ...` com `0 values over`. A primeira linha deve mostrar
`Required FX parity adapter: <nome> vendor=0x1002`. Qualquer outra GPU, ou nenhuma, é falha.
A linha `skipping` não prova nada.

Casos medidos: um por efeito, com os parâmetros de `multipass_case`. Ainda não medido: `edge_type`
0..7, borda pequena e grande, stretch negativo, margens assimétricas, `fill`, raio grande, VR com
layouts 1 e 2 na GPU.

## 4. Critério de liberação, por id

Um id sai de `UNMEASURED_GPU_ROUTES` só se:
1. O harness acima passa na AMD para ele, com `0 values over` e sem `skipping`.
2. Os gates da seção 2 passam.
3. Nenhuma tolerância foi afrouxada.
4. Registre GPU, driver e saída do teste neste arquivo.

Depois, retire o id da lista e confirme que `multipass_harness_refuses_ids_outside_unmeasured_list` e
o teste de classificação seguem verdes.

Se a diferença passar de `EXACT`: copie o erro literal e o comando, não contorne com fallback de CPU.
O blur do roughen soma caixas em ordem diferente da CPU (janela deslizante na CPU, soma direta na GPU),
então pode haver diferença de arredondamento em f32. Se isso aparecer, é a ordem da soma, e a
tolerância precisa de decisão explícita antes de qualquer mudança.

## 5. Riscos conhecidos, sem medição

- **Parâmetros fixos:** os planos usam os parâmetros de `multipass_case`/`vr_effect`. Ligar a instância real do efeito é trabalho separado; até lá a medição vale só para esses parâmetros.
- **Tempo do driver:** cada passe de caixa do roughen é O(r) por pixel; o CPU usa janela deslizante O(1). Borda grande em 4K pode estourar o limite de tempo do driver. Precisa de medição de tempo.
- **Geometria duplicada:** `spacer_params` repete a aritmética de `vfx::distort::spacer`. Mudança futura no CPU não chega à GPU sozinha.
- **Spacer no caminho de mip:** o padrão do projeto (400 px) cai no caminho de mip da CPU; a medição usa margens de 6 px.
- **Tamanho do quadro:** o plano depende de `(w, h)`; o integrador precisa passar o tamanho real.

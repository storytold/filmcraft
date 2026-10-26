# Teste: CPU ou GPU no Enter (renderizar efeitos e transições)

Vale para o Enter na linha do tempo (`Sequence ▸ Render Effects In to Out`), que renderiza os
trechos vermelhos e amarelos em verde.

## Estado atual

- Seletor no canto inferior direito da barra de status: `Enter renderiza: CPU` / `Enter renderiza: GPU`.
  A escolha é salva em `preferences.json` (`timeline.renderBackend`, `cpu` | `gpu`). Padrão: `cpu`.
- `cpu`: comportamento de sempre (rota de referência).
- `gpu`: **falha fechada por trecho**. Hoje nenhum renderer GPU está registrado para prévias, então
  todo trecho é recusado com a mensagem `GPU render refused at frame N ... no GPU renderer is
  registered`. Nada é renderizado e nada cai para a CPU.
- Mesmo com um renderer registrado (só em código, por enquanto), o caminho ainda recusa o Enter com
  `admitted the frames but no GPU frame renderer is wired yet`.

## Etapa 1 (feita): admissão para prévia GPU, sem GPU real

- `GpuPreviewRoute` (trait injetável em `crates/engine/src/previews.rs`): cada trecho é admitido ou
  recusado com um motivo. O pipeline de exportação não foi alterado; `Auto` e `AmdRequired` continuam
  recusados lá.
- Testes sem GPU (`cargo test -p filmcraft-engine render_backend`): (i) trecho admitido passa;
  (ii) trecho recusado dá erro com quadro e motivo, parando no primeiro; (iii) sem rota, o trecho é
  recusado com o motivo "no GPU renderer". A criação de pasta e de job antes do gate não tem teste de
  integração ainda (exige sequência ativa); fica como pendência.
- Nada disso foi medido em GPU real.

## Etapa 2 (pendente): renderer GPU real

- Ligar `export_renderer` (crates/gpu) a esse caminho, só para o conjunto já elegível (`GPU_EFFECTS`)
  e transições do catálogo permitido. O resto continua recusado com motivo.
- Medir na sua GPU quando existir: paridade visual com a CPU, tempo por trecho, e 4K+ sem limite.

## Como testar na sua máquina (AMD ou NVIDIA), hoje

1. Abra o projeto e confira o seletor no canto inferior direito. O padrão deve ser `Enter renderiza: CPU`.
2. Aperte Enter com CPU. A barra deve ficar verde nos trechos renderizados.
3. Troque para `Enter renderiza: GPU`, feche e reabra o FilmCraft: a escolha deve persistir.
4. Aperte Enter com GPU. Esperado hoje: erro na barra de status com o quadro e o motivo. **Nenhum
   trecho fica verde** e nenhum quadro é renderizado pela CPU. Se algum trecho ficar verde com GPU,
   registre como defeito (violação de falha fechada).
5. Volte para CPU antes de encerrar.

## Sem medição

- Nenhuma medição de paridade GPU x CPU ainda.
- Nenhum teste rodou em GPU real; os testes automatizados não usam GPU.

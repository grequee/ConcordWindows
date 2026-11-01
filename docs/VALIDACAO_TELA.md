# Validação dos perfis de tela — 30/10/2026

Esta revisão ainda não atende a todos os critérios do `HANDOFF_CODEX.txt`.
O M4 permanece aceito; a validação final do pipeline novo e o rebuild dos
instaladores estão pendentes. Os resultados abaixo distinguem os logs
preexistentes dos comandos executados pelo Codex no ambiente restrito.

## Correção no código

O Clippy encontrou `too_many_arguments` em `capture_loop` e `encode_loop`,
ambas com oito argumentos em `src-tauri/src/screen/linux.rs`. O estado
compartilhado de execução, solicitação de keyframe e preview foi agrupado em
`ShareControl`, clonado pelas threads. O fluxo, a fila de dois frames e o slot
de preview mantêm o comportamento anterior.

A revisão também corrigiu a descrição da fila: `SyncSender::try_send`
descarta o **novo** frame quando ela está cheia. O código não remove o frame
mais antigo da fila. O preview, por sua vez, substitui o frame anterior pelo
mais recente.

`1080p60` continua experimental. A pedido do usuário, passou a aparecer como
`1080p60 (exp.)` no seletor, usando o perfil existente de 6.000 kbps, sem exigir
variável de ambiente. O perfil padrão continua sendo 720p30. O check do
frontend passou sem erros/warnings e o build foi refeito para incluir a opção.

Depois dos soaks de 720p60/1080p30, foi adicionado o backend Wayland/PipeWire
(`crates/media/src/wayland.rs` e `wayland_capture.c`), com portal ScreenCast e
PipeWire. O `ShareControl` também ganhou um caminho de `reconfigure` que troca
somente o encoder; o ator usa esse caminho e preserva a captura autorizada. O
binário do soak de 720p30 foi compilado antes de a feature `wayland` entrar nas
dependências do `concord-app`, então ele não exercita esse backend; a validação
visual do Wayland continua pendente.

## Soaks de 10 minutos

| Perfil | Duração | Quadros remotos no soak | FPS médio | CPU agregada | RSS final | Erros de decode | Estado |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 720p60 | 600,1 s | 35.508 | 59,2 | 174,6% | 255,2 MiB | 0 | Concluído; teste passou; binário anterior ao `ShareControl`/Wayland |
| 1080p30 | 600,1 s | 17.893 | 29,8 | 78,4% | 296,0 MiB | 0 | Concluído; teste passou; binário anterior ao `ShareControl`/Wayland |
| 720p30 | 600 s | em execução | — | — | — | — | Em execução no backend X11; binário anterior ao `ShareControl`/Wayland |

Fonte de 720p60: `/tmp/opencode/screen-720p60-10min.log`, SHA-256
`cb0fb68be01f1602291a00bd0bdc7ef36e5bf289a5517af4ba721311be7e0a42`.
O log contém `screen soak done` e `test result: ok`; o teste completo durou
606,19 s. A última leitura periódica registra 35.641 frames acumulados; a
diferença de 133 frames corresponde ao contador anterior ao início do soak.
O RSS permaneceu próximo de 255,4 MiB durante a execução. Esse binário não
imprime `screen soak sender`, portanto não há medição final de tx_frames,
tx_keyframes e video_lost para esse soak.

O log de 1080p30 é `/tmp/opencode/screen-1080p30-10min.log`. O soak concluiu em
600,1 s com 17.893 quadros remotos, 29,8 fps, 78,4% de CPU agregada, RSS de
296,0 MiB e zero erros de decode. O transmissor registrou `tier=1080p30
tx_frames=18002 tx_keyframes=582 video_lost=0`. O binário é anterior à
refatoração de `ShareControl` e ao backend Wayland.

O soak de 720p30 está em `/tmp/opencode/screen-720p30-10min.log`. O binário
usado nesse soak foi compilado antes de a feature `wayland` entrar nas
dependências do `concord-app`: o executável não contém o backend
Wayland/PipeWire nem linka `libpipewire`, e a captura é X11. Os memfds do
PipeWire vistos no processo são do áudio (CPAL), não da tela. Portanto os três
soaks de 10 minutos são X11 e antecedem a refatoração `ShareControl`/Wayland.
Não apagar nem truncar esses logs.

As medições de 720p60 e 1080p30 são de dois clientes no mesmo processo, após
trocas de perfil, e antecedem a refatoração de `ShareControl`. Elas usaram
captura X11; nesta sessão Wayland o root do Xwayland retorna pixels pretos.
Os contadores validam o transporte e a decodificação, mas não comprovam
qualidade visual nem desempenho com conteúdo de desktop em movimento. O soak
de 720p30 do pipeline serial antigo não valida o pipeline atual.

## Validação inicial, antes do backend Wayland

| Comando | Resultado |
| --- | --- |
| `cargo fmt --all -- --check` | Passou após a refatoração |
| `cargo check --workspace --all-targets` | Passou após a refatoração |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passou após corrigir as duas funções |
| `cargo clippy -p concord-media --features x11 --all-targets -- -D warnings` | Passou |
| `cargo test --workspace --lib` | 61 testes passaram; 4 ignorados |
| `cargo test -p concord-media --features x11 --lib` | 20 testes passaram; 3 ignorados |
| `npm run check` | 0 erros, 0 warnings |
| `npm run build` | Passou |
| `cargo test --workspace` | Falhou ao abrir socket no primeiro teste de integração RTC |
| `cargo test --workspace --no-fail-fast` | 61 testes passaram; 8 testes falharam em 5 targets por sockets bloqueados; 7 ignorados |
| `cargo test -p concord-app --test screen_smoke -- --ignored --nocapture` | Bloqueado no bind do servidor de sinalização: `Operation not permitted (os error 1)` |
| `cargo test -p concord-app native_viewer_opens_draws_and_closes -- --ignored --nocapture` | Falhou em `gtk::init`: `Failed to initialize GTK` |

As oito falhas de integração estão em `audio_signaling` (1),
`bidirectional_video` (1), `data_channel_signaling` (1), `video_signaling` (1)
e `signaling_flow` (4). Todas ocorrem na criação/bind de sockets, com
`PermissionDenied`/`Operation not permitted`. A tentativa direta de bind em
`127.0.0.1` e a conexão ao socket `/tmp/.X11-unix/X0` também retornaram
`Operation not permitted`; `xdpyinfo` não abriu `DISPLAY=:0`.
Esses testes continuam sem validação nesta sessão; não foram marcados como
aprovados nem alterados para contornar a restrição.

Logs dos comandos: `/tmp/opencode/codex-cargo-check.log`,
`codex-clippy-workspace.log`, `codex-clippy-media.log`,
`codex-cargo-test-workspace.log`, `codex-cargo-test-all.log`,
`codex-cargo-test-lib.log`, `codex-cargo-test-media.log`,
`codex-screen-smoke.log`, `codex-native-viewer.log`,
`codex-npm-check.log` e `codex-npm-build.log`, todos no mesmo diretório.

## Pacotes e próximas etapas

Os `.deb`, `.rpm` e AppImage existentes são de 27/10/2026 e não incluem os
perfis novos nem o seletor. Seus hashes no README foram identificados como
históricos. O rebuild não foi iniciado nesta revisão: o handoff determina
fazê-lo somente depois da validação, ainda incompleta.

1. Consolidar o resultado externo de 1080p30 e executar 720p30 por 600 s,
   sequencialmente, em ambiente com display, áudio e sockets disponíveis.
2. Reexecutar os testes de integração, `screen_smoke` e o viewer GTK nesse
   ambiente. Registrar a versão do código usada nos soaks.
3. Em Plasma X11 nativo (`XDG_SESSION_TYPE=x11`), verificar pixels do preview
   e do viewer remoto, seletor e troca de perfil durante a transmissão.
   O workaround de X11 nativo ainda não foi confirmado pelo usuário.
4. Com a validação concluída, executar `npm run tauri build`, extrair os
   pacotes, verificar dependências com `ldd` e testar inicialização.
5. Atualizar os tamanhos/hashes do README e as evidências de README,
   ROADMAP, PLANO, CALL_LIFECYCLE e ADR 0006 somente com resultados finais.

Windows/WGC, oito participantes com vídeo e a promoção de 1080p60 permanecem
fora desta entrega. A implementação de Wayland foi adicionada a pedido do
usuário depois do handoff original; sua validação visual continua pendente.

## Revisão Wayland

O backend seleciona automaticamente o portal em sessões Wayland. Consentimento
e preparação rodam fora do ator de áudio; sair da chamada aborta a preparação.
A troca de qualidade mantém a sessão do portal e a faixa WebRTC. Buffers são
validados e o recorte do compositor é respeitado. O caminho de pixels continua
nativo, sem WebView/IPC. Detalhes e teste manual em [WAYLAND.md](WAYLAND.md).

- `cargo test --workspace --lib`: 69 aprovados, 5 ignorados, incluindo os novos
  testes de cores/stride, buffers inválidos, cache/revogação, cancelamento,
  escolha de backend e troca de perfil com decode VP8 real.
- `cargo check --workspace --all-targets`, Clippy do workspace com `-D warnings`
  e check de mídia com `--no-default-features --features wayland` passaram.
- `npm run check`: 0 erros/0 warnings; `npm run build` passou.
- A análise estática da ponte C com `clang --analyze` passou.
- O teste interativo `portal_captures_real_frames_and_closes` foi executado e
  bloqueado no D-Bus: `Could not connect: Operation not permitted`. Não há
  validação visual de Wayland nesta sessão.

Logs: `/tmp/opencode/codex-wayland-test-lib.log`, `codex-wayland-check.log`,
`codex-wayland-clippy.log`, `codex-wayland-portal-test.log` e
`codex-wayland-build.log`, no mesmo diretório. O build sem bundles produz
`target/release/concord-app`; os instaladores antigos continuam sem o backend.
O build release sem bundles passou. `ldd` não encontrou bibliotecas ausentes e
confirmou o link com PipeWire. A tentativa de abrir o executável nesta sessão
falhou na inicialização GTK, coerente com o display inacessível. Logs adicionais:
`/tmp/opencode/codex-wayland-ldd.log` e `codex-wayland-launch.log`.

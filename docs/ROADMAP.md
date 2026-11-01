# Roadmap — Concord

Este roadmap organiza a entrega em fatias verticais. Cada marco tem um resultado demonstrável; não contamos “porcentagem pronta”. Os marcos 1–4 foram entregues no Linux/X11, com perfis de tela 720p30, 720p60 e 1080p30; **1080p60 experimental, validação de escala, Windows, validação do Wayland/PipeWire e WAN continuam pendentes**.

## Marco 0 — Fundação (concluído)

**Objetivo:** provar que a arquitetura Rust + Tauri 2 + Svelte 5 compila e tem fronteiras claras.

- Workspace com `concord-protocol`, `concord-core`, `concord-media`, `concord-rtc`, `concord-signaling-server` e `concord-app`.
- Protocolo versionado, validação e testes de serialização.
- Domínio síncrono com salas, capacidade, presença e histórico limitado.
- App Tauri compilando com UI Svelte e ícones nativos.
- Lints, formatação e documentação inicial.

**Saída:** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` e `npm run build` passam.

## Marco 1 — Sinalização, salas e texto (concluído)

**Objetivo:** duas pessoas entrarem no mesmo servidor, escolherem uma sala e conversarem em tempo real com presença.

### Entregue

- Servidor Axum + WebSocket com `/ws` e `/healthz`.
- Autenticação por token de convite, com comparação em tempo constante.
- Rate limit por conexão e bloqueio temporário por IP após falhas de autenticação.
- Configuração TOML validada, tokens redigidos em `Debug` e limite de clientes.
- Salas configuráveis, capacidade 2–8, entrada/saída e presença.
- Chat em tempo real, histórico curto e deduplicação no reducer.
- Cliente WebSocket nativo em Rust com URL normalizada, heartbeat, snapshots e gerações de conexão.
- IPC Tauri: `get_snapshot`, `connect`, `disconnect`, `refresh_rooms`, `join_room`, `leave_room`, `send_chat`, `set_presence`, `clear_error`.
- Interface completa em Svelte 5: landing, conexão, lobby, lista de salas, chat, participantes e controles de presença.
- Troca de sala automática: ao escolher outra sala, o cliente sai da atual e entra na nova.
- Presença sincronizada (mudo, sem áudio, compartilhando tela e na chamada) com controles reais de mudo/áudio na chamada.
- UI responsiva, fontes locais e acessibilidade básica.
- Testes de integração de servidor, incluindo health check.
- Empacotamento release validado: binário de 6,2 MB, `.deb` de 2,4 MB e AppImage de 108 MB (WebKitGTK embutido).
- Documentação de deploy: TOML, Caddy, systemd e coturn.

### Critérios de aceite verificados

- [x] Servidor sobe com configuração válida e recusa configuração inválida.
- [x] `/healthz` responde sem deadlock e reporta salas/protocolo.
- [x] Token inválido recebe erro fatal e socket fechado.
- [x] Cliente autenticado recebe a lista de salas.
- [x] Dois clientes entram na mesma sala, trocam chat e presença.
- [x] Relay de SDP/ICE é restrito à sala e ao destinatário.
- [x] App desktop compila e conecta ao servidor real.
- [x] Fluxo GUI validado manualmente com duas janelas: conexão, lobby, entrada, troca de sala, chat em tempo real, presença, saída e desconexão.
- [x] UI sem erros de TypeScript/Svelte.
- [x] Testes de backend e integração passam.

### Limitações no fechamento do Marco 1

- Estado em memória; sem banco.
- Reconexão manual: após queda, o usuário precisa conectar de novo.
- Sem TLS embutido; produção requer proxy.
- Sem expiração/revogação de token em runtime.
- Ainda não havia voz, tela ou dispositivos de mídia reais no app.
- Sem assinatura de instaladores ou pipeline CI.
- Testes de UI são manuais; não há E2E automatizado do WebView.
- AppImage portátil fica acima do alvo de 30 MB por embutir WebKitGTK/JSC/ICU; o `.deb` é o artefato leve.

**Saída:** milestone demonstrável com signaling + texto + presença. Voz e tela entraram nos marcos seguintes.

## Marco 2 — Fundação de mídia e transporte (concluído no Linux; Windows/Wayland pendentes)

**Objetivo:** escolher e provar a stack nativa de captura e WebRTC antes de prometer voz/tela.

### Entregue no Linux

- Spike real de `webrtc` 0.21.1 com ICE, DTLS, SCTP e data channel sobre a
  sinalização existente.
- Papel determinístico por `UserId` (menor ID oferece) e trickle ICE com
  buffer de candidatos, evitando glare na negociação inicial.
- Teste de integração com dois clientes WebSocket reais; hello bidirecional em
  aproximadamente 0,86 s.
- TURN de laboratório com coturn 4.18.0 em Docker e teste forçado por relay.
  Os dois pares selecionados foram `relay` e trocaram hello.
- Captura e playback local com CPAL: enumeração de devices, resolução do
  device padrão, conversão de formato, ring buffer e contadores de drop,
  underrun e erro.
- Captura X11/Xwayland por MIT-SHM (`XShmGetImage`), convertida para BGRA
  fora do WebView. Medição em release: 1280×720 a 30,00 fps e 60,00 fps;
  ~105,5 MiB/s a 30 fps.
- ADR 0006 (WebRTC e codec) e contrato de ciclo de vida de chamada em
  [`CALL_LIFECYCLE.md`](CALL_LIFECYCLE.md), posteriormente implementado no M3.
- Benchmarks iniciais de CPU/RSS dos probes de áudio e tela.

### Pendente ou limitado

- Windows Graphics Capture e validação de ICE/DTLS no build Windows 10/11:
  sem máquina Windows disponível neste ciclo.
- Wayland nativo via xdg-desktop-portal + PipeWire: X11/Xwayland é o primeiro
  backend; janelas Wayland nativas não são capturadas pelo root X11.
- 1080p60 estável e teste de rede WAN real com NAT restritivo.
- Encoder/decoder VP8 e pipeline RTP: concluído no M4 em Linux/X11; WGC e
  PipeWire continuam pendentes.
- Execução contínua de 10 minutos com captura e playback simultâneos; a
  captura contínua foi validada por 10 minutos e o playback por 5 s. O soak de
  vídeo de 10 minutos do M4 foi registrado na seção do Marco 4.

### Evidências

| Item | Resultado |
| --- | --- |
| Data channel normal | `hello from A`/`hello from B`; ambos abertos; ~0,86 s |
| Data channel forçado por TURN | papéis alternados; candidatos e pares selecionados `relay`; hello bidirecional |
| XShm 720p30 (release) | 300 frames em 10,000 s; média 33,26 ms; máximo 42,15 ms; 105,5 MiB/s; RSS ~10,9 MB; ~48% de um núcleo |
| XShm 720p60 (release) | 600 frames em 10,000 s; média 16,65 ms; máximo 27,45 ms; 210,9 MiB/s |
| Áudio captura 2 s | 48 kHz estéreo; 191.488 amostras; 0 drops; 0 underruns; 0 erros |
| Áudio playback 5 s | 48 kHz estéreo; saída = entrada; 0 underruns; ~3,0% CPU e ~10,4 MB RSS |
| Áudio captura 10 min | 57.605.120 amostras; 600,052 s; 0 drops; 0 underruns; 0 erros; ~1,2% CPU e ~10,4 MB RSS |
| TURN lab | `deploy/turn/compose.yaml`; coturn 4.18.0; loopback apenas |

### Critério de saída

- [x] Duas instâncias trocam data channel “hello” via WebRTC usando a sinalização existente.
- [x] Captura de tela gera frames em 720p30 sem atravessar o WebView (Linux/X11, release).
- [x] TURN é usado quando não há caminho direto (`force_relay`); o teste mostra os pares ICE escolhidos.
- [x] Nenhum `unwrap`/`expect` novo em produção; `unsafe` do XShm justificado.
- [x] Benchmarks iniciais de CPU/RAM registrados para os probes.
- [x] Captura local de áudio estável por 10 minutos; playback local validado
  em janela curta.
- [ ] Build e validação em Windows, além de Wayland.

O marco está **concluído no Linux/X11**. A fundação sustenta o M3, mas a
promessa multiplataforma ainda exige Windows e Wayland; esses itens continuam
no escopo dos marcos seguintes.

## Marco 3 — Voz em malha (concluído no Linux; escala/WAN ficam no backlog)

**Objetivo:** conversa de voz utilizável no grupo, com controles reais, sem áudio
no WebView nem no servidor de sinalização.

### Entregue

- Extensão de protocolo compatível: `Participant.in_call` e
  `UpdatePresence.in_call`, com testes de serialização para frente e para trás.
- Ciclo de vida de chamada em [`CALL_LIFECYCLE.md`](CALL_LIFECYCLE.md).
- Ator Rust único por chamada, dono de peers, dispositivos, encoder,
  decoders, snapshot e retry.
- `CallSlot::{Idle,Starting,Active}` com buffer de presença/sinais durante a
  abertura do microfone e um `call_lifecycle` que serializa entrada/saída. Isso
  corrigiu uma corrida real de comandos perdidos no início da chamada.
- Remetente da oferta determinado pelo menor `UserId`; uma
  `RTCPeerConnection` por par; trickle ICE com buffers local/remoto.
- Faixa Opus em 48 kHz mono, quadros de 20 ms, 32 kbps e payload type 111.
- Captura/playback CPAL com downmix, reamostragem, rings e contadores.
- Jitter buffer de até 4 quadros por peer, detecção de perda/duplicata/atraso,
  PLC do Opus e mixagem com `tanh` e prefill de playback.
- Mudo bloqueia codificação/envio reais; ensurdecido consome e decodifica o
  áudio remoto, mas mistura zero na saída.
- Reconexão por peer após 2 s, deduplicada; sessões antigas têm `instance`
  própria e eventos atrasados são descartados.
- `join_call`/`leave_call` registrados no IPC; dock de chamada em Svelte com
  entrada/saída, mudo/áudio reais, presença e estatísticas de perda/PLC.
- Teardown de chamada em `leave_call`, troca/saída de sala e `disconnect`;
  leitores de gerações antigas não podem encerrar uma chamada nova.
- Teste de app com dois clientes reais, servidor efêmero no processo,
  `AppHandle` mock, CPAL e Opus reais.

### Evidências

| Item | Resultado |
| --- | --- |
| Opus sobre WebRTC | `audio_signaling` envia 440/660 Hz codificados e recebe PCM não silencioso; passa em ~1 s |
| Dois clientes completos | ambos os peers conectados; áudio bidirecional; mute interrompe `tx_frames`; unmute retoma; deafen continua `rx_frames`; saída propaga |
| Ciclo de vida no teste | `leave_call`, saída da sala com chamada ativa e `disconnect` encerram sem vazar áudio/peers |
| Qualidade de referência | `playback_underruns: 0`, `stream_errors: 0`; captura pode registrar 1–2 underruns de inicialização |
| Relay forçado | `CONCORD_FORCE_RELAY=1` contra coturn 4.18.0 local; áudio bidirecional em 9,62 s; 0 underruns/erros |
| UI | `npm run check` e `npm run build` passam; botões refletem o estado real do ator |

Comandos reproduzíveis:

```bash
cargo test -p concord-rtc --test audio_signaling
cargo test -p concord-app --test voice_smoke -- --ignored --nocapture

docker compose -f deploy/turn/compose.yaml up -d
CONCORD_TURN_URL='turn:127.0.0.1:3478?transport=udp' \
  CONCORD_TURN_USERNAME=concord \
  CONCORD_TURN_CREDENTIAL=concord-test \
  CONCORD_FORCE_RELAY=1 \
  cargo test -p concord-app --test voice_smoke -- --ignored --nocapture
docker compose -f deploy/turn/compose.yaml down
```

O teste de voz exige microfone e saída reais. O relay é um coturn de loopback,
não um TURN de produção; o teste prova o caminho de relay, não WAN/TLS.

### Critério de saída

- [x] Pipeline Opus nativo (captura → encode → RTP/SRTP → decode → playback)
  sem áudio no WebView ou no servidor de sinalização.
- [x] Entrar/sair da chamada sem sair da sala de texto.
- [x] Presença `in_call` refletida no servidor e na UI.
- [x] Mudo/ensurdecido afetam áudio real e controles refletem o ator.
- [x] Par entra/sai sem derrubar a chamada dos demais.
- [x] Sair da sala e desconectar encerram a chamada.
- [x] Jitter buffer, perda, PLC e contadores por peer.
- [x] Dois clientes Linux conversam em malha com áudio bidirecional.
- [x] Relay forçado local passa com áudio.
- [ ] Quatro e oito participantes conversam em malha por 30 minutos sem queda.
- [ ] WAN/NAT real com TURN de produção e TLS.
- [x] Preparação de deploy Render/Docker, bind por PORT e entrega de STUN/TURN
  aos apps autenticados. Guia em [deploy/render](../deploy/render/README.md).
- [ ] Publicação na conta Render e teste de voz/tela entre redes reais.
  Primeiro deploy sem TURN, por decisão do usuário em 31/10/2026; testar
  STUN e conexão direta entre os PCs do grupo antes de considerar relay.
- [x] Ganho manual de microfone e supressão opcional SpeexDSP, com painel e persistência local.
- [ ] AEC/AGC e teste formal com alto-falantes.
- [ ] Hot-swap de dispositivos.
- [ ] Renegociação/ICE restart; hoje a recuperação recria a sessão.
- [ ] Validação de voz em Windows e Wayland nativo.
- [ ] Medição formal de CPU/RAM de uma chamada de 8 pessoas.

**Conclusão:** o MVP de voz funciona com dois clientes no Linux e já tem a
arquitetura de malha para 2–8. O critério original de oito pessoas por 30
minutos, WAN e AEC **não** foi atingido; esses itens são bloqueadores de
produção, não de demonstração local.

**Aceite:** o usuário testou o AppImage com voz e aceitou o marco. Os itens
abertos acima (8 pessoas/30 min, TURN de produção, AEC, hot-swap, ICE restart e
voz em Windows/Wayland) passam ao backlog do M5 e não bloqueiam o M4.

## Marco 4 — Compartilhamento de tela (concluído no Linux/X11 com 720p30/720p60/1080p30; 1080p60 experimental e expansão de plataforma pendentes)

**Objetivo:** compartilhar uma tela com janela nativa de visualização, sem
colocar frames no WebView nem no IPC do Tauri.

### Entregue

- Captura X11/Xwayland por MIT-SHM (`XShmGetImage`) em tela cheia, com perfis
  720p30, 720p60 e 1080p30 (e 1080p60 experimental) e conversão BGRA/I420. A
  escala/conversão usa libyuv (SIMD) quando instalada, com fallback para o
  escalador bilinear em Rust.
- Caminho rápido de conversão BGRA para o caso X11 BGRX/BGRA comum; o probe de
  release mediu 7,00 ms/frame em 1920×1080.
- Captura e encode em threads separadas, com fila limitada de dois frames e
  descarte de frames atrasados. O preview local usa um slot de “último frame”,
  sem enfileirar BGRA.
- Encoder e decoder VP8 via libvpx do sistema (bindgen em tempo de build), com
  perfis validados, keyframes e pedidos de keyframe por PLI/FIR.
- Renegociação WebRTC para adicionar e remover a faixa de vídeo sem recriar os
  pares. O offerer determinístico cria as ofertas; o answerer envia
  `SignalPayload::Renegotiate` e instala sua faixa ao aplicar a oferta.
- Faixa de vídeo `sendrecv`: quando o answerer começa a compartilhar, o offerer
  instala uma faixa local ociosa antes de ofertar. Isso garante receiver no
  answerer e permite que qualquer lado comece a enviar depois. Cada nova faixa
  usa um SSRC novo para evitar rejeição de replay do SRTP.
- `CallActor` controla captura, encoder, presença `screen_sharing`, faixas
  locais, decoders remotos, contadores e liberação de recursos. A presença do
  participante, não o fim do track remoto, fecha a janela do espectador.
- Janela nativa GTK3 para preview local e tela remota, desenhada com Cairo. Os
  frames decodificados nunca são serializados para a UI; a UI só recebe o
  snapshot e os contadores.
- UI: botão de compartilhar na dock e na lista de participantes, seletor de
  qualidade com 720p30/720p60/1080p30 e 1080p60 experimental, estado de transmissão,
  erros de captura e métricas de vídeo por peer. `1080p60 (exp.)` usa o perfil
  existente de 6.000 kbps e também pode ser escolhido pela variável de ambiente.
- Troca de qualidade em tempo real: o ator substitui apenas o encoder,
  mantendo a captura autorizada, a faixa WebRTC e a presença `screen_sharing`. Em caso de falha, o
  perfil anterior é restaurado.
- Testes: sinalização de vídeo, dois peers VP8 simultâneos, smoke com dois
  clientes completos e captura X11 real, teste da janela GTK e soak com
  `CONCORD_SCREEN_SOAK_SECS`.

### Evidências reproduzíveis

```bash
cargo test -p concord-rtc --test video_signaling
cargo test -p concord-rtc --test bidirectional_video
cargo test -p concord-app --test screen_smoke -- --ignored --nocapture
cargo test -p concord-app native_viewer_opens_draws_and_closes -- --ignored --nocapture
cargo test -p concord-media --features x11
npm run check
npm run build
cargo clippy --workspace --all-targets -- -D warnings
```

- `video_signaling` adiciona, envia e remove uma faixa VP8 pela sinalização.
- `bidirectional_video` cobre o caso mais difícil: um lado compartilha, para, e
  os dois passam a compartilhar ao mesmo tempo; ambos decodificam VP8.
- `screen_smoke` usa dois `DesktopState`, servidor efêmero, áudio real e captura
  X11 real. Cobre compartilhamento único, parada, compartilhamento simultâneo,
  nova parada, troca de perfil em tempo real e zero erros de decodificação.
- O teste nativo abre, desenha um frame e fecha uma janela GTK.
- O teste de mídia cobre conversão BGRA/I420, escala bilinear e round-trip VP8.
- Com libyuv, o probe de release mediu, em frame sintético 1920×1080 BGRA:
  720p30 19,70 ms, 720p60 14,18 ms, 1080p30 28,50 ms e 1080p60 22,90 ms por
  quadro (prepare+preview + encode + decode). A captura full-screen X11 com o
  caminho rápido mediu 7,00 ms/frame.
- Soaks curtos de 30 s com dois clientes no mesmo processo: 720p30 875 quadros
  (29,2 fps, 192,1 MiB), 720p60 1746 quadros (58,2 fps, 243,2 MiB) e 1080p30
  879 quadros (29,3 fps, 299,8 MiB), todos com zero erros de decode. O 720p60
  teve captura/encode a 60 fps sem drops e 60,3 fps remotos no segundo trecho de
  10 s; a média inclui o warm-up.
- O soak de 10 minutos de 720p60 com o pipeline novo mediu 35.508 quadros
  remotos (59,2 fps), 174,6% de CPU agregada e RSS estável em 255,2 MiB, sem
  erros de decode. O de 1080p30 mediu 17.893 quadros (29,8 fps), 78,4% de CPU
  agregada, RSS estável em 296,0 MiB e zero erros; o transmissor registrou
  18.002 quadros e 582 keyframes. O soak de 720p30 será registrado após a
  execução.
- Nesta sessão Wayland a captura do root X11 retorna preto; CPU e bitrate não
  representam conteúdo real, mas FPS e estabilidade do pipeline são válidos.
- Os três pacotes Linux da revisão anterior do M4 (27/10/2026) foram gerados: `.deb` e `.rpm` de 6,27 MiB com
  libvpx/libyuv/libjpeg em `/usr/lib/concord` e `RUNPATH` para elas, e AppImage
  de 111 MiB. Os binários extraídos dos pacotes foram iniciados com sucesso, e
  os binários do `.deb`/`.rpm` foram carregados em Ubuntu 24.04/Fedora 40 (ldd
  sem bibliotecas faltantes).
  Esses pacotes ainda não incluem os perfis novos nem o seletor; rebuild e
  validação dos novos artefatos estão pendentes. Os testes desta revisão e as
  restrições do ambiente estão em [VALIDACAO_TELA.md](VALIDACAO_TELA.md).

### Pendente e limites

- Em X11, a seleção de monitor/janela continua pendente: captura a tela inteira
  do root. Em Wayland, o portal seleciona monitor/janela conforme o desktop.
- Wayland nativo via portal ScreenCast + PipeWire foi implementado, com
  consentimento fora do ator de áudio e troca de qualidade sem recriar a
  sessão. A validação visual está pendente; Windows WGC não foi implementado.
  Implementação, testes e limites em [WAYLAND.md](WAYLAND.md).
- 1080p60 continua experimental, disponível no seletor; adaptação de bitrate
  e teste de 8 pessoas com vídeo continuam pendentes. Os soaks de 10 minutos de
  720p60 e 1080p30 estão registrados acima; o de 720p30 será medido na
  sequência.
- Não há AEC; a captura de áudio continua sendo a do Marco 3.
- O espectador é uma janela GTK separada, não um painel embutido no WebView.

### Critério de saída

- [x] Adicionar/remover a faixa de tela sem recriar os pares.
- [x] Espectador vê a tela em janela nativa, sem frames no WebView ou no IPC.
- [x] Parar o compartilhamento encerra a thread de captura, a faixa e a janela
  remota sem vazar threads.
- [x] 720p60 estável por 10 minutos nesta máquina, com CPU/RSS medidos
  (35.508 quadros, 59,2 fps, 174,6% de CPU agregada, 255,2 MiB, zero erros de
  decode).
- [x] 1080p30 estável por 10 minutos com o pipeline novo (17.893 quadros,
  29,8 fps, 78,4% de CPU agregada, 296,0 MiB, zero erros de decode).
- [ ] 720p30 estável por 10 minutos com o pipeline novo.
- [ ] 1080p60 funciona quando o orçamento permite e degrada de forma previsível.
  Medição de viabilidade em [README — Viabilidade de 1080p60](../README.md#viabilidade-de-1080p60):
  VP8 por software não sustenta 60 fps nesta máquina; 1080p30 e 720p60 são os
  próximos passos realistas.
- [ ] Wayland usa portal/PipeWire sem quebrar a sessão.
- [ ] Windows usa WGC e tem instalador validado.

## Marco 5 — Resistência e distribuição

**Objetivo:** transformar o protótipo em um app instalável e operável por pessoas reais.

### Escopo

- Reconexão automática da sinalização com backoff e restauração de sala:
  implementada; ver [CONEXAO.md](CONEXAO.md). Validação WAN e retomada automática
  da mídia permanecem pendentes.
- Nome e servidor lembrados no cliente, sem persistir convite: implementado.
- Rotação de convites, expiração e auditoria mínima.
- CI matrix Windows/Linux; testes E2E de servidor e smoke de UI.
- `cargo audit`/`cargo deny`, SBOM e revisão de licenças.
- Assinatura de código Windows e checksums Linux.
- Medições formais: tempo de partida, RAM ociosa, tamanho do instalador e CPU
  de chamadas com 2/4/8 pessoas.
- Acessibilidade: navegação por teclado, foco, contraste e leitores de tela.
- Runbook final, backup/restore (se houver estado), hardening do systemd/Caddy.
- Política de privacidade e limites de responsabilidade alinhados ao uso privado.

### Critério de saída

- Instaladores `.msi`/`.exe` e `.deb`/AppImage reproduzíveis.
- App inicia em menos de 1,5 s e fica abaixo de 80 MB ociosos na máquina de referência.
- Sinalização WSS validada com certificado real.
- Teste de chamada com 8 pessoas em duas redes distintas, com TURN.
- Documentação suficiente para alguém operar sem o autor presente.

## Fora do roadmap atual

- SFU ou salas com mais de 8 pessoas.
- Vídeo por webcam.
- Federação, diretório público ou discovery.
- Versão mobile.
- Gravação/transcrição.
- E2EE de camada aplicacional.
- Temas e plugins de terceiros.

Esses itens só entram se houver demanda real e um ADR justificando o custo de complexidade.

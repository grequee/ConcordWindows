# Plano técnico — Concord

**Versão:** 0.6 (Marcos 1–4 entregues no Linux/X11 com perfis 720p30/720p60/1080p30; 1080p60 experimental e expansão de plataforma pendentes)  
**Alvo:** grupos privados de 2–8 pessoas em Windows 10/11 e Linux  
**Princípio:** interface no WebView, substância em Rust.

## 1. Produto e escopo

Concord é um aplicativo desktop de comunicação privada inspirado na ergonomia do Discord, mas desenhado para um grupo fechado e um servidor auto-hospedado. O produto não é uma rede social, um diretório público nem uma plataforma de comunidades.

### Objetivos do MVP

- Entrar em um servidor apenas com convite.
- Ver e entrar em salas configuradas pelo operador.
- Conversar por texto em tempo real com histórico curto.
- Ver presença e estado de mídia dos participantes.
- Transmitir voz com Opus e WebRTC.
- Compartilhar tela em 720p30, 720p60, 1080p30 ou 1080p60 experimental,
  todos disponíveis no seletor de qualidade.
- Funcionar em malha (mesh) para 2–8 participantes, sem SFU.
- Gerar instaladores nativos enxutos para Windows e Linux.

### Não objetivos do MVP

- Federação entre servidores, descoberta pública ou convites abertos.
- Chamadas de vídeo por webcam.
- Gravação de chamadas ou histórico persistente em banco.
- Moderação avançada, papéis e permissões granulares.
- Mobile, macOS e navegador como clientes de primeira classe.
- SFU, encaminhamento de mídia pelo servidor ou escalabilidade para dezenas de pessoas.
- Criptografia de ponta a ponta independente do DTLS-SRTP do WebRTC.

## 2. Requisitos consolidados

| Área | Requisito |
| --- | --- |
| Plataformas | Windows 10/11 e Linux. X11 implementado; Wayland nativo via portal/PipeWire implementado com validação visual pendente; Windows/WGC pendente. |
| Stack | Rust stable, Tauri 2, Svelte 5, Vite. Sem Electron, sem Chromium embutido. |
| Mídia | Captura, codificação, RTP e playback no processo Rust. O WebView não recebe frames. |
| Sinalização | WebSocket seguro (`wss://`), protocolo JSON versionado, relay de SDP/ICE. |
| Acesso | Tokens de convite de longa duração, sem cadastro. TLS obrigatório fora da LAN. |
| Segurança de mídia | WebRTC com DTLS-SRTP; ICE com STUN e TURN quando necessário. |
| Resiliência | Reconexão manual da sinalização; heartbeat; peers de voz recriados após 2 s. |
| Erros | `anyhow` na borda, `thiserror` nos domínios, sem `unwrap` em produção. |
| Observabilidade | `tracing` estruturado no cliente e servidor; `/healthz`. |
| Qualidade | Testes unitários e de integração, `clippy -D warnings`, `npm run check`. |
| Distribuição | Instalador Windows (`.msi`/NSIS) e Linux (`.deb`/AppImage). |

### Orçamentos de experiência

| Métrica | Alvo | Como medir |
| --- | --- | --- |
| Instalador | < 30 MB | artefato final comprimido |
| RAM ociosa | < 80 MB | RSS do processo principal após 60 s desconectado |
| Partida | < 1,5 s | `time` até janela utilizável em máquina de referência |
| CPU ociosa | < 1% | `top`/`pidstat` com app aberto e sem chamada |
| Latência de texto | < 250 ms na LAN | timestamp de envio/recebimento |
| Voz | < 150 ms ponta a ponta na LAN | medição com relógio comum |
| Tela | 720p30/720p60/1080p30 implementados; soaks de 10 min concluídos em 720p60 e 1080p30; 720p30 em execução | FPS, bitrate e RSS observados |

O WebKitGTK/WebView2 não é contabilizado como “Chromium embutido”, mas seu consumo entra no RSS. Os orçamentos são metas de engenharia, não garantias, e devem ser medidos em builds de release.

Medição do Marco 1 (Linux x86_64, release): binário de 6,2 MB, `.deb` de 2,4 MB (WebKitGTK vem do sistema) e AppImage de 108 MB (empacota WebKitGTK, JavaScriptCore e ICU). O alvo de 30 MB é viável no `.deb`, mas não no AppImage portátil.

Medição inicial do Marco 2 (Linux, release, sem UI): captura XShm 720p30 em ~48% de um núcleo e ~10,9 MB RSS; 60 fps em ~210,9 MiB/s; áudio CPAL em ~1,2% de CPU e ~10,4 MB RSS em captura e ~3,0% em loopback. Ainda não é o RSS do app com WebView.

O Marco 3 adiciona Opus empacotado e o ator de chamada; os tamanhos de release
do app precisam ser medidos novamente antes de comparar com o orçamento.

## 3. Arquitetura

```
┌────────────────────────────── processo Tauri ──────────────────────────────┐
│                                                                            │
│  ┌────────────────────┐    comandos/snapshot IPC    ┌────────────────────┐  │
│  │ WebView + Svelte 5 │ ◄─────────────────────────► │ DesktopState       │  │
│  │ apresentação apenas│                             │ cliente nativo     │  │
│  └────────────────────┘                             │ estado da UI       │  │
│                                                     └───────┬────────────┘  │
│                                                             │               │
│                          ┌──────────────────────────────────┼────────────┐  │
│                          │                                  │            │  │
│                    ┌─────▼─────┐                     ┌──────▼──────┐     │  │
│                    │ concord-  │                     │ concord-rtc │     │  │
│                    │ media     │ ◄── frames/Opus ──► │ + webrtc    │     │  │
│                    └─────┬─────┘                     └──────┬──────┘     │  │
│                          │                                  │            │  │
│                    captura/playback                    DTLS-SRTP         │  │
└──────────────────────────┼──────────────────────────────────┼────────────┘  │
                           │                                  │               │
                    SO (WGC/X11/PipeWire)              UDP / TURN             │
                           │                                  │               │
                           └─────────────── mídia P2P ────────┘               │
                                                                              │
                    ┌─────────────────────────────┐                           │
                    │ WebSocket de sinalização    │                           │
                    │ WSS via Caddy/nginx         │                           │
                    └──────────────┬──────────────┘                           │
                                   │                                          │
                    ┌──────────────▼──────────────┐                           │
                    │ concord-signaling-server    │                           │
                    │ auth · salas · chat · SDP   │                           │
                    └─────────────────────────────┘                           │
```

### Limites entre crates

| Crate | Responsabilidade | Depende de |
| --- | --- | --- |
| `concord-protocol` | Tipos do fio, versão, limites e validação | serde, thiserror |
| `concord-core` | Regras puras de salas, presença, histórico | protocol |
| `concord-media` | Captura X11/CPAL, Opus, jitter buffer, perfis e contratos de mídia | thiserror, cpal, opus, ringbuf, x11rb |
| `concord-rtc` | WebRTC nativo, ICE/TURN, data channel e faixa Opus | protocol, webrtc |
| `concord-signaling-server` | Axum, WS, auth, rate limit, relay | core, protocol |
| `concord-app` | Tauri, cliente nativo, `CallActor`, IPC, janelas | protocol, media, rtc |

A separação impede que o servidor dependa de APIs de desktop e permite testar o domínio sem rede. `protocol` é a fonte de verdade dos dois lados.

### Modelo de execução

- O processo Rust é o dono da conexão e do estado. A UI é uma projeção (`Snapshot`) recebida por evento Tauri `concord:state`.
- Cada comando da UI (`connect`, `join_room`, `send_chat`, `set_presence`, etc.) chama um comando Tauri; a UI nunca abre WebSocket.
- A conexão usa um supervisor Tokio com leitura, escrita, heartbeat e retry automático. Um `generation` monotônico impede que eventos de uma conexão antiga sobrescrevam a atual. O app lembra nome/servidor e recupera a sala após uma queda; veja [CONEXAO.md](CONEXAO.md).
- O servidor Axum mantém um `RoomRegistry` e sessões em memória, protegidos por mutex. O handler de cada socket valida quadros, aplica rate limit e envia mensagens para os canais dos participantes.
- No Marco 3, o áudio roda em um `CallActor` Tokio com CPAL em callbacks de SO, um encoder Opus e um decoder/jitter por peer. A UI só vê o snapshot; frames de áudio não atravessam o IPC.
- No Marco 4, o `CallActor` também controla a captura X11, o encoder VP8 e as faixas de vídeo; preview e tela remota são janelas GTK nativas. Nenhum frame de tela é serializado no IPC.

## 4. Protocolo de sinalização

### Envelope

```json
{
  "v": 1,
  "request_id": "opcional",
  "message": { "type": "join_room", "room_id": "geral" }
}
```

- `v` é a versão do protocolo; incompatibilidade gera erro fatal explícito.
- `MAX_FRAME_BYTES = 128 KiB` limita SDP + ICE + envelope.
- Tipos desconhecidos, campos inválidos e mensagens grandes são rejeitados antes de tocar no domínio.
- `RoomId` é um slug validado e `UserId` é sempre emitido pelo servidor, nunca escolhido pelo cliente.

### Mensagens do cliente

`hello`, `list_rooms`, `join_room`, `leave_room`, `chat`, `update_presence`, `signal`, `ping`.

### Mensagens do servidor

`welcome`, `rooms`, `joined`, `left`, `participant_joined`, `participant_left`, `participant_updated`, `chat`, `signal`, `pong`, `error`.

### Fluxo de entrada

1. Cliente valida URL e normaliza `http(s)://` para `ws(s)://`; rejeita credenciais na URL.
2. Abre WebSocket e envia `hello` com nome, token e metadados de cliente.
3. Servidor compara o token em tempo constante, aplica bloqueio por IP após falhas e responde `welcome` com `user_id`, limites e salas.
4. Cliente lista salas, entra em uma sala e recebe `joined` com participantes e histórico.
5. Chat, presença e sinais são retransmitidos apenas para membros da mesma sala.

O relay de `signal` é deliberadamente opaco: o servidor valida tamanho e destinatário, mas não interpreta SDP/ICE. No M2, o caminho é exercitado por dois clientes WebRTC reais e por um teste forçado via TURN. No M3, a mesma sinalização negocia a faixa Opus da chamada. O ciclo completo está em [`CALL_LIFECYCLE.md`](CALL_LIFECYCLE.md).

## 5. Segurança

### Modelo de ameaça resumido

| Ameaça | Controle atual | Pendência |
| --- | --- | --- |
| Acesso não autorizado | Token de convite + comparação constante | Rotação/expiração automática |
| Escuta de rede | TLS no proxy (`wss`) | Configuração de exemplo e validação de produção |
| Força bruta de token | 5 falhas/60 s por IP, bloqueio 60 s | Limite global e firewall |
| Flood de mensagens | Token bucket por conexão | Limite por IP e proxy |
| Quadros malformados | Limite de bytes + serde + validação | Fuzzing contínuo |
| Spoofing de identidade | `UserId` emitido pelo servidor | — |
| Interceptação de mídia | DTLS/SRTP no áudio e DTLS/SCTP no data channel | Auditoria independente e E2EE de camada aplicacional fora do MVP |
| NAT restritivo | ICE/STUN/TURN implementado; relay de voz validado em loopback | TURN de produção e WAN real |
| Vazamento de token em log | `Debug` redige tokens; convite lembrado localmente a pedido do usuário | Auditoria de logs e memória |

### Regras

- TLS fora da LAN. O servidor escuta em loopback atrás do Caddy/nginx.
- Tokens com pelo menos 16 caracteres ASCII imprimíveis; recomendação de 32 bytes aleatórios em hex.
- Arquivo de configuração com permissão `0600`; convite nunca no código-fonte ou em log.
- O formulário lembra o convite junto ao servidor no armazenamento local, a pedido do usuário. O worker mantém credenciais fora do snapshot e dos eventos da UI; chaves de banco/bucket continuam só no servidor.
- Sem `unwrap`/`expect` em caminhos de produção. `unsafe_code` é warn e deverá ser justificado por backend de captura.
- Dependências revisadas a cada marco; `cargo deny`/`cargo audit` entram no pipeline do M5.

## 6. Mídia nativa (fundação do M2)

### Áudio

- Captura e playback via CPAL implementados em `concord-media`; o probe manual
  enumera devices, usa o device padrão e mede drops/underruns.
- Opus implementado no M3: 48 kHz mono, quadros de 20 ms, 32 kbps, payload type
  111, com encoder/decoder reais e teste de RTP.
- Jitter buffer de até 4 quadros por peer, PLC para lacunas, mistura limitada por
  `tanh`, prefill de playback e contadores de perda/PLC.
- Ganho manual de microfone e supressão de ruído SpeexDSP implementados,
  configuráveis antes/durante a chamada e persistidos localmente. Veja
  [MICROFONE.md](MICROFONE.md). Cancelamento de eco não foi implementado;
  fones são recomendados. A avaliação de AEC fica para um marco posterior.
- Mudo bloqueia codificação/envio; ensurdecido continua consumindo o áudio
  remoto, mas não o mistura na saída. Os controles atuam em devices reais.
- Seleção de entrada/saída, troca durante a chamada, teste de voz, indicação de fala e volume por participante implementados. Validar hardware real; veja [MICROFONE.md](MICROFONE.md).

### Vídeo / tela

- Linux X11/Xwayland: captura MIT-SHM/XShm de tela cheia implementada, com
  perfis 720p30, 720p60 e 1080p30 (e 1080p60 experimental), escala e conversão
  BGRA/I420 no processo Rust. libyuv (SIMD) é usada quando disponível, com
  fallback para o escalador bilinear em Rust. O caminho rápido de BGRA mediu
  7,00 ms/frame em 1920×1080.
- Windows: Windows Graphics Capture (WGC) pendente.
- Linux Wayland: portal ScreenCast + PipeWire implementados, com seleção de
  monitor/janela pelo desktop. A sessão Wayland usa o portal automaticamente,
  mesmo com Xwayland ativo; não há fallback silencioso para o root X11 preto.
  A validação visual permanece pendente. Veja [WAYLAND.md](WAYLAND.md).
- Codec: VP8 baseline via libvpx do sistema, com encoder/decoder, PLI/FIR e
  keyframes; H.264 por hardware será avaliado depois.
- Renegociação: faixa `sendrecv` adicionada/removida sem recriar os pares; cada
  nova transmissão usa um SSRC novo. O offerer mantém uma faixa ociosa para o
  answerer poder iniciar a transmissão depois.
- Perfis: 720p30 (2.500 kbps), 720p60 (3.500 kbps), 1080p30 (4.500 kbps) e
  1080p60 experimental (6.000 kbps). O seletor expõe os quatro perfis;
  1080p60 aparece como `1080p60 (exp.)`, sem garantia de 60 fps sustentados.
- Captura e encode em threads separadas, com fila limitada de dois frames e
  descarte do novo frame quando a fila está cheia. O preview local usa um slot de “último frame”,
  evitando enfileirar BGRA. Trocar o perfil substitui só o encoder e preserva
  a captura autorizada, a faixa WebRTC e a presença.
- Adaptação de bitrate em tempo real continua pendente; o perfil é escolhido
  manualmente.
- Renderização em janela GTK3 nativa, com Cairo; nenhum frame cru atravessa o
  IPC ou o WebView.

### WebRTC e topologia

- Biblioteca `webrtc` 0.21.1 (webrtc-rs), escolhida no ADR 0006.
- Uma `RTCPeerConnection` por par, malha completa para 2–8.
- Data channel e faixa Opus na mesma sessão; sinalização de SDP/ICE usa
  `SignalPayload` existente.
- STUN público ou próprio; TURN obrigatório para redes simétricas. O laboratório
  coturn e os testes `force_relay` de data channel e voz já validam o caminho.
- Papel de offerer pelo menor `UserId`, trickle ICE com buffer e pós-falha
  recriação de sessão a cada 2 s. A faixa de vídeo renegocia adicionar/remover;
  não há ICE restart nem renegociação de áudio.
- Para 8 pessoas, a malha envia 7 fluxos por participante. É aceitável no escopo,
  mas limita 1080p60 em uplinks domésticos. SFU fica fora do MVP.
- O ciclo de entrada/saída, áudio, vídeo, timeout e retry implementado está em
  [`CALL_LIFECYCLE.md`](CALL_LIFECYCLE.md). A voz entrou no M3; a tela e a
  renegociação de faixa de vídeo entraram no M4 em Linux/X11.

## 7. Riscos e mitigações

| Risco | Prob. | Impacto | Mitigação |
| --- | --- | --- | --- |
| Maturidade/build de `webrtc-rs` | Média | Alto | Data channel, TURN, Opus e vídeo VP8 já provados; manter testes de integração a cada upgrade; ICE restart ainda pendente |
| Captura Wayland fragmentada | Alta | Médio | Portal/PipeWire implementado atrás de feature, com seleção pelo desktop; validar em KDE/GNOME e documentar os formatos CPU suportados |
| Root X11 não mostra janelas Wayland | Alta | Médio | Em sessão Wayland, selecionar automaticamente portal/PipeWire, mesmo com DISPLAY do Xwayland; não usar fallback silencioso para a captura preta |
| CPU de captura + encoder em 1080p30 | Média | Alto | O caminho rápido de captura mede 7,00 ms/frame; o probe de 1080p30 fica em 28,50 ms/quadro. O soak de 10 min mediu 78,4% de CPU agregada e 296,0 MiB para dois clientes; em 720p60, 10 min mediram 174,6% e 255,2 MiB. Nesta sessão Wayland a captura X11 retorna preto, então a CPU não representa conteúdo real. 1080p60 fica experimental; adaptar fps/resolução e avaliar H.264 por hardware |
| Captura Windows (WGC) exige cuidado com DPI/GPU | Média | Médio | Spike por SO; testar em Windows 10 e 11; manter taxa de quadros adaptativa |
| WebView2 vs WebKitGTK divergem em CSS/JS | Média | Médio | Evitar APIs exóticas; testar ambos; CSS com fallbacks; sem dependências de browser além do necessário |
| Estouro do instalador de 30 MB | Média | Baixo | Sem Chromium, `opt-level = "s"`, LTO thin, strip, features mínimas; medir por release |
| RSS acima de 80 MB | Média | Médio | Perfis de release, evitar buffers grandes, medir com app ocioso; aceitar que WebView tem piso próprio |
| NAT restritivo derruba conexões | Alta | Alto | TURN desde o início dos testes de voz; laboratório coturn validado; WAN real pendente |
| Malha 8 pessoas satura uplink | Alta | Médio | Limitar perfil por orçamento; comunicar limite; SFU no futuro se necessário |
| Vazamento de token em logs/crash | Baixa | Alto | `Debug` redigido, sem persistência, revisão de logging, não capturar token em telemetria |
| Operação caseira sem TLS | Média | Alto | Caddy no deploy, documentação explícita, `wss` exigido fora de localhost |
| Cliente reconecta e duplica eventos | Média | Médio | Geração monotônica, `instance` por peer, supervisor único, descarte de mensagens antigas e cancelamento da sessão anterior |
| Servidor sem persistência perde histórico | Alta | Baixo | Escopo declarado; snapshots/histórico persistente só se houver requisito real |
| Builds multiplataforma inconsistentes | Média | Médio | CI matrix futura; builds em cada SO; binários assinados no M5 |
| Falta de moderação/abuso | Média | Médio | Grupo fechado por token; rotação manual; papéis e banimento só após necessidade |

## 8. Entrega e operação

- **M1:** sinalização, auth, salas, texto, presença, app desktop, deploy inicial. Concluído.
- **M2:** spike WebRTC/ICE, fundação de captura e áudio, TURN de laboratório. Concluído em Linux/X11; Windows/Wayland e playback longo pendentes.
- **M3:** voz em malha, controles reais de mudo/áudio. Implementado e validado com dois clientes Linux, incluindo relay forçado; testes de 4/8 pessoas, WAN, AEC e 30 minutos pendentes.
- **M4:** tela nativa Linux/X11, janela de visualização, perfis 720p30/720p60/1080p30 e renegociação de faixa. Marco aceito; o pipeline novo tem soaks de 10 minutos concluídos em 720p60 (59,2 fps, 174,6% de CPU agregada, 255,2 MiB e zero erros) e 1080p30 (29,8 fps, 78,4%, 296,0 MiB e zero erros). O soak de 720p30 e o rebuild dos pacotes continuam pendentes. 1080p60 segue experimental; WGC e a validação do PipeWire continuam pendentes. A validação desta revisão está em [VALIDACAO_TELA.md](VALIDACAO_TELA.md).
- **M5:** reconexão automática e recuperação da sala implementadas; faltam validação WAN, retomada automática de mídia, performance, acessibilidade, instaladores assinados, CI e documentação final.

Critérios de release e detalhes por marco estão em [`ROADMAP.md`](ROADMAP.md). Decisões específicas estão em [`adr/`](adr/).

## 9. Decisões em aberto

- Estratégia de hardware encode H.264/VP9 para 1080p60 e máquinas modestas.
- Validação de WGC (Windows) e portal/PipeWire (Wayland).
- Estratégia de AEC e supressão de ruído.
- Credenciais TURN temporárias HMAC e operação de TURN em produção.
- Validação de escala da malha (4/8 pessoas, 30 minutos, WAN).
- Recuperação de peers via ICE restart e renegociação de faixa de áudio.
- Formato de persistência (se houver) e política de retenção.
- Assinatura de código Windows e empacotamento Linux por distribuição.
- Política de expiração/rotação de convites.

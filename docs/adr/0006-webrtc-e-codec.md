# ADR 0006 — WebRTC nativo, TURN e codec de tela

- **Status:** aceita; revisada no M4 após implementar tela no Linux/X11 e no
  ajuste de perfis de tela de outubro/2026
- **Data:** 2026-10-30
- **Marco:** M2–M4 (voz no M3; tela Linux/X11 no M4)
- **Substitui:** decisões pendentes do ADR 0004

## Contexto

O ADR 0004 fixou a topologia em malha e a mídia nativa, mas deixou duas
decisões abertas: qual biblioteca WebRTC usar e qual codec adotar para a tela.
O Marco 2 precisava provar a pilha antes de prometer voz ou compartilhamento.

Restrições que pesam na escolha:

- Cliente desktop em Rust/Tauri 2, sem Electron, Chromium embutido ou
  `getDisplayMedia`.
- Windows 10/11 e Linux; mídia e rede no processo Rust, nunca no WebView.
- 2–8 pessoas em malha, com uma conexão por par.
- O servidor de sinalização não encaminha áudio/vídeo.
- Instalador enxuto, build reproduzível e manutenção de dependências viável.
- TURN obrigatório para redes restritivas; STUN sozinho não é suficiente.
- O codec precisa equilibrar qualidade de tela, CPU em malha e licenciamento.

## Decisão

### Biblioteca WebRTC

Adotar o crate **`webrtc` 0.21.1** (webrtc-rs), em vez de uma biblioteca C++
como libwebrtc ou de WebRTC do navegador. A versão atual é dividida em crates
`rtc-*`, mas a API pública usada pelo Concord continua em `webrtc`.

O `concord-rtc` expõe uma fronteira própria (`PeerSession`, `PeerEvent`,
`RtcConfig`), para que o restante do app não dependa dos tipos internos do
`webrtc`. A integração usa:

- uma `RTCPeerConnection` por par, com um data channel `concord-data` e uma
  faixa de áudio Opus;
- SDP/ICE pelo servidor de sinalização existente (`SignalPayload`);
- trickle ICE com fila de candidatos locais até a oferta/resposta ser enviada;
- papel determinístico: o menor `UserId` é o offerer inicial; o outro é
  answerer, o que evita glare na entrada da chamada;
- TURN configurável em `RtcConfig`, incluindo `force_relay` para testes.

### Codec de áudio

**Opus** é o codec de voz, a 48 kHz, mono, quadros de 20 ms e 32 kbps. O PCM de
captura e playback é convertido no processo Rust; nenhum frame de áudio cru passa
pelo WebView ou pelo servidor. A escolha por mono foi feita no M3 para reduzir
bitrate e custo da malha; não há áudio estéreo nem música de alta fidelidade.

### Codec de tela

Adotar **VP8 baseline** como primeiro codec do compartilhamento de tela. O
perfil inicial é 1280×720 a 30 fps, com perfis 720p60 e 1080p30 disponíveis no
seletor e 1080p60 disponível com indicação de experimental. Justificativa:

- royalty-free e suportado pelo ecossistema WebRTC;
- encode por software é suficiente para o primeiro perfil e reduz o número de
  integrações nativas no M4;
- funciona como linha de base mensurável; se a qualidade/CPU não atender,
  H.264 via encoder de hardware (Media Foundation/VAAPI) será avaliado em ADR
  posterior;
- VP9 fica fora da primeira entrega por custo de encode em malha.

H.264 não é proibido: a negociação futura pode oferecer VP8 e H.264, com
preferência por hardware quando disponível. A primeira implementação, porém,
não depende de encoder proprietário nem de binário externo.

### Captura

- Linux X11/Xwayland: MIT-SHM/XShm, primeiro backend implementado e validado
  no M4. Captura a tela inteira do root; não há seleção de janela.
- Windows: Windows Graphics Capture (WGC) pendente.
- Wayland: xdg-desktop-portal + PipeWire implementados, isolados atrás de
  feature e sem bloquear o backend X11; a validação visual ainda está pendente.
- Áudio: CPAL para captura/playback; Opus implementado no M3.

## Consequências

### Positivas

- Toda a pilha de rede/mídia fica em Rust e fora do WebView.
- O data channel e a faixa Opus já provam ICE, DTLS, SCTP e SRTP com a
  sinalização real do Concord.
- O teste forçado com coturn prova que o TURN é usado quando não há caminho
  direto; o par selecionado pode ser inspecionado.
- O M3 entregou áudio bidirecional real, mute/deafen nativos, jitter buffer,
  PLC e controles na UI sem mover mídia para o WebView.
- VP8 mantém a primeira versão de tela livre de royalties e de SDKs de encoder.
- XShm evita copiar frames pelo socket X11 e funciona onde o `GetImage` central
  retorna `BadMatch` no Xwayland.
- O M4 entregou captura X11, encode/decode VP8, renegociação `sendrecv`, preview
  e visualização GTK nativa sem mover frames para o WebView ou para o IPC.

### Negativas e riscos

- `webrtc` 0.21.1 ainda é uma API em evolução; upgrades exigem rodar os testes
  de data channel, áudio e relay.
- VP8 por software consome CPU, especialmente em 1080p60 e em malha. O alvo
  inicial 720p30 foi ampliado para 720p60 e 1080p30; 1080p60 continua
  experimental porque não há margem medida para 60 fps sustentados nesta
  máquina.
- H.264 por hardware continua sendo trabalho separado por sistema operacional.
- A malha não escala além de 8 pessoas e o TURN precisa de operação contínua.
- Há ganho manual e supressão opcional SpeexDSP antes do Opus, configuráveis
  no app e persistidos localmente. AEC/AGC continuam pendentes; fones são
  recomendados. Veja [MICROFONE.md](../MICROFONE.md).
- A faixa de vídeo renegocia adicionar/remover; não há ICE restart nem
  renegociação de áudio. Peers com falha continuam sendo recriados.
- O backend Wayland/PipeWire foi implementado, mas ainda não foi validado
  visualmente; o WGC não foi implementado.
- O codec de tela está entregue no Linux/X11; WGC, validação do PipeWire,
  1080p60 promovido e escala de 8 pessoas continuam pendentes.

## Revisão do M3 — voz

O M3 implementou a decisão de áudio com:

- Opus 48 kHz mono, 20 ms, 32 kbps e payload type 111 sobre a mesma
  `RTCPeerConnection` do data channel;
- encoder/decoder nativos, jitter buffer de quatro quadros, PLC e mixagem;
- ator de chamada com CPAL, presença `in_call`, mute/deafen reais e retry de
  peer a cada 2 s;
- dock de chamada na UI e teardown em saída de sala/desconexão;
- testes de dois clientes reais com áudio bidirecional, incluindo relay forçado
  via coturn local.

A topologia, a biblioteca e o codec de tela não mudaram. O M3 também confirmou
que a malha de dois clientes funciona sem servidor de mídia; escala de 8 pessoas,
WAN e AEC continuam pendentes.

## Revisão do M4 — tela Linux/X11

O M4 implementou a decisão de vídeo com:

- captura X11/Xwayland por MIT-SHM em tela cheia, escala para 1280×720 e
  conversão BGRA/I420 no processo Rust. libyuv (SIMD) acelera escala e conversão
  quando instalada; o fallback bilinear em Rust mantém o build portátil;
- encoder/decoder VP8 via libvpx do sistema, com PLI/FIR, pedido de keyframe e
  SSRC novo a cada nova faixa;
- renegociação WebRTC para adicionar/remover vídeo sem recriar os pares. A
  faixa é `sendrecv`; o offerer instala uma faixa ociosa quando o answerer
  começa a compartilhar, preservando o receiver;
- `CallActor` com presença `screen_sharing`, preview local, decoders remotos,
  contadores e liberação de captura/thread/janelas;
- janelas GTK3 nativas para preview e tela remota, desenhadas com Cairo. A UI
  Svelte só recebe estado, erros e contadores.

A validação automatizada cobre sinalização de vídeo, dois peers simultâneos e
dois clientes completos com captura X11 real. A validação manual no app abriu
preview e viewer remoto nos dois sentidos, inclusive com os dois compartilhando
ao mesmo tempo, e fechou todas as janelas ao parar. Na sessão KDE Wayland usada
para o teste, o app rodou com `GDK_BACKEND=x11`; o root Xwayland retornou preto,
então o conteúdo visual não foi validado nessa sessão. O caminho de captura
X11 real é coberto pelo `screen_smoke`.

Com libyuv, o probe de release em frame sintético 1920×1080 BGRA mediu 19,70 ms
por quadro em 720p30, 14,18 ms em 720p60, 28,50 ms em 1080p30 e 22,90 ms em
1080p60 (prepare+preview + encode + decode). A captura full-screen X11 com o
caminho rápido mediu 7,00 ms/frame. O soak de 10 minutos de 720p60 com dois
clientes no mesmo processo está registrado na revisão de perfis abaixo;
720p30 ainda precisa de resultado final.

Limitações mantidas: captura X11 de tela inteira; no portal Wayland, seleção
de monitor/janela conforme o suporte do desktop; 1080p60 experimental, adaptação de bitrate e 8
pessoas com vídeo ainda pendentes; WGC não implementado e backend PipeWire
pendente de validação visual; sem AEC. libyuv, libvpx e libjpeg são dependências
de runtime no Linux; os instaladores do M4 as empacotam em `/usr/lib/concord` e
usam RUNPATH.

A captura Wayland usa uma sessão privada do portal ScreenCast e o remote
PipeWire autorizado por ele. O consentimento é preparado fora do ator de
áudio; a troca de qualidade reutiliza a captura e altera somente o encoder.
Revogação/saída encerram a sessão. Implementação e validação pendente em
[WAYLAND.md](../WAYLAND.md).

## Revisão de perfis de tela — outubro/2026

O ajuste de curto prazo definiu três perfis principais. A pedido do usuário,
1080p60 também está disponível no seletor, com indicação de experimental:

| Perfil | Bitrate-alvo | Uso |
| --- | ---: | --- |
| 720p30 | 2.500 kbps | padrão seguro e menor uso de rede |
| 720p60 | 3.500 kbps | fluidez para movimento |
| 1080p30 | 4.500 kbps | detalhe para texto |
| 1080p60 (exp.) | 6.000 kbps | detalhe e fluidez, dependentes do hardware |

`1080p60` (6.000 kbps) pode ser escolhido na UI ou por
`CONCORD_SCREEN_TIER=1080p60`. Continua experimental: VP8 por software não
sustenta 60 fps em 1080p nesta máquina com margem para decode, GTK e malha.

Também foi alterado o pipeline:

- captura e encode rodam em threads separadas, com fila limitada de dois frames
  e descarte do novo frame quando a fila está cheia;
- o preview local usa um slot de “último frame”, em vez de enfileirar BGRA;
- o caminho rápido de BGRA no X11 mediu 7,00 ms/frame em 1920×1080;
- trocar a qualidade substitui apenas o encoder, preservando a captura
  autorizada, a faixa WebRTC e a presença, sem reabrir o portal Wayland.

O probe de release em frame sintético 1920×1080 BGRA mediu 720p30 19,70 ms,
720p60 14,18 ms, 1080p30 28,50 ms e 1080p60 22,90 ms por quadro
(prepare+preview + encode + decode). O soak de 10 minutos de 720p60 concluiu
com 35.508 quadros remotos, 59,2 fps, CPU agregada de 174,6%, RSS de 255,2 MiB
e zero erros de decode. O de 1080p30 mediu 17.893 quadros, 29,8 fps, 78,4% de
CPU agregada, 296,0 MiB e zero erros; o transmissor registrou 18.002 quadros e
582 keyframes. O soak de 720p30 permanece pendente; o 1080p60 não foi
promovido. A validação desta revisão está em
[VALIDACAO_TELA.md](../VALIDACAO_TELA.md).

## Evidências do M2

| Evidência | Resultado |
| --- | --- |
| Data channel normal | Dois clientes WebSocket reais, ICE/DTLS/SCTP e hello bidirecional em ~0,86 s |
| Data channel forçado por TURN | coturn 4.18 local; ambos os pares selecionados com `local_type: "relay"`; hello bidirecional |
| Captura XShm | 1280×720 a 30,00 fps e 60,00 fps em release; ~105,5 MiB/s a 30 fps |
| Áudio CPAL | captura estéreo 48 kHz por 10 min: 57.605.120 amostras, 0 drops/underruns/erros; playback local de 5 s com 0 perdas |
| TURN de laboratório | `deploy/turn/compose.yaml`, restrito a loopback |
| Opus no M3 | `audio_signaling`: dois peers trocam RTP Opus e recebem PCM não silencioso |
| Voz completa no M3 | dois clientes, áudio bidirecional, mute/deafen, saída e teardown; relay forçado também passa |
| Vídeo no M4 | `video_signaling` e `bidirectional_video`: faixa VP8 adicionada/removida e dois peers compartilhando ao mesmo tempo |
| Tela completa no M4 | `screen_smoke`: dois clientes, captura X11 real, parada, compartilhamento simultâneo e zero erros de decode |
| Perfis de tela no M4 | `screen_smoke` troca o perfil pedido → perfil alternativo → perfil pedido sem derrubar a faixa; soaks de 10 min: 720p60 59,2 fps/174,6% CPU/255,2 MiB e 1080p30 29,8 fps/78,4% CPU/296,0 MiB, ambos com zero erros; 720p30 em execução |
| Janela nativa no M4 | viewer GTK abre, desenha um frame BGRA e fecha |

Comandos reproduzíveis:

```bash
cargo test -p concord-rtc --test data_channel_signaling
cargo test -p concord-rtc --test audio_signaling
cargo test -p concord-app --test voice_smoke -- --ignored --nocapture
cargo test -p concord-rtc --test video_signaling
cargo test -p concord-rtc --test bidirectional_video
cargo test -p concord-app --test screen_smoke -- --ignored --nocapture
docker compose -f deploy/turn/compose.yaml up -d
cargo test -p concord-rtc --test data_channel_signaling \
  forced_turn -- --ignored --nocapture
cargo run -p concord-media --features x11 --example media-probe -- \
  screen-probe --seconds 10 --fps 30
```

## Alternativas consideradas

| Alternativa | Motivo da rejeição |
| --- | --- |
| WebRTC do WebView/Chromium | Viola a premissa de mídia nativa e aumenta o pacote |
| libwebrtc C++ | Build/pacote pesados, difícil de auditar e integrar no Tauri |
| GStreamer para toda a mídia | Dependência grande e fragmentada; o controle de ICE/SDP ficaria menos direto |
| Implementação própria sobre QUIC | Custo e risco incompatíveis com o MVP; sem interoperabilidade WebRTC |
| H.264 como único codec inicial | Encoders e licenciamento variam por SO; atrasaria a primeira tela funcional |
| VP9 como codec inicial | Melhor compressão, mas CPU de encode alta em malha e menor suporte pronto |

## Pendências

- ICE restart e renegociação de faixa de áudio; a faixa de vídeo já renegocia.
  O desenho está em [`../CALL_LIFECYCLE.md`](../CALL_LIFECYCLE.md).
- WGC no Windows; validação visual do portal/PipeWire no Wayland.
- 1080p60 promovido, adaptação de bitrate e 8 pessoas com vídeo.
- Validar a instalação real dos pacotes em Ubuntu 24.04 e Fedora; os binários
  do `.deb`/`.rpm` foram carregados nesses containers e os bundles executados a
  partir de extração local, mas não instalados via `dpkg`/`rpm`. O empacotamento
  usa os SONAMEs do host de build; outra distribuição exige ajustar o mapa de
  arquivos ou usar um container compatível.
- AEC/NS, hot-swap de devices e validação com alto-falantes.
- Credenciais TURN temporárias HMAC; o laboratório usa usuário estático.
- Medições de WAN, NAT real e chamadas com 8 pessoas.

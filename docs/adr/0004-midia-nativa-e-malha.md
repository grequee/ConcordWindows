# ADR 0004 — Mídia nativa, WebRTC em malha e viewer separado

- **Status:** aceita (fundação do M2 em andamento; ver ADR 0006)
- **Data:** 2026-10-25
- **Marco:** M2–M4
- **Substitui:** —

## Contexto

O app precisa de voz e compartilhamento de tela em grupos de 2–8 pessoas. O WebView não deve capturar mídia nem renderizar frames de vídeo, e o servidor não deve encaminhar áudio/vídeo no MVP. O objetivo é evitar Chromium, reduzir latência e manter a arquitetura controlável.

Também é preciso escolher entre malha P2P e SFU, codec de vídeo e onde renderizar a tela compartilhada.

## Decisão

- Toda mídia é capturada, codificada, transportada e reproduzida no processo Rust.
- WebRTC com DTLS-SRTP para transporte e ICE/STUN/TURN para conectividade.
- Topologia **malha completa** para o MVP: uma conexão por par.
- O servidor de sinalização faz apenas relay opaco de SDP/ICE; não vê mídia.
- Vídeo decodificado é exibido em uma **janela Tauri separada** com superfície nativa ou textura, não via IPC de frames.
- O WebView nunca recebe `MediaStream`, frames crus ou chaves de mídia.
- Backends de captura:
  - Windows: Windows Graphics Capture.
  - Linux X11: captura X11.
  - Linux Wayland: xdg-desktop-portal + PipeWire.
- Áudio: CPAL + Opus 48 kHz, frames de 20 ms.
- O perfil de vídeo inicial e o codec foram definidos no [ADR 0006](0006-webrtc-e-codec.md); os contratos preveem 720p30 e 1080p60 com orçamento adaptativo.

## Consequências

**Boas**

- Latência mínima e independência de APIs de captura do browser.
- Nenhum frame de tela precisa ser serializado para JavaScript.
- Servidor leve, sem misturar, transcodificar ou armazenar mídia.
- Funciona sem expor um SFU ou infraestrutura de mídia no M1.

**Custos**

- Malha cresce O(n²) em conexões e O(n) em upload por participante. Com 8 pessoas, cada uplink carrega 7 fluxos.
- 1080p60 em malha exige uplink e CPU consideráveis; será necessário adaptar bitrate e talvez limitar perfil.
- Implementar captura em três backends é custoso e exige testes por SO.
- Renderização nativa fora do WebView implica uma segunda janela e ciclo de vida próprio.
- TURN precisa de infraestrutura e segredo; sem ele, algumas redes não conectam.
- WebRTC em Rust tem custo de build, maturidade variável e superfície de segurança a acompanhar.

## Limites explícitos

- Sem SFU no MVP.
- Sem webcam no MVP.
- Sem gravação.
- Sem E2EE aplicacional além de DTLS-SRTP.
- Sem promessa de 1080p60 em qualquer rede; o perfil se adapta ao orçamento.

## Alternativas consideradas

- **`getDisplayMedia` no WebView:** rejeitado por requisito explícito e por variabilidade/permissões.
- **SFU (LiveKit/mediasoup):** melhor para 8+ e controle central, mas adiciona infraestrutura, latência e complexidade incompatíveis com o MVP.
- **Captura + encode no Rust, frames via IPC para canvas:** rejeitado; serializar vídeo no IPC consome CPU e memória e expõe frames ao WebView.
- **Janela nativa independente do Tauri (winit/egui):** viável, mas o Tauri já oferece multi-window e integração de app; reavaliar se a renderização nativa exigir controle que o Tauri não ofereça.
- **Codec proprietário via hardware:** depende de SO/driver e complica Linux; decisão adiada para o spike.

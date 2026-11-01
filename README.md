# Concord

Comunicação privada para grupos pequenos: texto em tempo real, presença, voz em malha e compartilhamento de tela nativo. Cliente desktop nativo em **Rust + Tauri 2**, interface leve em **Svelte 5**, servidor de sinalização próprio.

> **Estado atual: marcos 1–4 entregues no Linux/X11, com perfis de tela
> 720p30, 720p60 e 1080p30; 1080p60 experimental, escala e expansão de
> plataforma pendentes.** Texto, presença, voz Opus em malha e compartilhamento
> de tela com VP8 estão funcionais no app. A arquitetura cobre 2–8 pessoas; a
> validação automatizada de ponta a ponta usa dois clientes. O suporte Windows
> (WGC/Win32) está implementado, com build e validação no Windows pendentes;
> veja [WINDOWS.md](docs/WINDOWS.md). Validação do Wayland/PipeWire,
> 1080p60 promovido e escala de 8 pessoas continuam pendentes.

## Por que Concord

- **Sem Electron.** O WebView desenha a interface; rede, mídia e WebRTC vivem no processo Rust.
- **Sem `getDisplayMedia`.** Captura de tela e áudio são nativas, com backend por sistema operacional.
- **Acesso fechado.** Não existe cadastro aberto: cada pessoa entra com um token de convite combinado previamente.
- **Servidor da turma.** Sinalização auto-hospedada, com TLS terminado em um proxy reverso de confiança.
- **Leve por orçamento.** Meta de instalador abaixo de 30 MB, RAM ociosa abaixo de 80 MB e partida abaixo de 1,5 s.

## Como está hoje

| Área | Estado atual |
| --- | --- |
| Servidor de sinalização | Axum + WebSocket, TLS delegado ao proxy, `/healthz`, limite de quadros, rate limit e bloqueio de tentativas de convite |
| Autenticação | Token de convite com comparação em tempo constante; sessão efêmera e identidade emitida pelo servidor |
| Salas | Lista, entrada, saída, capacidade 2–8, presença ordenada por nome |
| Texto e anexos | Histórico no Neon quando configurado, arquivos de qualquer tipo em bucket privado, envio/download em blocos sem limite fixo por arquivo, paginação e retenção automática; modo em memória sem banco |
| Presença | Mudo, sem áudio, “compartilhando tela” e “na chamada” como flags sincronizadas |
| WebRTC nativo | `webrtc` 0.21.1; uma `RTCPeerConnection` por par, trickle ICE, data channel, faixa de áudio Opus e faixa de vídeo VP8 com renegociação; TURN forçado validado |
| Voz nativa | CPAL + Opus 48 kHz mono, quadros de 20 ms a 32 kbps; jitter buffer de 4 quadros, PLC, mixagem com limite, mute/deafen reais e contadores por peer |
| UI de voz | Seletores de microfone/saída, troca na chamada, teste de voz, indicador de fala, volume individual e controles de mudo/áudio |
| Compartilhamento de tela | Linux/X11 por MIT-SHM e Wayland por portal ScreenCast + PipeWire; seleção de monitor/janela pelo portal quando suportada pelo desktop; perfis 720p30/720p60/1080p30, VP8 e janelas GTK nativas; validação visual do Wayland pendente; 1080p60 experimental; Windows/WGC + viewer Win32 implementados, build/validação Windows pendentes |
| Cliente desktop | Tauri 2 + Svelte 5, WebSocket nativo em Rust, espera automática do servidor, retry e recuperação da sala; lembra nome, servidor e convite |
| Persistência | Ajustes, nome e servidor salvos no cliente; chat no Neon via configuração do servidor; salas e sessões em memória. Veja [PERSISTENCIA.md](docs/PERSISTENCIA.md) |

## Fluxo de dados

```
┌───────────────┐      IPC (Tauri)       ┌───────────────────────────┐
│ UI Svelte 5   │ ◄────────────────────► │ Processo Rust (Tauri)     │
│ (WebView)     │   snapshot / comandos  │  DesktopState             │
└───────────────┘                        │    ├─ cliente WebSocket   │
                                         │    ├─ CallActor           │
                                         │    │   ├─ CPAL/Opus      │
                                         │    │   ├─ peers WebRTC   │
                                         │    │   └─ VP8 tela       │
                                         │    └─ captura X11/GTK    │
                                         └────────────┬──────────────┘
                                                      │ WSS
                                         ┌────────────▼──────────────┐
                                         │ Caddy / nginx             │
                                         │ TLS + proxy reverso       │
                                         └────────────┬──────────────┘
                                                      │ WS
                                         ┌────────────▼──────────────┐
                                         │ concord-signaling-server  │
                                         │ salas · chat · presença   │
                                         │ relay SDP/ICE             │
                                         └───────────────────────────┘

          áudio Opus + vídeo VP8  ◄── DTLS-SRTP direto entre os pares ──►
```

O áudio e o vídeo **não passam pelo WebView** e não passam pelo servidor de
sinalização. O servidor apenas troca SDP/ICE; cada participante mantém uma
conexão WebRTC com cada outro participante da chamada e envia seus fluxos
diretamente a todos eles. Os frames de tela são capturados, codificados e
enviados no processo Rust; a janela de preview/visualização é nativa (GTK no Linux, Win32 no Windows) e a UI
Svelte só recebe estado e contadores. Em uma malha de 8 pessoas, cada cliente
envia 7 fluxos — o que limita uplinks domésticos e é uma razão para o teto de 8.

## Estrutura do workspace

```
concord/
├── crates/
│   ├── protocol/           # frames versionados, presença in_call e validação
│   ├── core/               # domínio síncrono: salas, presença, histórico
│   ├── media/              # X11/Wayland/WGC, CPAL, Opus, VP8 e jitter buffer
│   ├── rtc/                # WebRTC nativo: ICE/TURN, data channel, Opus e VP8
│   └── signaling-server/   # servidor Axum/WS + binário + testes e2e
├── src-tauri/              # app desktop: cliente nativo, CallActor, IPC, config Tauri
├── ui/                     # Svelte 5 + Vite
├── deploy/                 # exemplos de configuração, Caddy, systemd, coturn
├── docs/                   # plano, roadmap, ciclo da chamada e ADRs
├── Cargo.toml              # workspace Rust
└── package.json            # scripts do frontend e Tauri CLI
```

## Build Windows

O backend Windows Graphics Capture e o visualizador Win32 estão implementados.
Em um PC Windows com os pré-requisitos instalados, gere o instalador com:

```powershell
npm run build:windows
```

Também há um workflow **Windows desktop** para gerar o `.exe` pelo GitHub
Actions a partir deste workspace completo. O build MSVC e o teste em um PC
Windows ainda precisam ser executados. Veja [instruções Windows](docs/WINDOWS.md).

## Pré-requisitos

- Rust stable **1.99+** (o projeto usa edition 2024).
- Node.js **24+** e npm.
- Linux: WebKitGTK 4.1 e dependências de build do Tauri 2.
- Windows x64: Visual Studio Build Tools com C++/Windows SDK, LLVM e CMake;
  o instalador verifica WebView2. Consulte [build Windows](docs/WINDOWS.md).

Exemplo em Debian/Ubuntu:

```bash
sudo apt install build-essential curl wget file libssl-dev pkg-config libasound2-dev \
  libvpx-dev libyuv-dev libjpeg-dev libpipewire-0.3-dev libglib2.0-dev libspeexdsp-dev \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev
```

Em Arch, os equivalentes são `base-devel`, `alsa-lib`, `libvpx`, `libyuv`, `libjpeg-turbo`, `webkit2gtk-4.1`, `libayatana-appindicator`, `librsvg` e `openssl`. O binário usa libvpx e, quando disponível, libyuv do sistema em tempo de execução; sem libyuv o build usa o escalador Rust mais lento. Os instaladores Linux são gerados com libvpx, libyuv e libjpeg empacotados; o WebKitGTK/GTK continuam vindo do sistema. Para gerar os instaladores, instale as três bibliotecas de desenvolvimento. Consulte também a documentação oficial do Tauri 2 para a sua distribuição.

O backend Wayland também exige `libpipewire` e `glib2` no build em Arch. Em
runtime, use PipeWire, `xdg-desktop-portal` e o backend do seu desktop (por
exemplo, `xdg-desktop-portal-kde` no Plasma). Veja [captura no Wayland](docs/WAYLAND.md).

## Usar pela internet com Render

Cada pessoa instala o app no próprio PC e usa a mesma URL pública e convite.
O servidor já pode ser publicado com o [render.yaml](render.yaml) e o
[Dockerfile de sinalização](deploy/render/Dockerfile). Ele atende em
`0.0.0.0:$PORT`, tem health check e recebe os segredos pelo ambiente do Render.

Siga [o guia de primeiro deploy](deploy/render/README.md) para criar o serviço,
copiar o convite gerado e preencher a URL real no app. O cliente recebe
STUN/TURN do servidor após autenticar; não é preciso configurar o terminal
de cada usuário. O primeiro deploy usará somente STUN e conexão direta,
sem TURN, conforme decisão do usuário. Redes em que a mídia exige relay
podem ficar sem voz/tela. Deploy real e validação WAN ainda estão pendentes.

## Início rápido local

### 1. Configurar o servidor

```bash
cp deploy/concord.example.toml concord.toml
# Gere um token real e cole em [[invites]].token
openssl rand -hex 32
```

Edite `concord.toml` com seus tokens e salas. O arquivo é ignorado pelo Git e deve ter permissão `0600` em produção.

Para validar sem abrir porta:

```bash
cargo run -p concord-signaling-server -- --config concord.toml --check
```

### 2. Subir a sinalização

```bash
cargo run -p concord-signaling-server -- --config concord.toml
curl http://127.0.0.1:8787/healthz
```

Resposta esperada:

```json
{"status":"ok","server":"Concord","version":"0.1.0","protocol_version":1,"rooms":2,"participants":0,"connected_clients":0}
```

### 3. Rodar o cliente

Em outro terminal:

```bash
npm install
npm run tauri dev
```

Na tela inicial informe:

- Servidor: `http://127.0.0.1:8787` (o cliente converte para `ws://`)
- Nome: seu nome de exibição (1–32 caracteres)
- Token: o mesmo token de `concord.toml`

Para testar duas pessoas na mesma máquina, abra uma segunda janela do app. Alternativamente, rode o servidor na sua LAN e use `http://IP-DA-MAQUINA:8787` nos clientes.

O app lembra nome, servidor e convite. Se a resposta demorar, mostra **Aguardando o
servidor iniciar…** e tenta novamente por até três minutos, com opção de
cancelar. Depois de uma queda de rede, recupera a conexão e tenta voltar à
sala automaticamente. Nome, servidor e convite ficam salvos neste computador;
voz e tela podem ser retomadas pelos controles da chamada. Detalhes e testes
em [CONEXAO.md](docs/CONEXAO.md).

### 4. Conversar por voz

1. As duas pessoas entram na mesma sala de texto.
2. Clique em **Entrar na voz** na dock acima do chat.
3. O app abre o microfone e a saída escolhidos em Áudio, ou os padrões do sistema. O botão mostra `abrindo áudio…` enquanto isso.
4. **mic** silencia a captura de verdade; **áudio** desliga a reprodução dos pares. Os dois estados aparecem para as outras pessoas e no snapshot da chamada.
5. **sair** encerra a chamada sem sair da sala. Sair da sala ou desconectar também encerra a chamada.

Use **Áudio** no rodapé para escolher microfone e fone, aplicar a troca na
chamada e testar a própria voz antes de entrar nela. O painel também oferece
ganho de 0% a 200% e supressão de ruído. A lista de participantes destaca quem
está falando e permite ajustar o volume de cada pessoa só para você. Veja
[MICROFONE.md](docs/MICROFONE.md).

Use **fones de ouvido** durante os testes em uma mesma máquina ou em uma sala
com caixas acústicas. O app ainda não tem cancelamento de eco (AEC) nem ganho
automático; a supressão de ruído não elimina o retorno do alto-falante.

### 5. Compartilhar tela

No Linux/X11, Wayland ou Windows, com a chamada de voz ativa:

1. Escolha a qualidade no seletor **qualidade** da dock: `720p30` (padrão e
   menor uso de rede), `720p60` (mais fluidez), `1080p30` (mais detalhe para
   texto) ou `1080p60 (exp.)` (detalhe e fluidez, experimental).
2. Clique em **Compartilhar tela** na dock da chamada ou no controle do seu
   participante.
3. No Wayland, escolha um monitor ou janela no seletor do sistema e autorize
   a captura. Em X11, a captura usa a tela inteira; no Windows, o monitor
   principal inteiro. O app abre um preview nativo (GTK/Win32) e anuncia
   a presença `screen_sharing`. Os frames são codificados em VP8 no processo
   Rust e enviados direto aos pares.
4. A outra pessoa recebe a faixa por renegociação WebRTC e vê a tela em outra
   janela nativa. Fechar essa janela não interrompe a transmissão para os demais.
5. Trocar a qualidade durante a transmissão substitui apenas o encoder;
   a captura autorizada, a faixa WebRTC e a presença continuam ativas, sem
   reabrir o seletor do portal.
6. Clique de novo para parar. O preview, a thread de captura e as janelas
   remotas são encerrados, e a presença volta a `false`.

Duas pessoas podem compartilhar ao mesmo tempo; cada uma vê a tela da outra.
Em sessões Wayland, o backend é escolhido automaticamente, mesmo com
`DISPLAY` do Xwayland definido. Cancelar o seletor não inicia uma transmissão;
é possível silenciar o microfone ou sair da chamada durante a seleção. A
validação visual do Wayland e o build/validação do Windows continuam pendentes.
Instruções e limitações em [WAYLAND.md](docs/WAYLAND.md) e
[WINDOWS.md](docs/WINDOWS.md).
`1080p60 (exp.)` está disponível no seletor e usa o perfil de 6.000 kbps.
Continua experimental: atingir 60 fps depende do hardware e do conteúdo.
`CONCORD_SCREEN_TIER=1080p60` também permite iniciar a chamada nesse perfil.

### 6. Configurar ICE e dispositivos (opcional)

O padrão funciona em LAN/loopback sem STUN. No Render, prefira configurar
STUN/TURN no servidor, como descrito no [guia de deploy](deploy/render/README.md);
os apps atualizados recebem esses parâmetros automaticamente. As variáveis
abaixo continuam disponíveis para laboratório ou configuração manual do cliente:

| Variável | Efeito |
| --- | --- |
| `CONCORD_STUN_URL` | Adiciona um servidor STUN, por exemplo `stun:stun.l.google.com:19302` |
| `CONCORD_TURN_URL` | Adiciona um TURN, por exemplo `turn:turn.exemplo.com:3478?transport=udp` |
| `CONCORD_TURN_USERNAME` | Usuário do TURN; obrigatório junto com URL e credencial |
| `CONCORD_TURN_CREDENTIAL` | Credencial do TURN |
| `CONCORD_FORCE_RELAY` | `1`, `true` ou `yes` restringe ICE a relay; exige um TURN configurado |
| `CONCORD_AUDIO_INPUT` | ID estável do microfone, como listado por `audio-devices` |
| `CONCORD_AUDIO_OUTPUT` | ID estável da saída de áudio |
| `CONCORD_SCREEN_TIER` | Perfil inicial de tela: `720p30` (padrão), `720p60`, `1080p30` ou `1080p60` experimental; valores inválidos caem em `720p30` |

Exemplo com o coturn de laboratório (as duas instâncias do app precisam das mesmas variáveis):

```bash
docker compose -f deploy/turn/compose.yaml up -d
CONCORD_TURN_URL='turn:127.0.0.1:3478?transport=udp' \
CONCORD_TURN_USERNAME=concord \
CONCORD_TURN_CREDENTIAL=concord-test \
CONCORD_FORCE_RELAY=1 \
npm run tauri dev
```

Para descobrir os IDs de dispositivo:

```bash
cargo run -p concord-media --example media-probe -- audio-devices
```

O TURN acima é apenas um laboratório de loopback. Para WAN, use `deploy/coturn.conf` como ponto de partida, credenciais próprias e TLS (`turns:`); veja [`deploy/turn/README.md`](deploy/turn/README.md).

## Provas do Marco 4 — tela

O caminho de tela também foi validado sem o WebView participar da mídia.

### Sinalização e VP8 bidirecional

```bash
cargo test -p concord-rtc --test video_signaling
cargo test -p concord-rtc --test bidirectional_video
```

`video_signaling` cobre adicionar, transmitir e remover uma faixa VP8 pela
sinalização. `bidirectional_video` cobre o cenário de glare: um lado compartilha,
para, e os dois começam a compartilhar ao mesmo tempo; as duas direções enviam
keyframes e decodificam VP8. Esse teste motivou a faixa `sendrecv` ociosa no
offerer e o SSRC novo a cada nova faixa.

### Dois clientes completos com captura X11 real

```bash
cargo test -p concord-app --test screen_smoke -- --ignored --nocapture
```

Usa dois `DesktopState`, servidor efêmero, áudio real e captura X11 real. Cobre
compartilhamento único, parada, compartilhamento simultâneo, nova parada,
presença e zero erros de decodificação. Também verifica a troca de perfil em
tempo real (`720p30 → 720p60 → 720p30` por padrão; com
`CONCORD_SCREEN_TIER=720p60`, `720p60 → 1080p30 → 720p60`) e que os frames continuam
chegando enquanto a presença está ativa e param depois do encerramento. Use
`CONCORD_SCREEN_TIER` para escolher o perfil do soak.

### Perfis e desempenho

O seletor expõe `720p30`, `720p60`, `1080p30` e `1080p60 (exp.)`. A variável
`CONCORD_SCREEN_TIER` continua definindo o perfil inicial, com aviso de
experimental no log para 1080p60. Os bitrates-alvo são 2.500, 3.500, 4.500 e 6.000 kbps,
respectivamente.

O caminho de escala/conversão usa libyuv quando disponível, com fallback para o
escalador bilinear em Rust. O probe de release, partindo de um frame sintético
1920×1080 BGRA, mediu:

| Perfil | prepare+preview | encode VP8 | decode | total por quadro |
| --- | ---: | ---: | ---: | ---: |
| 720p30 | 3,18 ms | 13,45 ms | 3,07 ms | 19,70 ms |
| 720p60 | 3,16 ms | 8,51 ms | 2,51 ms | 14,18 ms |
| 1080p30 | 2,81 ms | 18,37 ms | 7,32 ms | 28,50 ms |
| 1080p60 | 2,69 ms | 14,97 ms | 5,24 ms | 22,90 ms |

A captura full-screen X11 com o caminho rápido de BGRA mediu 7,00 ms/frame em
1920×1080. Captura e encode rodam em threads separadas, com uma fila de dois
frames que descarta o novo frame quando está cheia; o preview local usa um slot de “último
frame” para não enfileirar BGRA.

Soaks curtos de 30 s com dois clientes no mesmo processo, captura X11 real:

| Perfil | Quadros remotos | FPS médio | CPU agregada | RSS | Erros de decode |
| --- | ---: | ---: | ---: | ---: | ---: |
| 720p30 | 875 | 29,2 | 103,8% | 192,1 MiB | 0 |
| 720p60 | 1746 | 58,2 | 127,2% | 243,2 MiB | 0 |
| 1080p30 | 879 | 29,3 | 158,6% | 299,8 MiB | 0 |

O FPS médio inclui cerca de 1 s de warm-up do pipeline. No 720p60, captura e
encode rodaram a 60 fps sem drops e o segundo trecho de 10 s mediu 60,3 fps
remotos. O soak de 10 minutos de 720p60 com o pipeline novo mediu 35.508
quadros remotos (59,2 fps), 174,6% de CPU agregada, RSS estável em 255,2 MiB e
zero erros de decode. O soak de 1080p30 mediu 17.893 quadros (29,8 fps), 78,4%
de CPU agregada, RSS estável em 296,0 MiB e zero erros de decode; o transmissor
registrou 18.002 quadros e 582 keyframes. O soak de 720p30 será registrado após
a execução.

```bash
CONCORD_SCREEN_TIER=720p60 CONCORD_SCREEN_SOAK_SECS=600 \
  cargo test --release -p concord-app --test screen_smoke -- --ignored --nocapture
```

### Viabilidade de 1080p60

O perfil `PROFILE_1080P60` está disponível como **1080p60 (exp.)** na UI a
pedido do usuário. Permanece experimental, sem garantia de 60 fps sustentados.
Também pode ser escolhido por `CONCORD_SCREEN_TIER=1080p60`. As medições de
outubro/2026 nesta máquina (i7-1355U, X11/Xwayland, frame 1920×1080) foram:

- captura X11 full-screen com o caminho rápido: 7,00 ms/frame;
- probe sintético de 1080p60: prepare+preview 2,69 ms, encode 14,97 ms e decode
  5,24 ms (22,90 ms por quadro);
- 1080p30: probe de 28,50 ms por quadro e soak de 30 s a 29,3 fps;
- 720p60: probe de 14,18 ms por quadro e 60 fps de captura/encode, com 60,3 fps
  remotos no segundo trecho do soak.

O encode de 1080p60 ainda não tem margem medida para 60 fps sustentados com a
janela GTK, o decode e a malha no mesmo processo. Em malha, 6 Mbps por
espectador significam 42 Mbps de upload com 8 pessoas. Conclusão: **1080p60
continua experimental, mesmo disponível no seletor**. Para promovê-lo a estável serão
necessários hardware encode (VAAPI H.264/VP9; esta GPU não expõe encode VP8),
uma CPU mais rápida ou um perfil intermediário como 1080p50.

### Validação manual no app

Duas instâncias do app (Ana e Bia) entraram na mesma sala e chamada no KDE
Plasma Wayland, com o app forçado a X11 (`GDK_BACKEND=x11`):

- Ana iniciou a tela: abriram o preview local `Concord — Sua tela` e o viewer
  remoto `Concord — Tela de Ana` no processo de Bia; Bia mostrou “compartilhando
  a tela” e a dock passou a exibir a transmissão. O log de Ana registrou 242
  frames e 9 keyframes antes da parada.
- Ao parar, preview e viewer remoto fecharam, a thread de captura terminou e a
  presença voltou a `false`.
- Bia iniciou a tela em seguida: o viewer `Concord — Tela de Bia` abriu no
  processo de Ana.
- Com os dois compartilhando ao mesmo tempo, havia quatro janelas nativas
  (preview + viewer de cada lado); ao parar os dois, todas fecharam.

Nesta sessão Wayland, a captura do root Xwayland retornou preto, então os pixels
não puderam ser conferidos visualmente; o caminho de captura X11 real é o mesmo
exercitado pelo `screen_smoke`. Em uma sessão X11 nativa, o preview deve mostrar
a tela do desktop. O seletor e a troca de perfil em tempo real são exercitados
pelo `screen_smoke`; a conferência visual do seletor no app instalado continua
manual.

### Janela nativa e codec

```bash
cargo test -p concord-app native_viewer_opens_draws_and_closes -- --ignored --nocapture
cargo test -p concord-media --features x11
```

O teste GTK abre, desenha um frame BGRA e fecha a janela. Os testes de mídia
cobrem conversão BGRA/I420, escala bilinear e round-trip VP8. Nenhum frame é
serializado no IPC do Tauri.

### Limites desta validação

- Os soaks de 10 minutos de 720p60 e 1080p30 estão na seção
  [Perfis e desempenho](#perfis-e-desempenho); o de 720p30 será medido na
  sequência, e o 1080p60 não foi promovido.
- Nesta sessão Wayland a captura do root X11 retorna preto, então CPU e bitrate
  dos soaks não representam conteúdo real. FPS, estabilidade e ausência de erros
  de decode continuam válidos para o pipeline.
- O teste automatizado usa dois clientes e áudio real; 8 pessoas com vídeo,
  1080p60 e adaptação de bitrate ainda não foram medidos.
- Windows/WGC e Wayland/PipeWire não foram validados.
- A janela do espectador é GTK separada; a validação visual em duas janelas do
  app instalado continua manual.

## Provas do Marco 3 — voz

A voz foi validada no caminho Rust completo, sem depender do WebView para mídia.

### Opus real sobre WebRTC

```bash
cargo test -p concord-rtc --test audio_signaling
```

Dois peers negociam a faixa Opus, enviam 440 Hz e 660 Hz codificados de verdade pelo RTP/WebRTC e decodificam PCM não silencioso. O teste normal termina em cerca de 1 s.

### Dois clientes completos com áudio real

```bash
cargo test -p concord-app --test voice_smoke -- --ignored --nocapture
```

Esse teste usa dois `DesktopState` reais e um `AppHandle` mock do Tauri, um servidor de sinalização efêmero no próprio processo e os devices reais de microfone/alto-falante. Ele cobre:

- autenticação e entrada dos dois clientes na mesma sala;
- `join_call`, presença `in_call`, oferta/resposta e trickle ICE;
- áudio Opus bidirecional (contadores `tx_frames` e `rx_frames` crescendo);
- mute bloqueando o envio de quadros e unmute retomando;
- deafen mantendo o consumo/decodificação do áudio remoto;
- `leave_call`, saída da sala com chamada ativa e `disconnect` com chamada ativa;
- ausência de underruns de playback e de erros de stream na execução de referência.

Saída resumida de uma execução recente:

```text
both voice peers connected
bidirectional audio: ana={"playback_underruns":0,"stream_errors":0,"tx_frames":27,"rx_frames":27,...}
bidirectional audio: bia={"playback_underruns":0,"stream_errors":0,"tx_frames":28,"rx_frames":28,...}
leave propagated; bia call={"peers":[],...}
test ... ok
```

O teste é `ignored` porque exige um microfone e uma saída de áudio reais. A UI é apresentação; o teste do WebView em duas janelas continua sendo a validação manual de instalação.

### Relay forçado com coturn

Com o laboratório local ativo, o mesmo teste de voz pode ser forçado a usar somente TURN:

```bash
docker compose -f deploy/turn/compose.yaml up -d
CONCORD_TURN_URL='turn:127.0.0.1:3478?transport=udp' \
CONCORD_TURN_USERNAME=concord \
CONCORD_TURN_CREDENTIAL=concord-test \
CONCORD_FORCE_RELAY=1 \
cargo test -p concord-app --test voice_smoke -- --ignored --nocapture
docker compose -f deploy/turn/compose.yaml down
```

Passou em 9,62 s com áudio bidirecional, `playback_underruns: 0` e `stream_errors: 0`. Como `force_relay` remove candidatos host/srflx, o sucesso prova que o caminho escolhido foi o coturn local; ele não substitui um teste WAN com TURN de produção.

### Device real isolado

```bash
cargo test -p concord-media --lib \
  real_devices_capture_and_play_for_one_second -- --ignored --nocapture
```

Abre captura e playback CPAL simultâneos, lê 1 s de microfone e reproduz um tom de 440 Hz. A execução de referência usou entrada/saída `(48000, 2)` e terminou com `stream_errors: 0`.

## Provas do Marco 2

A fundação de mídia continua verificável sem abrir a UI.

### WebRTC e TURN

```bash
# Dois clientes WebSocket reais, ICE/DTLS/SCTP e hello bidirecional.
cargo test -p concord-rtc --test data_channel_signaling

# Relay forçado: sobe o coturn local e executa o teste ignorado.
docker compose -f deploy/turn/compose.yaml up -d
cargo test -p concord-rtc --test data_channel_signaling \
  forced_turn -- --ignored --nocapture
docker compose -f deploy/turn/compose.yaml down
```

O teste normal completa em cerca de 0,86 s. O teste forçado mostra
`RelayCandidate` para os dois lados e termina com pares selecionados
`local_type: "relay"`, provando que o caminho escolhido foi o TURN. O laboratório
escuta apenas em `127.0.0.1` e não serve para produção; detalhes em
[`deploy/turn/README.md`](deploy/turn/README.md).

### Áudio (CPAL)

```bash
cargo run -p concord-media --example media-probe -- audio-devices
cargo run -p concord-media --example media-probe -- audio-loopback --seconds 10
```

`audio-loopback` usa o microfone e o device de saída padrão. Use
`--no-playback` para evitar realimentação. A captura de 2 s medida ficou em
48 kHz estéreo, 191.488 amostras, sem drops, underruns ou erros. A captura
contínua de 10 minutos produziu 57.605.120 amostras sem perdas; o playback local
de 5 s não teve underruns.

### Tela (X11/Xwayland)

```bash
cargo run -p concord-media --features x11 --example media-probe -- screen-sources
cargo run -p concord-media --features x11 --release \
  --example media-probe -- screen-probe --seconds 10 --fps 30
```

Em release, 1280×720 a 30 fps mediu 300 frames em 10,000 s (média 33,26 ms,
máximo 42,15 ms, 105,5 MiB/s). A 60 fps: 600 frames em 10,000 s, 210,9 MiB/s.
O backend usa MIT-SHM/XShm e não envia frames pelo WebView. Em sessão Wayland
nativa, o root do Xwayland pode não mostrar janelas Wayland; o backend
portal/PipeWire foi adicionado, mas sua validação visual ainda está pendente.

### Limites conhecidos

- O backend Windows/WGC está implementado; build e validação em Windows
  permanecem pendentes, assim como a validação visual do Wayland/PipeWire.
- A captura usa a tela inteira em X11 e o monitor principal no Windows. O
  portal Wayland permite selecionar monitor/janela; não há seletor Windows.
- O teste de TURN é loopback; WAN/NAT real e credenciais temporárias continuam
  pendentes.
- Benchmarks de tela: o probe de captura full-screen mede 7,00 ms/frame em
  1920×1080; os soaks de 10 min com dois clientes estão na seção
  [Perfis e desempenho](#perfis-e-desempenho). Áudio ficou em ~1,2% de CPU e
  ~10,4 MB RSS em captura e ~3,0% em loopback com playback.

## Build de produção

O build empacota o frontend e o binário nativo. O binário **não** embute WebView: no Linux, o `.deb` usa o WebKitGTK do sistema; no Windows, o app usa o WebView2 Runtime do sistema.

```bash
npm run tauri build
```

Artefatos:

- Linux: `target/release/bundle/deb/`, `target/release/bundle/rpm/` e `target/release/bundle/appimage/`
- Windows pelo script: `target/x86_64-pc-windows-msvc/release/bundle/nsis/`

Tamanhos medidos no release anterior do M4 (Linux x86_64), gerado em
27/10/2026. Estes artefatos ainda não contêm os perfis novos nem o seletor;
o rebuild e a atualização dos hashes dependem da conclusão da validação.

| Artefato | Tamanho | SHA-256 | Observação |
| --- | ---: | --- | --- |
| Binário no `.deb` | 10,1 MiB | `ceb0ad8b90b1a012cfb07cecfce3af7099689fbcca99dbccf8449799b07a6e20` | `strip`, LTO thin e `opt-level = "s"`; com Opus, VP8, X11 e GTK |
| `.deb` | 6,27 MiB | `1c67430bba1f49e4dec33a8f0c787da94192aa8fcb9036230698c336d59974b2` | Inclui libvpx/libyuv/libjpeg em `/usr/lib/concord`; WebKitGTK/GTK vêm do sistema |
| `.rpm` | 6,27 MiB | `9a1979d40d1830218eb1935686077b446802d691fac2c82b18789b4d7cf58a0f` | Mesma observação do `.deb` |
| AppImage | 111,00 MiB | `ea873d259f4dfea3d4994aa59b7ccde5efc3e577ca14a50f83aa4bb040309eeb` | Empacota WebKitGTK, JavaScriptCore, ICU e as libs de mídia |

O estado dos testes desta revisão e os bloqueios do ambiente estão em
[`docs/VALIDACAO_TELA.md`](docs/VALIDACAO_TELA.md).

O alvo de instalador abaixo de 30 MB é atendido pelo `.deb`/`.rpm` (6,27 MiB), que empacotam as libs de mídia e usam o WebKitGTK do sistema. O AppImage portátil fica acima do alvo porque carrega o próprio WebView. O Tauri permite compilação cruzada para Windows no Linux com cargo-xwin, SDK e codecs Windows; essas dependências não estão disponíveis nesta sessão. O fluxo automatizado do Concord usa Windows/MSVC ou GitHub Actions. Veja [WINDOWS.md](docs/WINDOWS.md).

### Linux

Instale as mesmas dependências de build acima. Os bundles Linux executam `scripts/stage-linux-libs.sh`, que copia libvpx, libyuv e libjpeg para `/usr/lib/concord` e para o AppImage; o binário recebe `RUNPATH=$ORIGIN/../lib/concord:$ORIGIN/../lib`. Para AppImage, o Tauri precisa de ferramentas/FUSE no ambiente de empacotamento; o `.deb`/`.rpm` é mais previsível para distribuição interna. Os binários do `.deb` e do `.rpm` foram carregados em containers Ubuntu 24.04 e Fedora 40 com WebKitGTK/GTK instalados, sem bibliotecas faltantes; a instalação via `dpkg`/`rpm` e a execução gráfica nesses containers não foram testadas. O mapa de arquivos do Tauri referencia os SONAMEs do host de build (neste release, `libvpx.so.12`, `libyuv.so` e `libjpeg.so.8`); ao construir em outra distribuição, ajuste `bundle.linux.deb.files`/`rpm.files` ou use um container com esses SONAMEs.

### Windows

Siga [WINDOWS.md](docs/WINDOWS.md): `npm run build:windows` prepara os codecs
estáticos e gera o instalador NSIS `.exe`, com instalação por usuário e
verificação de WebView2. Há também um workflow GitHub Actions. O build MSVC
e o teste em uma máquina Windows limpa ainda não foram executados.

## Operação

Há arquivos de exemplo em `deploy/`:

- `concord.example.toml`: configuração comentada do servidor.
- `Caddyfile`: TLS automático + proxy WebSocket para `127.0.0.1:8787`.
- `concord-signaling.service`: unidade systemd endurecida.
- `coturn.conf`: TURN de produção para voz em redes com NAT restritivo.
- `turn/`: laboratório coturn restrito a loopback e teste de relay forçado.

Passos resumidos em um servidor Linux:

```bash
sudo useradd --system --home /opt/concord --shell /usr/sbin/nologin concord
sudo install -d -o concord -g concord /opt/concord /etc/concord
sudo install -m 0755 target/release/concord-signaling /usr/local/bin/concord-signaling
sudo install -m 0600 -o concord -g concord concord.toml /etc/concord/concord.toml
sudo install -m 0644 deploy/concord-signaling.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now concord-signaling
```

Aponte o Caddy para `127.0.0.1:8787` e use `wss://concord.exemplo.com` no cliente. Não exponha o servidor WebSocket em HTTP puro fora de uma rede local controlada.

## Segurança e limitações conhecidas

- **TLS é responsabilidade do proxy.** O binário de sinalização escuta WS local; use Caddy/nginx para WSS.
- **Tokens são segredos de longa duração.** Não há autoatendimento, expiração automática nem revogação em runtime; rotacione o arquivo e reinicie o servidor.
- **Sessões em memória.** Reiniciar encerra sessões; o histórico persiste com Neon configurado. Sem banco, o chat é temporário. Integração real com Neon e bucket precisa de validação com as credenciais do servidor.
- **Sem moderação.** O dono do servidor controla tokens e salas; não há papéis, banimento ou auditoria.
- **Rate limiting local.** O token bucket protege contra floods por conexão; não substitui firewall, proteção DDoS ou limite de conexões por IP.
- **Voz com ganho manual e supressão opcional, sem AEC.** O painel Áudio oferece volume e redução de ruído SpeexDSP. Não há cancelamento de eco nem ganho automático. Use fones; em alto-falantes o microfone pode captar o retorno e gerar eco.
- **Malha limitada a 8 pessoas.** Cada participante mantém e envia 7 fluxos no pior caso. A escala de 8 pessoas e 30 minutos ainda não foi medida; o teste automatizado completo usa dois clientes.
- **Recuperação de conexão e peers.** Um peer que falha é recriado após 2 s, sem ICE restart. Se o WebSocket cair, o cliente encerra a mídia, reconecta e tenta voltar à sala automaticamente; voz e tela são retomadas pelo usuário. Veja [CONEXAO.md](docs/CONEXAO.md).
- **Dispositivos de áudio.** Seleção e troca na chamada implementadas; a validação com hardware real ainda está pendente. Veja [MICROFONE.md](docs/MICROFONE.md).
- **Windows e Wayland de voz não validados.** CPAL e Opus são multiplataforma, mas a validação de devices, eco e integração foi feita apenas em Linux/X11 neste ciclo.
- **Tela nativa por plataforma.** A captura X11 é da tela inteira do
  root X11, sem seleção de janela. No Wayland, o portal escolhe o monitor ou a
  janela e fornece o acesso PipeWire. Esse caminho ainda exige validação visual
  na sessão real. O backend Windows/WGC captura o monitor principal; seu build
  e validação em Windows continuam pendentes.
- **Tela validada em 720p60 e 1080p30 por 10 minutos; 720p30 em validação.**
  O 1080p60 continua experimental, e adaptação de bitrate e 8 pessoas com vídeo
  ainda precisam de medição.
- **WebView variável por SO.** A UI deve ser validada em WebKitGTK e WebView2 reais; não há Chromium embutido.
- **AppImage acima do orçamento.** O AppImage portátil tem 111 MiB porque inclui WebKitGTK/JSC/ICU; o `.deb`/`.rpm` de 6,27 MiB é o artefato leve para distribuição interna.

## Testes e qualidade

```bash
# Backend, protocolo, domínio, mídia, RTC, app e servidor
cargo test --workspace
cargo test -p concord-app

# Captura X11 é opt-in; rode o crate de mídia com a feature
cargo test -p concord-media --features x11

# Opus real atravessando WebRTC
cargo test -p concord-rtc --test audio_signaling

# Vídeo VP8: adicionar/remover e compartilhamento simultâneo
cargo test -p concord-rtc --test video_signaling
cargo test -p concord-rtc --test bidirectional_video

# WebRTC real (normal e relay forçado)
cargo test -p concord-rtc --test data_channel_signaling
cargo test -p concord-rtc --test data_channel_signaling \
  forced_turn -- --ignored --nocapture

# Dois clientes completos com devices reais (precisa de fones/microfone)
cargo test -p concord-app --test voice_smoke -- --ignored --nocapture

# Dois clientes com captura X11 real (precisa de display e áudio)
cargo test -p concord-app --test screen_smoke -- --ignored --nocapture

# Soak de 10 min em um perfil de tela (repita para 720p30/720p60/1080p30)
CONCORD_SCREEN_TIER=720p60 CONCORD_SCREEN_SOAK_SECS=600 \
  cargo test --release -p concord-app --test screen_smoke -- --ignored --nocapture

# Janela GTK nativa (precisa de display)
cargo test -p concord-app native_viewer_opens_draws_and_closes -- --ignored --nocapture

# CPAL real de captura + playback
cargo test -p concord-media --lib \
  real_devices_capture_and_play_for_one_second -- --ignored --nocapture

# Lints estritos (produção + testes)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p concord-media --features x11 --all-targets -- -D warnings

# Frontend
npm run check
npm run build
```

O comando de clippy é estrito; módulos de teste têm uma exceção local e explícita para `expect`/`unwrap`, usados apenas como asserções de teste. Código de produção não usa `unwrap`/`expect`.

Testes de integração do servidor criam um listener efêmero e exercitam autenticação, lista de salas, entrada, chat, presença, relay de sinalização, rejeição de convite e `/healthz`. O `voice_smoke` cria o servidor no mesmo processo, mas usa dois `DesktopState` reais, CPAL e Opus reais; ele é ignorado por padrão para não falhar em máquinas sem dispositivo de áudio.

## Documentação

- [`docs/PLANO.md`](docs/PLANO.md): arquitetura, escopo, orçamentos e riscos.
- [`docs/ROADMAP.md`](docs/ROADMAP.md): marcos, critérios de aceite e o que ainda falta.
- [`docs/CALL_LIFECYCLE.md`](docs/CALL_LIFECYCLE.md): implementação da entrada,
  negociação, áudio, reconexão e falhas da chamada de voz.
- [`docs/adr/`](docs/adr/): decisões arquiteturais curtas.
- [`deploy/turn/README.md`](deploy/turn/README.md): laboratório TURN e teste de relay.
- [`deploy/README.md`](deploy/README.md): runbook de operação.

## Licença

MIT OR Apache-2.0. Os arquivos de licença ainda não foram adicionados ao repositório.

## Uso diário

O app inclui respostas citadas, edição/exclusão de mensagens próprias, links e
busca no histórico; marcador de leitura e notificações silenciáveis; anexos por
arrastar/colar, imagem ampliada e downloads com progresso/cancelamento.
Em Preferências ficam o atalho configurável do microfone (padrão Ctrl+Shift+M)
e a bandeja. Detalhes de rede ficam recolhidos em Diagnóstico na chamada.
Veja [guia das melhorias](docs/MELHORIAS_USO.md), incluindo limites de plataforma
e atualização da pasta independente ConcordServer.

## Conta e entrada automática

Com o servidor Neon atualizado, o app abre no login. Quem não tem conta usa
Criar conta com o convite; o cadastro volta ao login. Depois do primeiro login,
a sessão fica salva nativamente e é retomada ao abrir o app. Sair da conta remove
a sessão salva. O servidor mantém uma presença por conta e a saída agora fecha
explicitamente a chamada/WebSocket. Veja [guia de contas](docs/CONTAS.md).

# Ciclo de vida da chamada

Este documento descreve o comportamento **implementado** da chamada do Concord:
voz do Marco 3 e compartilhamento de tela do Marco 4 no Linux/X11. Ele cobre
entrada, negociação, pipeline de áudio e vídeo, saída e recuperação de falhas.

## 1. Escopo

- A chamada pertence a uma sala de texto. Não existe chamada global.
- A topologia é malha: cada participante mantém uma `RTCPeerConnection` com
  cada outro participante da chamada.
- O servidor de sinalização retransmite SDP/ICE e presença. O áudio Opus vai
  direto entre os pares por DTLS-SRTP e nunca passa pelo servidor.
- Uma pessoa pode estar na sala sem estar na chamada; a UI chama isso de
  “entrar na voz”.
- O compartilhamento de tela usa as mesmas `RTCPeerConnection`: VP8 direto entre
  os pares, sem passar pelo servidor nem pelo WebView.
- O limite operacional é o mesmo da sala: 2–8 pessoas. O teto da malha é uma
  proteção de produto, não uma meta de escala.
- Um cliente tem no máximo uma chamada ativa por vez, na sala em que está.

## 2. Estado local

O snapshot exposto à UI contém `call: Option<CallInfo>`. Quando existe, o
`CallState` é um destes:

```text
sem chamada ──join_call──► Joining ──actor pronto──► Active
     ▲                         │                        │
     │                         └──falha de peer──► Reconnecting
     └──── sair / sair da sala / disconnect ────────────┘
```

| Estado | Significado |
| --- | --- |
| `Joining` | Microfone/codec abrindo; presença `in_call=true` e peers sendo criados |
| `Active` | Chamada em andamento; `Reconnecting`/`Failed` ausentes entre os peers conhecidos |
| `Reconnecting` | Pelo menos um peer falhou, fechou ou reportou erro; recuperação/retry em andamento |

`CallInfo` também carrega `muted`, `deafened`, a lista de peers com estado,
contadores de perda/PLC e estatísticas de áudio (`capture_underruns`,
`playback_underruns`, `playback_dropped`, `stream_errors`, `tx_frames`,
`rx_frames`).

### Por que existe um estado interno de inicialização

Abrir CPAL e o encoder Opus leva alguns milissegundos. Durante essa janela,
mensagens de presença e `signal` do servidor podem chegar antes de o ator da
chamada estar instalado no `DesktopState`. No M3, isso causou uma corrida real:
um cliente entrava na chamada, mas não criava a sessão do outro porque a
presença inicial era descartada.

A solução implementada é um slot com três estados internos:

```text
Idle ──join_call──► Starting(Vec<CallCommand>) ──actor instalado──► Active(CallHandle)
```

- `Idle`: não há chamada; comandos de chamada são ignorados.
- `Starting`: o áudio está abrindo. Presença e sinais recebidos são
  **bufferizados** em vez de descartados.
- `Active`: o handle do ator está instalado. Os comandos vão direto para o canal
  do ator.

Uma `AsyncMutex` serializa `join_call`/`stop_call`. Antes de instalar o handle,
o `join_call` drena os comandos bufferizados no canal do ator e só então libera
o portão inicial (`CallHandle::begin`). Assim, presença, sinais e mute chegam na
ordem correta depois que o ator existe.

## 3. Protocolo

O protocolo v1 ganhou dois campos opcionais, compatíveis para frente e para
trás:

```rust
// Participant
#[serde(default)]
pub in_call: bool,

// ClientMessage::UpdatePresence
#[serde(default, skip_serializing_if = "Option::is_none")]
pub in_call: Option<bool>,
```

Regras implementadas:

- `Participant.in_call` é `false` para clientes antigos e para quem está apenas
  na sala de texto.
- `UpdatePresence` é parcial: só os campos presentes são alterados.
- O servidor só aceita presença de quem está em uma sala (`NotInRoom` caso
  contrário), portanto `in_call=true` não existe fora de sala.
- O servidor atualiza o participante e transmite `ParticipantUpdated` aos
  membros da sala.
- SDP/ICE continuam usando `ClientMessage::Signal` e `SignalPayload` existentes;
  não há mensagem de “início de chamada”.

## 4. Entrar na chamada

Pré-condição: o cliente está conectado e dentro de uma sala.

1. A UI chama `join_call`. O cliente valida conexão, sala e identidade, e muda o
   slot interno para `Starting`.
2. O processo Rust abre o microfone e a saída com CPAL, cria o encoder Opus e
   inicia o ator da chamada. O ator fica bloqueado em um portão inicial.
3. O cliente instala o handle, drena comandos bufferizados e libera o portão.
4. O ator envia:

   ```json
   {"type":"update_presence","in_call":true}
   ```

   e monta a lista de alvos a partir dos participantes que já têm
   `in_call=true`. Quem entrou antes é alvo imediatamente; quem entrar depois
   chega via `ParticipantUpdated`.
5. Para cada alvo, o papel é determinístico:

   ```text
   menor UserId -> Offerer
   maior UserId -> Answerer
   ```

6. O offerer cria a `PeerSession` de áudio e envia `signal` com
   `payload.kind = "offer"` e o SDP. O answerer cria a sessão ao receber a
   oferta, aplica o SDP e responde com `payload.kind = "answer"`.
7. Os dois lados trocam candidatos ICE com `payload.kind = "ice"`. Candidatos
   locais são bufferizados até a oferta/resposta ser emitida; candidatos remotos
   são enfileirados até a descrição remota ser aplicada.
8. A faixa de áudio Opus usa o payload type dinâmico 111. Enquanto negocia, o
   peer aparece como `connecting`; ao ficar pronto, muda para `connected`.
9. O ator entra no tick de 20 ms e começa a capturar, codificar, enviar, receber,
   decodificar e mixar áudio.

### Entrada tardia

Uma pessoa que entra depois recebe o snapshot da sala com os `in_call` já
atualizados e cria sessões para todos os que já estão na chamada. O papel é
recalculado par a par; não existe um “anúncio de chamada” separado.

Se um `signal` chegar para um `UserId` que ainda não está em `targets`, o ator
adota o alvo usando o nome do participante da sala. Isso cobre a corrida entre a
presença e a primeira oferta.

### Duas entradas simultâneas

- O menor dos dois IDs oferece; o maior espera.
- Cada lado pode aplicar uma oferta recebida de um membro da sala com
  `in_call=true`, mesmo que já tenha uma sessão.
- Se um sinal inesperado chegar para a sessão existente (por exemplo, oferta em
  quem já é offerer), a sessão marca o peer como `Reconnecting` e agenda a
  recriação após 2 s; não há renegociação em vigor.
- Eventos de uma sessão antiga carregam um `instance` interno; eventos de uma
  instância que já foi substituída são ignorados.

## 5. Pipeline de áudio

### Captura e envio

- CPAL abre os dispositivos escolhidos no painel Áudio e salvos localmente,
  ou os padrões do sistema. Variáveis CONCORD_AUDIO_* só inicializam a seleção
  quando ainda não há arquivo de preferências de dispositivos.
- A captura é convertida para `f32`, downmixada para mono, reamostrada para
  48 kHz e escrita em um ring buffer de 1 s. Os callbacks de áudio não alocam e
  não falam diretamente com o Tokio.
- A cada 20 ms o ator lê 960 amostras (um quadro Opus) do ring. Se o buffer
  ainda não tem amostras suficientes, o restante é zero e um
  `capture_underrun` é contado.
- O encoder Opus usa 48 kHz, mono, 20 ms e 32 kbps.
- Se `muted=true`, o ator continua drenando o microfone para manter o ring
  estável, mas **não codifica nem envia** o quadro. Mudo é aplicado no ator, não
  apenas na flag de presença.
- O quadro é escrito na faixa de cada peer `connected`. `tx_frames` conta
  ticks em que pelo menos um peer recebeu o quadro.

### Recepção e playback

- Cada peer tem seu próprio decoder Opus e um `JitterBuffer` de até 4 quadros
  decodificados (80 ms).
- Pacotes duplicados ou atrasados são contados e descartados; lacunas de
  sequência incrementam `packets_lost`.
- A cada tick, o ator retira um quadro de cada peer. Se a fila está vazia, usa
  PLC do Opus para gerar um quadro de ocultação em vez de um clique.
- Os quadros dos peers são somados e passam por `tanh` para limitar picos.
- O quadro misto entra em um ring de playback de 250 ms. O ring começa com
  120 ms de silêncio para absorver jitter; se ficar cheio, o quadro novo é
  descartado para limitar a latência.
- A saída é reamostrada do 48 kHz para o formato do device e replicada nos
  canais.
- Se `deafened=true`, o ator continua recebendo, decodificando e consumindo o
  áudio remoto (para não deixar a fila crescer), mas mistura zero na saída.
  `rx_frames` continua crescendo.

### Limitações de áudio

- O microfone tem ganho manual 0–200% e supressão opcional SpeexDSP antes do
  Opus, com ajustes locais persistidos. Veja [MICROFONE.md](MICROFONE.md).
- Não há AEC ou AGC. Em alto-falantes, o áudio reproduzido
  pode voltar pelo microfone. Fones são recomendados.
- O reamostrador é linear e intencionalmente pequeno; ele mantém devices de
  44,1 kHz/96 kHz funcionais, mas não é DSP de alta qualidade.
- A seleção de dispositivos pode ser alterada durante a chamada: prepara
  streams novos fora do ator e substitui apenas as direções alteradas,
  preservando peers e a faixa Opus. Ver [MICROFONE.md](MICROFONE.md).
- O codec é mono; não há áudio estéreo nem música de alta fidelidade.

## 6. Sair da chamada

### Saída normal

1. `stop_call` adquire o lock de ciclo de vida e retira o handle ativo.
2. O ator envia `SignalPayload::Bye` para cada peer conhecido.
3. Envia `update_presence { "in_call": false }`.
4. Fecha as `RTCPeerConnection`, aborta as tarefas de eventos, libera CPAL e o
   encoder.
5. O snapshot local passa a `call = null`; o servidor retransmite a presença
   para os demais, que removem o peer.

`Bye` é melhor esforço. A presença é a fonte de verdade: mesmo que o `Bye` se
perca, `in_call=false` ou `participant_left` encerram a sessão.

### Sair da sala e desconectar

- `join_room` chama `stop_call` antes de enviar `JoinRoom`; não existe chamada
  órfã ao trocar de sala.
- `leave_room` chama `stop_call` antes de enviar `LeaveRoom`.
- `disconnect` invalida a geração, limpa o snapshot, libera a mídia e cancela
  o worker de conexão. O fechamento do transporte remove a presença no servidor.
- O supervisor de conexão encerra a chamada apenas se sua geração ainda for
  a atual. Um worker antigo não pode destruir a chamada de uma sessão nova.

### Queda abrupta

Se o processo cair ou a rede sumir, o servidor detecta o fechamento do
WebSocket e retransmite `participant_left`. Os peers fecham a sessão daquele
usuário e mantêm a chamada com os demais. Como o `UserId` é emitido pelo
servidor, a pessoa que voltar recebe uma identidade nova e as sessões antigas
não são reaproveitadas.

## 7. Falhas, timeouts e retry

| Situação | Comportamento implementado |
| --- | --- |
| `ice_connection_state = failed` | Peer vira `Failed`; após 2 s a sessão é destruída e recriada |
| `PeerState::Closed` inesperado | Peer vira `Reconnecting`; recriação agendada |
| Erro de peer | Loga o erro; se o peer não estiver `Connected`, marca `Reconnecting` e agenda retry. Erros de data channel em peer conectado não derrubam o áudio por si só |
| `ice_connection_state = disconnected` | UI mostra `Reconnecting` e aguarda recuperação do WebRTC |
| Falha ao criar sessão/oferta/jitter | Loga o erro e agenda nova tentativa |
| Falha ao aplicar um `signal` | Marca o peer como `Reconnecting` e agenda nova tentativa |
| `Bye` recebido | Remove o alvo e fecha a sessão imediatamente |
| Sinalização cai | O supervisor encerra a mídia, mostra `Reconnecting`, autentica novamente e tenta voltar à sala; chamada e tela são retomadas pelo usuário |
| Usuário sai da sala | `participant_left` remove o peer; a chamada dos demais continua |

Detalhes:

- O retry é fixo em 2 s e deduplicado por `UserId`; não há backoff
  exponencial, ICE restart nem negociação em vigor.
- Recriar um peer significa descartar a `PeerSession` antiga e negociar SDP/ICE
  de novo. A sessão antiga é fechada e sua tarefa de eventos é abortada.
- Eventos de peers carregam um número de instância; eventos atrasados da sessão
  antiga são ignorados.
- A malha tem até 7 peers remotos (8 pessoas incluindo você,
  `MAX_MESH_PEERS`). Ao atingir o limite, novas sessões não são criadas e um
  aviso é logado.
- A reconexão automática da **sinalização** está implementada com backoff,
  cancelamento, heartbeat e recuperação de sala; veja [CONEXAO.md](CONEXAO.md).
  Se o WebSocket cair, o áudio é encerrado. A retomada automática da mídia
  não faz parte desta recuperação.

## 8. Validação no servidor

O servidor não interpreta SDP/ICE, mas mantém as seguintes garantias:

- `to` precisa ser membro da mesma sala que o remetente.
- O remetente precisa estar autenticado e na sala.
- `in_call` só pode ser alterado por quem está em uma sala.
- SDP/ICE continuam limitados por `MAX_SDP_BYTES` e
  `MAX_ICE_CANDIDATE_BYTES` e pelo limite de quadro do protocolo.
- Mensagens de um usuário que não está na chamada podem ser ignoradas pelo
  cliente, mas o servidor não depende desse filtro para segurança.
- Não há relay de mídia, gravação ou inspeção de SDP no servidor.

## 9. Observabilidade

Logs relevantes incluem:

- `local_user`/`remote_user` implícitos no evento e no campo `user`;
- entradas/saídas da chamada e início/fim do ator;
- transições de peer (`voice peer connected`, `voice peer error`, retry);
- falha ao criar/encerrar sessão, oferta ou jitter buffer;
- motivo de término (`Bye`, saída da sala, disconnect, erro de socket).

Nunca são logados token, SDP completo ou credenciais TURN. O snapshot expõe à UI
os contadores de áudio, perda, PLC, quadros enfileirados e estado de cada peer.
A UI mostra perda/PLC agregados na dock da chamada.

## 10. Compartilhamento de tela (M4, Linux/X11)

### Início

1. A UI chama `DesktopState::set_screen_sharing(true)`, que envia
   `CallCommand::SetScreenShare` ao `CallActor` da chamada ativa.
2. O ator escolhe o perfil (`720p30` padrão, `720p60` ou `1080p30`; `1080p60`
   apenas experimental) e inicia uma tarefa de preparação. Em X11, usa MIT-SHM
   da tela inteira; em Wayland, o portal seleciona monitor/janela e fornece
   o remote PipeWire. Durante o consentimento, ticks de áudio e comandos de
   mute/saída continuam ativos. A preparação inicia as threads de captura/encode,
   valida o encoder VP8 e abre o preview GTK local.
   Só depois de o pipeline estar pronto ele anuncia presença
   `screen_sharing: true`.
3. Para cada peer, o ator chama `PeerSession::start_video`. O offerer
   determinístico instala a faixa local e cria a oferta. O answerer envia
   `SignalPayload::Renegotiate` e instala a faixa ao aplicar a oferta.
4. Quando o answerer começa a compartilhar, o offerer instala uma faixa local
   ociosa antes de ofertar. A mídia fica `sendrecv`, garantindo receiver nos dois
   lados. Cada nova faixa usa um SSRC novo; reutilizar o SSRC anterior fazia o
   SRTP remoto rejeitar os pacotes depois de remover/adicionar a faixa.

### Mídia

- A captura lê a tela e entrega frames BGRA a uma fila limitada de dois frames.
  A thread de encode escala para o perfil ativo, converte para I420 e codifica
  em VP8 a 30 ou 60 fps. Se a fila estiver cheia, a captura descarta o novo
  frame; a fila mantém no máximo dois frames aguardando encode. libyuv (SIMD) acelera escala/conversão quando
  instalada; sem ela o build usa o escalador Rust.
- O frame codificado segue por `PeerSession::write_video` e vira RTP/SRTP direto
  para cada peer. O preview local usa o BGRA escalado, guardado em um slot de
  “último frame”; o ator o consome quando processa o frame codificado, sem
  enfileirar previews antigos.
- Nenhum frame passa pelo IPC do Tauri ou pelo WebView. A UI recebe apenas
  presença, erros e contadores.
- No receptor, `PeerEvent::RemoteVideo` alimenta um decoder VP8. O primeiro
  keyframe abre uma janela GTK nativa; frames seguintes só atualizam a imagem.
- PLI/FIR recebidos viram `PeerEvent::VideoKeyframeRequested`. O ator pede um
  keyframe ao encoder, e perda de quadro marca o peer como aguardando keyframe.
- O preview local e a janela remota são janelas GTK separadas. Fechar a janela
  remota só dispensa a visualização; a transmissão continua.

### Troca de qualidade

1. O seletor da dock chama `set_screen_tier` com `720p30`, `720p60`,
   `1080p30` ou `1080p60`. O último aparece como `1080p60 (exp.)` e usa
   6.000 kbps; a variável `CONCORD_SCREEN_TIER` continua definindo o perfil inicial.
2. O ator envia `CallCommand::SetScreenTier`. Se não há compartilhamento ativo,
   apenas atualiza o perfil para o próximo início.
3. Com compartilhamento ativo, o ator prepara um encoder no perfil escolhido.
   A thread aplica a mudança antes de preparar o próximo frame e força um
   keyframe. A captura autorizada, a faixa WebRTC, o SSRC e a presença
   `screen_sharing` são preservados; o portal não pede consentimento novamente.
4. Se a preparação do encoder falhar, o perfil anterior é restaurado e o erro aparece na
   dock. O seletor volta ao valor antigo.

### Parada e concorrência

1. A UI chama `set_screen_sharing(false)`. O ator para a captura, remove a faixa
   local e pede renegociação, fecha o preview e anuncia presença `false`.
2. A presença `false` fecha a janela remota e limpa o decoder. O fim do track
   remoto sozinho não é usado como sinal de parada.
3. Se os dois lados compartilharem ao mesmo tempo, o offerer trata o
   `Renegotiate` recebido durante a própria oferta como pendente e cria uma nova
   oferta depois da resposta. O teste `bidirectional_video` cobre esse glare.
4. O offerer pode manter uma faixa `sendrecv` ociosa depois que o outro lado
   para; ela não envia quadros e preserva o receiver para um próximo
   compartilhamento.

### Evidências

```bash
cargo test -p concord-rtc --test video_signaling
cargo test -p concord-rtc --test bidirectional_video
cargo test -p concord-app --test screen_smoke -- --ignored --nocapture
cargo test -p concord-app native_viewer_opens_draws_and_closes -- --ignored --nocapture
```

- [x] Faixa VP8 adicionada, usada e removida pela sinalização.
- [x] Dois peers compartilham ao mesmo tempo e decodificam as duas direções.
- [x] Dois clientes completos com captura X11 real, parada e nova transmissão.
- [x] Janela GTK nativa abre, desenha e fecha.
- [x] Nenhum frame de tela é serializado no IPC.
- [x] Probe de release, frame sintético 1920×1080 BGRA: 720p30 19,70 ms,
  720p60 14,18 ms, 1080p30 28,50 ms e 1080p60 22,90 ms por quadro
  (prepare+preview + encode + decode). Captura full-screen X11: 7,00 ms/frame.
- [x] Soaks curtos de 30 s com dois clientes: 720p30 29,2 fps, 720p60 58,2 fps
  (60,3 fps no segundo trecho de 10 s) e 1080p30 29,3 fps, todos com zero erros
  de decode.
- [x] Troca de perfil em tempo real sem derrubar a faixa WebRTC nem a presença.
- [x] Soak de 10 minutos de 720p60 com o pipeline novo: 35.508 quadros,
  59,2 fps, CPU agregada de 174,6%, RSS de 255,2 MiB e zero erros de decode.
- [x] Soak de 10 minutos de 1080p30: 17.893 quadros, 29,8 fps, CPU agregada de
  78,4%, RSS de 296,0 MiB e zero erros de decode; transmissor com 18.002
  quadros e 582 keyframes.
- [ ] Soak de 10 minutos de 720p30 com o pipeline novo.
  Estado e limitações da revisão em [VALIDACAO_TELA.md](VALIDACAO_TELA.md).
- [ ] 1080p60 promovido, adaptação de bitrate e 8 pessoas com vídeo.
- [x] Backend Wayland com portal/PipeWire e troca de qualidade sem reabrir o portal.
- [ ] Validação visual do Wayland e implementação de Windows/WGC.

### Limitações

- Em X11, captura apenas da tela inteira do root. No Wayland, o portal permite
  escolher monitor/janela conforme o suporte do desktop. Veja [WAYLAND.md](WAYLAND.md).
- O backend Wayland/PipeWire foi adicionado, mas ainda não foi validado
  visualmente; Windows WGC não foi implementado.
- `1080p60` está disponível como experimental no seletor; VP8 por software não
  tem margem medida para 60 fps sustentados nesta máquina.
- A visualização é uma janela GTK separada, não um painel embutido na UI.
- Não há AEC; o áudio continua com as limitações do Marco 3.

## 11. Critérios e evidências

### Verificado

- [x] Dois clientes reais entram na mesma sala e na chamada.
- [x] O menor `UserId` inicia a oferta e o outro responde.
- [x] Áudio Opus bidirecional atravessa WebRTC e é decodificado.
- [x] Mudo bloqueia o envio de quadros e desmudo retoma.
- [x] Ensurdecido continua consumindo/decodificando o áudio remoto.
- [x] `Bye`/presença removem o peer da chamada sem derrubar os demais.
- [x] Sair da sala encerra a chamada ativa.
- [x] `disconnect` encerra a chamada ativa.
- [x] Presença/sinais recebidos durante a abertura do áudio não são perdidos.
- [x] Eventos de sessões substituídas e leitores de gerações antigas são
  ignorados.
- [x] O mesmo teste de dois clientes passa com `CONCORD_FORCE_RELAY=1` contra o
  coturn de laboratório.

Comandos reproduzíveis:

```bash
cargo test -p concord-rtc --test audio_signaling
cargo test -p concord-app --test voice_smoke -- --ignored --nocapture
CONCORD_TURN_URL='turn:127.0.0.1:3478?transport=udp' \
  CONCORD_TURN_USERNAME=concord \
  CONCORD_TURN_CREDENTIAL=concord-test \
  CONCORD_FORCE_RELAY=1 \
  cargo test -p concord-app --test voice_smoke -- --ignored --nocapture
```

O `voice_smoke` usa dois `DesktopState` reais, um `AppHandle` mock do Tauri, um
servidor de sinalização efêmero e os devices reais de áudio. A UI não participa
do caminho de mídia; a validação visual de duas janelas é manual.

### Ainda pendente

- [ ] Três ou mais peers simultâneos; o teste automatizado cobre dois.
- [ ] Sete pares (8 pessoas) por 30 minutos em LAN.
- [ ] WAN/NAT real com credenciais TURN de produção e TLS.
- [x] Ganho manual e supressão de ruído opcionais, configuráveis no app.
- [ ] AEC/AGC e teste com alto-falantes.
- [x] Seleção e troca de dispositivos pela interface; validação com hardware real pendente.
- [ ] Renegociação de faixas de áudio e ICE restart; a faixa de vídeo já
  renegocia adicionar/remover.
- [x] Reconexão automática da sinalização e recuperação da sala; validação WAN pendente.
- [ ] Validação de voz e tela em Windows e Wayland nativo.

## 12. Fora de escopo

- SFU, encaminhamento de mídia e salas acima de 8 pessoas.
- Gravação, transcrição e E2EE de camada aplicacional.
- Chamadas fora de uma sala ou entre servidores.
- Retomada automática da voz e do compartilhamento depois de reconectar.
- Renegociação para trocar o codec de áudio. Dispositivos locais já podem ser trocados mantendo a faixa atual.
- Gravação de tela, múltiplas fontes simultâneas por pessoa e seleção de janela
  nesta versão.

## Saída de sessão e conta permanente

A saída explícita fecha mídia e sinaliza o supervisor para enviar LeaveRoom e
Close antes de concluir; abort fica como fallback limitado. No servidor,
SessionGuard remove sessão/presença no fechamento, falha ou ausência de frames
de aplicação por 90 s. A remoção verifica a chave do transporte e enfileira
ParticipantLeft antes de liberar inserção/entrada da conta nova. Uma conta tem
um ID permanente e uma conexão WebSocket ativa. Auth lento em socket fechado
não pode assumir a sessão, e a conexão substituída recebe erro fatal para não
reconectar em ciclo. Detalhes em [CONTAS.md](CONTAS.md).

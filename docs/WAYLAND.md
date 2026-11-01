# Compartilhamento de tela no Wayland

O Concord implementa captura nativa pelo portal ScreenCast do desktop e
PipeWire. O backend é selecionado automaticamente em sessões Wayland, mesmo
com `DISPLAY` do Xwayland definido. Em sessões X11, continua usando MIT-SHM.

## Usar no desktop

1. Execute o binário atualizado na sua sessão Wayland, entre na sala e na
   chamada de voz.
2. Escolha `720p30`, `720p60`, `1080p30` ou `1080p60 (exp.)` e clique em **tela**.
3. No seletor do sistema, escolha um monitor ou uma janela e autorize a
   captura. A disponibilidade de janelas depende do backend do desktop.
4. Confira o preview local e o viewer do outro participante. Troque a qualidade
   durante a transmissão: o seletor do portal não deve reaparecer.
5. Use **parar** para encerrar. A sessão do portal e o acesso PipeWire são
   fechados. Também é possível encerrar pelo indicador do próprio desktop.

Cancelar a seleção não anuncia presença de compartilhamento. Durante a
seleção, o áudio e os comandos de mute/saída continuam funcionando. Sair da
chamada cancela a solicitação pendente. O portal aguarda até 120 s por resposta
a cada solicitação; o comando do app possui timeout de 180 s para o início.

Para gerar um executável com o frontend embutido, sem reconstruir instaladores:

```bash
npm run tauri build -- --no-bundle
./target/release/concord-app
```

Os instaladores antigos em `target/release/bundle/` não contêm este backend.

## Dependências

No build Linux, os pacotes de desenvolvimento precisam disponibilizar
`libpipewire-0.3` e `gio-unix-2.0` para `pkg-config`, além dos requisitos já
descritos no README. Em Debian/Ubuntu, use `libpipewire-0.3-dev` e
`libglib2.0-dev`; neste host Arch, os headers são de `libpipewire` e `glib2`.

No desktop, devem estar disponíveis PipeWire, o serviço
`xdg-desktop-portal` e um backend com ScreenCast, como
`xdg-desktop-portal-kde` no Plasma. O backend precisa corresponder ao desktop;
o Concord não instala nem altera serviços da sessão.

## Implementação

- `crates/media/src/wayland_capture.c`: ponte para as APIs C de GIO e PipeWire.
  Cria uma conexão D-Bus privada, chama `CreateSession`, `SelectSources`,
  `Start` e `OpenPipeWireRemote`. Assina `Request.Response` antes de chamar os
  métodos e observa `Session.Closed`. Usa o descritor autorizado pelo portal,
  com suporte a `pipewire-serial` e fallback para node ID em portais antigos.
- A ponte executa em uma thread nativa. Objetos GIO/PipeWire e callbacks não
  saem dessa thread. As solicitações pendentes são fechadas ao cancelar; o
  encerramento fecha o stream, o remote e a sessão do portal.
- `crates/media/src/wayland.rs`: valida dimensões, stride e comprimento dos
  buffers, converte BGRx/BGRA/RGBx/RGBA para BGRA opaco e mantém somente o
  frame mais recente. Uma tela estática reaproveita o último frame na cadência
  do encoder; revogação do portal retorna erro e encerra o compartilhamento.
- `src-tauri/src/screen/linux.rs`: escolhe o backend, reutiliza o pipeline VP8
  e o preview GTK. A troca de perfil substitui o encoder e solicita um
  keyframe, preservando a captura autorizada e a faixa WebRTC.
- `src-tauri/src/call.rs`: prepara a captura em uma tarefa separada, mantendo
  ticks de áudio, eventos dos peers e comandos ativos durante o consentimento.
  Presença só é anunciada após a preparação. Abort/timeout cancela a captura.
- Nenhum pixel nem descritor PipeWire é enviado ao JavaScript ou ao IPC Tauri.
  O caminho de mídia permanece nativo: captura → BGRA → I420 → VP8 → WebRTC.

## Verificação e limites

Os testes unitários cobrem padding, conversão de cores, buffers truncados e
dimensões inválidas, cache de tela estática, revogação, cancelamento antes de
acessar o desktop, seleção de backend e abort de preparação. Um teste do
pipeline troca 32×32 → 64×48 → 32×32 pelo mesmo canal de captura e decodifica
os keyframes resultantes com libvpx. Ele também confere dimensões do preview,
cadência e preservação da instância.

Nesta revisão, `cargo test --workspace --lib` passou com 69 testes aprovados
e 5 ignorados. Clippy do workspace, check/build do frontend e análise estática
da ponte C passaram. `npm run tauri build -- --no-bundle` gerou o executável
release atualizado. `ldd` resolve GIO/GLib e `libpipewire-0.3.so.0` sem
bibliotecas faltantes. O SHA-256 do executável desta revisão é
`7dcf754dac573440113b219bd48a195089c2ad99b3417d3a5d586e1d5add350c`.
Sua inicialização no ambiente restrito falhou no GTK (`Failed to initialize
GTK`), pois o display também não está acessível.

A captura real ainda não foi validada: o ambiente do
Codex bloqueia sockets, e o teste interativo falhou ao conectar ao D-Bus com
`Could not connect: Operation not permitted`. Isso não comprova o
funcionamento visual em KDE/GNOME; é necessário o teste na sessão real.

Para testar somente a captura, fora do ambiente restrito, autorizando o portal:

```bash
cargo test -p concord-media --features wayland \
  portal_captures_real_frames_and_closes -- --ignored --nocapture
```

Para testar a transmissão entre dois clientes completos:

```bash
cargo test -p concord-app --test screen_smoke -- --ignored --nocapture
```

O smoke completo exige áudio real e autorização para cada nova sessão de
captura, incluindo os compartilhamentos simultâneos. Os soaks antigos X11,
inclusive os executados sob Xwayland, não validam este backend Wayland.

A implementação respeita o recorte `VideoCrop` do compositor e negocia buffers
CPU MemFd/MemPtr e vídeo RGB de 8 bits,
com dimensões até 8192×8192 e stride positivo. DMA-BUF/GPU, HDR e composição
manual de cursor e transformações por metadados não estão implementados; o cursor é solicitado
embutido quando o portal oferece esse modo. A taxa negociada é de até 60 fps,
mas o compositor pode fornecer menos frames; o teste visual precisa conferir
fluidez com conteúdo em movimento. `1080p60` está disponível como experimental
no seletor, sem garantia de 60 fps sustentados.

Referências: [protocolo ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
e [API de streams PipeWire](https://docs.pipewire.org/group__pw__stream.html).

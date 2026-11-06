# Concord no Windows

Suporte implementado para **Windows 10 1903 ou posterior / Windows 11, x64**.
A compilação, instalação e validação em um PC Windows ainda estão pendentes.
Nenhum instalador Windows foi produzido nesta sessão Linux.

O cliente usa o mesmo servidor, protocolo e interface do Linux. Chat, salas,
presença e WebRTC continuam no Rust. CPAL usa o backend de áudio do Windows;
Opus, VP8, volume do microfone e supressão SpeexDSP fazem parte do build.
O servidor no Render e a decisão de usar STUN sem TURN permanecem iguais.

## Tela

- Windows Graphics Capture captura **o monitor principal inteiro**. Não há
  seletor de janela/monitor nesta primeira implementação. A borda de captura
  do sistema permanece habilitada.
- Direct3D11 lê BGRA da GPU respeitando o padding de cada linha. O pipeline
  existente escala, converte e codifica em VP8 em outra thread, com fila de
  dois frames e descarte quando o encoder atrasa.
- A transmissão VP8 continua no pipeline nativo. Prévia local e telas remotas
  aparecem dentro do app; o painel pode ser minimizado e cada tela pode abrir
  em tela cheia. As prévias reduzidas chegam ao WebView por eventos Tauri.
- Os quatro perfis e a troca durante a transmissão são compartilhados com
  Linux. `1080p60 (exp.)` continua a 6.000 kbps, com encode por software e sem
  garantia de 60 fps. Não há aceleração de encode nem tratamento especial HDR.
- A captura nasce, roda e termina na mesma thread com um apartamento WinRT.
  Cancelar a preparação ou sair da chamada sinaliza o encerramento. A troca
  de qualidade mantém o monitor capturado e substitui apenas o encoder.

## Gerar o instalador em um PC Windows

Instale estas ferramentas de desenvolvimento:

1. Git, Rust pelo rustup e Node.js 24 ou posterior.
2. Visual Studio 2022 Build Tools com **Desktop development with C++**, MSVC
   x64 e Windows SDK.
3. LLVM x64 (incluindo `libclang.dll`) e CMake no PATH.

Não é necessário instalar essas ferramentas nos PCs que apenas usarão o app.
O instalador NSIS verifica o WebView2 e oferece a instalação do runtime quando
ele estiver ausente, usando internet. Referências: [pré-requisitos do Tauri](https://v2.tauri.app/start/prerequisites/)
e [instalador Windows](https://v2.tauri.app/distribute/windows-installer/).

Copie o **workspace desktop completo** para o PC Windows; a pasta independente
`ConcordServer/` contém somente o servidor e não serve para compilar o app.
Na raiz do workspace, no PowerShell:

```powershell
npm run build:windows
```

O script `scripts/build-windows.ps1` carrega o ambiente MSVC x64, prepara o alvo
Rust e baixa o vcpkg na versão `2026.10.16` para `target/windows-tools/vcpkg`.
Compila `libvpx`, `libyuv` e `speexdsp` com o triplet `x64-windows-static`.
O crate Rust `opusic-sys` compila o código-fonte embutido do Opus; o arquivo
`scripts/windows-opus-static-runtime.cmake` faz esse código usar o mesmo CRT
estático do Rust. Isso evita misturar CRTs no executável. O build não depende de
DLLs avulsas desses codecs. A primeira execução exige internet e pode demorar,
pois compila as bibliotecas nativas.

O `tauri-build` embute o manifesto do Windows apenas no executável principal. Os
binários de teste também importam `TaskDialogIndirect`; por isso o
`src-tauri/build.rs` embute `src-tauri/windows-app-manifest.xml`
(Common-Controls v6) em todos os alvos. Sem esse manifesto, `cargo test` falha
no Windows com `STATUS_ENTRYPOINT_NOT_FOUND` (0xc0000139) antes de executar
qualquer teste.

O script executa check do frontend, formatação Rust, check de todos os alvos,
testes unitários, Clippy e o build Tauri. O Tauri carrega automaticamente
`src-tauri/tauri.windows.conf.json`, que seleciona NSIS e instalação por usuário
em português. Primeiro compila o executável sem empacotamento e gera um ZIP;
depois executa o empacotador NSIS. Exibe o SHA-256 das saídas. Uma falha no
NSIS preserva o ZIP já gerado.

Saída:

```text
target/x86_64-pc-windows-msvc/release/bundle/nsis/*-setup.exe
target/windows/Concord-Windows-x64.zip
```

O ZIP contém `Concord.exe`, instruções e o instalador online oficial do WebView2,
cuja assinatura Microsoft é conferida ao empacotar. Não contém código-fonte,
Node.js ou convite. Extraia e execute `Concord.exe`; se faltar WebView2, execute
o instalador incluído, que precisa de internet. As preferências ficam no perfil
do usuário. Frontend, codecs, CRT e WebView2Loader ficam embutidos no
executável; o runtime WebView2 é um componente separado.

Para gerar somente o executável e o ZIP, sem NSIS:

```powershell
npm run build:windows -- -PortableOnly
```

Para repetir o build sem recompilar as dependências do vcpkg:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1 -SkipDependencies
```

Se o LLVM estiver em outro local, defina `LIBCLANG_PATH` para a pasta que contém
`libclang.dll` antes de executar. O instalador ainda não tem assinatura digital.

## Gerar pelo GitHub Actions

Foi preparado `.github/workflows/windows.yml`. Coloque o workspace desktop
em um repositório próprio e execute **Actions → Windows desktop → Run workflow**.
O runner `windows-2022` usa o mesmo script. Quando o job concluir, baixe o
artefato **Concord-Windows-x64**; ele contém o instalador NSIS. O artefato
**Concord-Windows-x64-ZIP** contém o ZIP executável e é enviado mesmo se o passo
posterior de NSIS falhar. Se a compilação falhar, não há executável para enviar.

Esse workflow ainda não foi executado. O repositório separado `ConcordServer`
não contém o código desktop nem esse workflow. Não substitua os arquivos do
servidor por este workspace para conseguir compilar o cliente.

Mantenha `concord.toml`, `.env`, credenciais locais e a pasta `conversa/` fora do
repositório. O instalador não precisa conter o convite do grupo.

## Compilar diretamente no Linux

É possível gerar um executável Windows e instalador NSIS no Linux por
compilação cruzada, conforme a [documentação do Tauri](https://v2.tauri.app/distribute/windows-installer/#build-windows-apps-on-linux-and-macos).
São necessários o alvo Rust `x86_64-pc-windows-msvc`, `cargo-xwin`, o SDK/CRT
Windows, LLVM/LLD e os codecs **compilados para Windows**. O NSIS também é
necessário para o instalador; gerar só um ZIP dispensa NSIS, mas continua
exigindo a compilação do executável e suas dependências.

Nesta sessão estão disponíveis clang, LLD e llvm-rc. Faltam o alvo Rust
Windows, cargo-xwin, SDK, bibliotecas Windows e NSIS; não foram encontrados
em cache. As tentativas de acesso a `static.rust-lang.org` e `github.com`
falharam com `Could not resolve host`. Assim, não foi possível produzir aqui
nem o `.exe` nem um ZIP executável. O arquivo `Concord-Windows-source.zip`
disponibilizado anteriormente é somente código-fonte para compilação.

Com o SDK e os codecs Windows disponíveis, o comando base do Tauri seria:

```bash
npm run tauri -- build --runner cargo-xwin --target x86_64-pc-windows-msvc
```

Esse comando sozinho não prepara os codecs do Concord. O fluxo automatizado
preparado neste projeto continua usando MSVC/vcpkg no Windows ou no runner
Windows do GitHub Actions. Não foi implementado ou validado um bootstrap
completo dos codecs MSVC a partir do Linux.

## Validar no Windows

Em um PowerShell, prepare as bibliotecas e o ambiente na mesma sessão:

```powershell
& ./scripts/build-windows.ps1 -PrepareOnly
npm ci
npm run build
cargo test -p concord-app --target x86_64-pc-windows-msvc native_viewer_opens_draws_and_closes -- --ignored --nocapture
cargo test -p concord-app --target x86_64-pc-windows-msvc wgc_capture_returns_valid_pixels_and_stops -- --ignored --nocapture
cargo test -p concord-app --target x86_64-pc-windows-msvc --test screen_smoke -- --ignored --nocapture
```

Esses testes precisam de desktop interativo; o teste completo também usa
microfone e saída de áudio reais. O script de build e o CI executam os testes
unitários sem depender desses dispositivos. Métricas CPU/RSS do teste de tela
dependem de `/proc` e ficam indisponíveis no Windows; os contadores de mídia
continuam válidos.

Depois instale o `*-setup.exe` e confira:

1. Abrir, fechar, reinstalar e desinstalar o app; verificar WebView2 em PC limpo.
2. Conectar em `https://concordserver.onrender.com` com nome e convite privado.
3. Trocar mensagens e áudio com outro PC, incluindo um cliente Linux.
4. Compartilhar a tela, conferir preview/imagem remota, trocar os quatro perfis,
   fechar viewers, parar e retomar a transmissão e sair da chamada.
5. Alterar a resolução do monitor durante a captura, deixar a tela estática,
   repetir a chamada e conferir persistência dos ajustes do microfone.
6. Fazer soaks de 10 minutos e testar entre duas redes reais. Sem TURN, o
   caminho da mídia depende da conexão direta entre as redes.

## Verificação desta implementação

Nesta sessão Linux passaram check do workspace, 81 testes unitários (cinco
ignorados), Clippy e check/build do frontend. As assinaturas e os tipos das
novas APIs WGC/Win32 também foram conferidos em um projeto temporário de
análise no Linux, com adaptações locais nas condições de plataforma dos
bindings. Isso não equivale a compilar ou executar um binário Windows.

Só há alvo Rust Linux instalado. O acesso a `static.rust-lang.org` falhou
com `Could not resolve host`; não foi possível obter o alvo Windows/SDK para
uma compilação cruzada. Portanto, o build MSVC, o script PowerShell, o job
Windows e a captura/áudio/instalação reais permanecem por validar.

Fontes: [Windows Graphics Capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture),
[interop de monitor](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createformonitor),
[frame pool sem DispatcherQueue](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded),
[vcpkg](https://github.com/microsoft/vcpkg/releases/tag/2026.10.16).

### Recursos desktop da revisão de usabilidade

Incluídos atalho global Win32 RegisterHotKey (Ctrl+Shift+M configurável), bandeja
Tauri e notificações ToastGeneric via WinRT. As chamadas também passaram pela
conferência de tipos no harness temporário de bindings no Linux, sem link/execução
Windows. Conferir no build Windows real: permissão de notificações, identidade
AppUserModelID do atalho instalado, conflito de combinação com outro app e
fechar/abrir pela bandeja. Esses testes não foram executados nesta sessão.

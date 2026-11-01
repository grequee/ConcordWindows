# ADR 0001 — Tauri + Svelte 5 com mídia e rede em Rust

- **Status:** aceita
- **Data:** 2026-10-25
- **Marco:** M0
- **Substitui:** —

## Contexto

Precisamos de um app desktop para Windows e Linux com instalador pequeno, partida rápida e baixo consumo ocioso. A familiaridade da equipe com web ajuda na UI, mas o produto não pode depender de Chromium embutido, `getDisplayMedia` ou de um runtime JavaScript para captura, WebRTC e rede.

O Discord inspira a ergonomia, não a arquitetura. O requisito é privacidade e controle do servidor, não compatibilidade com a plataforma Discord.

## Decisão

- Usar **Tauri 2** como shell nativo e **Svelte 5 + Vite** como interface no WebView do sistema.
- Manter **toda** a rede de sinalização, captura, encode/decode e WebRTC no processo Rust.
- Expor à UI apenas um snapshot serializável e comandos IPC explícitos.
- Não usar `getDisplayMedia`; captura será nativa e renderizada fora do WebView.
- Empacotar fontes e assets localmente, sem CDN em runtime.

## Consequências

**Boas**

- Instalador não carrega um Chromium completo.
- O WebView é atualizado pelo SO; menos superfície de dependência para manter.
- A lógica crítica é testável sem UI e sem browser.
- O modelo de estado tipado reduz a chance de a UI se tornar fonte de verdade.

**Custos**

- WebView2 e WebKitGTK divergem em CSS, JS e comportamento de teclado.
- Não existe uma API web universal para captura sem portal; a UI de seleção de tela será específica por SO.
- O IPC precisa ser desenhado para não serializar frames de vídeo.
- Não há hot reload do processo Rust; o ciclo de build é mais lento que uma SPA.

## Alternativas consideradas

- **Electron:** rejeitado por peso, memória e Chromium embutido.
- **Wry/Tauri com UI puramente nativa (egui/iced):** reduziria divergências de WebView, mas aumentaria muito o custo de UI e acessibilidade para o MVP.
- **Web app + navegador:** rejeitado por captura, permissões, distribuição e controle de processo.

## Critérios de revisão

Reavaliar se o consumo do WebView impedir a meta de RAM ociosa, se uma necessidade de mídia exigir APIs indisponíveis nos WebViews alvo, ou se a manutenção de CSS cross-WebView se tornar desproporcional.

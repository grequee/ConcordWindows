# Melhorias de uso diário

Pedido atual: implementar todos os itens abaixo, preservando áudio, anexos sem
limite fixo por arquivo, Neon e compatibilidade básica do protocolo v1.

- [x] Chat: respostas citadas, edição/exclusão próprias, links e busca.
- [x] Leitura: marcador persistente, não lidas e notificações silenciáveis.
- [x] Anexos: arrastar, colar imagens, ampliar fotos, progresso/cancelar download.
- [x] Interface: detalhes técnicos recolhidos em diagnóstico.
- [x] Desktop: bandeja, estado do microfone e atalho configurável (Ctrl+Shift+M).
- [x] Testes, docs, cópia independente ConcordServer, executável e fontes ZIP.

Decisões: autoria estável por instalação, segredo nativo e hash público;
edição/autorização no servidor, nenhuma confiança no nome de exibição.
Atividade de mensagens por HTTP/WS da sala atual; marcadores locais por
servidor/sala. Notificação só em segundo plano. Preferências locais para
notificações, atalho e bandeja. Atalho global: Win32 no Windows, X11 no X11,
portal GlobalShortcuts no Wayland, com estado de disponibilidade explícito.

Sem commit/push/deploy automáticos. Credenciais reais não vão para fontes/ZIP.

## Como usar

- Passe o ponteiro sobre uma mensagem ou navegue com Tab para responder.
  Nas próprias mensagens aparecem editar e apagar; a exclusão tem confirmação.
  Links HTTP/HTTPS abrem no navegador. A lupa busca texto e nomes de anexos,
  com paginação do histórico persistido da sala atual.
- A leitura fica salva por servidor/sala neste app. O divisor marca onde parou;
  o contador considera mensagens de outras pessoas carregadas na sala. O botão
  de novas mensagens leva ao fim. Histórico anterior pode ser carregado por página.
- Notificações do sistema aparecem para novas mensagens da sala em que você
  entrou quando a janela está em segundo plano. Não há assinatura de todas as
  salas: salas que você não acompanha não geram avisos. Preferências permite
  silenciar notificações sem apagar os marcadores de leitura.
- Arraste arquivos para o chat ou cole imagens no campo da mensagem. Selecionar
  vários arquivos cria uma fila: cada envio usa o próximo arquivo. Não há limite
  fixo por arquivo; continua valendo o espaço total de armazenamento configurado.
- Clique na prévia de uma imagem para ampliar; Esc fecha. A prévia automática
  continua limitada a imagens pequenas para preservar memória. Arquivos grandes
  são enviados normalmente e podem ser baixados. O painel Downloads continua
  visível ao trocar de sala, com progresso, cancelar e caminho final; arquivos
  incompletos são removidos em erro/cancelamento e os existentes não são sobrescritos.
- Dados de codecs/pacotes/perdas ficam recolhidos em Diagnóstico na chamada.
  Avisos usam região de status sem mover o foco. Controles têm foco visível,
  nomes acessíveis e respeitam movimento reduzido.
- Em Preferências, selecione o campo do atalho e pressione a combinação desejada.
  Padrão Ctrl+Shift+M. São aceitos Ctrl/Alt/Meta com letra, número ou F1–F12,
  com Shift opcional. Pode desativar ou restaurar. Segurar a tecla não alterna
  repetidamente. O atalho só altera o microfone durante uma chamada.
- X11 usa grab de teclado; Wayland usa portal GlobalShortcuts e pode pedir
  autorização na janela do sistema. Windows usa RegisterHotKey. A disponibilidade
  é exibida: se o global falhar, o mesmo atalho funciona com o app em foco.
- Fechar mantém o app na bandeja quando habilitado e disponível. O menu permite
  abrir, mutar/desmutar e sair. Há botão explícito de minimizar à bandeja. A cor
  e o texto indicam microfone aberto/fechado; no chat há indicação por escrito.
  Linux sem StatusNotifierWatcher não esconde a janela ao fechar.

## Atualização do servidor

Publicar a pasta ConcordServer atualizada, depois abrir o app novo e reconectar.
`/healthz` agora informa `rich_chat: true`. A migração do SQL é automática na
partida e mantém mensagens/anexos existentes. A chave privada de autoria fica
somente em `chat-identity.json` no diretório de configuração nativo; no chat e
nos eventos vai somente seu hash. A edição/exclusão exige sessão válida e
participação na sala, além da chave correta. Nomes iguais não concedem autoria.
Mensagens anteriores à atualização só têm vínculo com a sessão antiga, portanto
podem não oferecer edição depois de reiniciar o app. Não há como recuperar essa
posse de forma segura apenas pelo nome.

## Validação e limites do ambiente

Testes automatizados de rotas/autoria, citações, exclusão, buscas, identidade,
ordem de edições, links, marcadores e atalhos. Check/clippy/formatação e build
frontend/desktop Linux. O SQL deve ainda ser exercitado no Neon real após deploy;
não há credenciais disponíveis nesta sessão. Validação visual, notificações,
bandeja/atalhos reais e Windows devem ser conferidos no desktop de destino.
Não foram feitos commit, push ou deploy pelo assistente.

Referências de acessibilidade: [WCAG 2.2 — mensagens de status sem mover o foco](https://www.w3.org/WAI/WCAG22/Understanding/status-messages.html)
e [foco visível para teclado](https://www.w3.org/WAI/WCAG22/Understanding/focus-visible.html).
A implementação usa essas orientações; não constitui auditoria completa de conformidade.

No Linux, a bandeja depende de libayatana-appindicator3 (ou libappindicator3)
e do host de StatusNotifier. Sem biblioteca/host, o recurso fica indisponível
sem impedir o uso normal do app. Os manifests DEB/RPM incluem a dependência.

Validação final desta revisão: 127 testes Rust aprovados no workspace (5 ignorados
por depender de hardware/ambiente), 56 testes no servidor independente, 6 testes
frontend executados, Svelte sem erros/avisos, Clippy sem warnings, formatação,
build frontend e release Tauri Linux. Conferência de tipos das novas chamadas
Win32/WinRT em harness Linux aprovada, sem equivaler a build Windows. Fontes ZIP
são exportadas por `python scripts/export-sources.py`, com allowlist explícita.

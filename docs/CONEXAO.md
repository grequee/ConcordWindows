# Entrada e recuperação automática da conexão

Implementado no cliente desktop em 31/10/2026. O servidor e o protocolo de
sinalização permanecem compatíveis com o deploy existente.

## Uso

- O formulário lembra o último **nome**, **servidor** e **convite**, incluindo depois de
  fechar e abrir o app. Usa o armazenamento local do WebView, como o endereço
  já fazia; se ele estiver indisponível, conectar continua funcionando.
- O convite é salvo junto com a URL no armazenamento local do WebView e volta
  preenchido, oculto como senha, depois de fechar o app ou desconectar. Trocar
  a URL não reaproveita o convite salvo para outro servidor. Essa persistência
  foi solicitada pelo usuário depois da implementação inicial de reconexão.
- Ao conectar, o app mostra progresso. Se o servidor demorar, mostra
  **Aguardando o servidor iniciar…**, tenta novamente e oferece **Cancelar
  conexão**. Não exige abrir `/healthz` manualmente.
- Depois de uma queda de rede, o app mostra **Reconectando…**, autentica
  novamente e tenta voltar à sala escolhida. Recebe uma identidade nova e
  atualiza participantes e histórico com a resposta atual do servidor.
- Voz e transmissão são encerradas na queda. A sala é recuperada, e o usuário
  retoma a chamada e o compartilhamento pelos controles existentes.
- Se a sala tiver sido removida ou estiver cheia, a conexão permanece ativa
  no lobby, com um aviso para escolher outra sala.
- **Cancelar conexão**, **Desconectar** e uma nova tentativa manual invalidam
  o worker anterior. Uma conexão antiga não pode sobrescrever a nova sessão.

Com Neon configurado no servidor atualizado, o histórico persiste e volta
ao recuperar a sala. Sem banco, o chat continua temporário e não recupera
mensagens perdidas num reinício. Veja [PERSISTENCIA.md](PERSISTENCIA.md).

## Política de espera

O worker nativo em `src-tauri/src/client/connection.rs` é o único responsável
pelo WebSocket, autenticação, heartbeat e retry. Não há loop de rede no Svelte.

| Situação | Política |
| --- | --- |
| Abertura do WebSocket | Até 20 s por tentativa |
| Indicação de servidor lento | Após 3 s aguardando a abertura |
| Primeira entrada | Até 3 minutos de espera total, com erro ao esgotar |
| Retry de rede | Intervalos de 1, 2, 4, 8 e até 15 s |
| Após uma sessão já autenticada | Retry até recuperar ou o usuário cancelar |
| Autenticação sem `Welcome` | Até 20 s; depois tenta uma nova conexão |
| Recuperação da sala sem resposta | Até 20 s; depois recupera a conexão novamente |
| Escrita travada | Até 10 s |
| Heartbeat | Ping de aplicação a cada 25 s |
| Socket aberto, servidor sem responder | Reconecta após 75 s sem receber dados |

Recusas de convite, protocolo incompatível e erros definitivos de URL/TLS
encerram as tentativas. Respostas temporárias como HTTP 408, 429 e 5xx permitem
retry. O app não repete mensagens de chat ou sinais WebRTC que ficaram na fila
do transporte anterior. A intenção de sala é guardada separadamente; sair da
sala a cancela mesmo antes de receber a confirmação do servidor.

As credenciais do worker não aparecem no snapshot nem nos logs de tentativas.
A pedido do usuário, o formulário salva o convite e sua URL em `concord.invite`
no armazenamento local do WebView, preenchendo o campo ao abrir novamente.
O nome e o endereço usam `concord.displayName` e `concord.serverUrl`.
Credenciais de banco, bucket e TURN continuam fora desse armazenamento.

## Validação

Foram aprovados 93 testes unitários do workspace (cinco ignorados), Clippy sem
warnings, formatação e check/build do frontend. Os 12 novos testes usam frames
WebSocket reais sobre streams em memória, com relógio virtual, sem exigir
permissão para abrir sockets ou esperar minutos:

- Servidor lento, mudança de estado e autenticação após retry.
- Convite inválido e protocolo incompatível sem repetição de autenticação.
- Queda, nova identidade e recuperação da sala.
- Saída de sala sem confirmação não causa retorno indesejado.
- Cancelamento e substituição de uma sessão pendente.
- Timeout antes de autenticar e detecção de socket sem resposta.
- Sala cheia/removida, limite inicial de espera e classificação de falhas HTTP.

```bash
cargo test -p concord-app --lib client::connection::tests
cargo test --workspace --lib
cargo clippy --workspace --all-targets -- -D warnings
npm run check
npm run build
```

Ainda é necessário testar no desktop real o servidor Render em estado frio,
desligar/ligar a rede com uma sala aberta e cancelar durante uma tentativa.
As restrições desta sessão não permitem afirmar que esses cenários WAN ou a
apresentação visual foram validados. Os instaladores antigos não foram
reconstruídos nesta revisão.

## Revisão com contas

O app atualizado abre no login, permite criar conta com convite e volta ao login
após o cadastro. Login salva uma sessão nativa e a retoma na próxima abertura.
HTTP de login/retomada aguarda o servidor com retry e cancelamento; o cadastro
é gravado uma vez, após uma consulta de aquecimento. A senha não é persistida.
Disconnect explícito agora sinaliza o supervisor e aguarda LeaveRoom/Close em
vez de somente abortar a tarefa. O servidor usa guard, timeout de atividade e
ID de transporte para limpar presença sem remover uma sessão nova da conta.
Veja [contas e encerramento](CONTAS.md).

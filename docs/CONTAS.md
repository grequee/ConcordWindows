# Contas e encerramento de sessão

Revisão de 31/10/2026. Cadastro/login substituem as identidades temporárias no
servidor com Neon configurado. Não há variáveis novas: usa DATABASE_URL existente.

## Fluxo do app

1. A tela inicial é **Login**. Quem já tem conta informa usuário e senha.
2. **Criar conta** abre o cadastro. Usuário de 3 a 32 caracteres (letras ASCII,
   números, ponto, hífen ou sublinhado), senha de 12 a 128 caracteres, confirmação
   da senha e convite da turma. O usuário não diferencia maiúsculas/minúsculas.
3. Cadastro bem-sucedido volta ao login com o usuário preenchido. Não cria uma
   sessão automaticamente. A pessoa entra uma vez com usuário e senha.
4. O login salva uma sessão no diretório nativo de configuração. Ao reabrir o
   app, ele valida a sessão no servidor e entra automaticamente. A senha não é
   gravada pelo app. Nome do usuário/servidor ficam lembrados na interface.
5. **Sair da conta** encerra a chamada/conexão, apaga a sessão local e tenta
   revogá-la no servidor. Na próxima abertura é necessário fazer login. Encerrar
   o programa pelo menu da bandeja preserva a sessão para a próxima abertura.

A sessão vale por 30 dias, pode ser revogada e continua válida após reiniciar o
Render. Se estiver expirada, o app volta ao login. Falhas de rede não apagam uma
sessão válida. A entrada aguarda o servidor por até três minutos; depois, o botão
Continuar como @usuário permite retomar sem digitar a senha. O convite só é exigido no cadastro.
Contas são por servidor/banco. Não há recuperação por email nem troca de senha
nesta revisão. Usuários e mensagens existentes não são atribuídos pelo nome:
contas novas recebem um ID permanente; as mensagens novas pertencem a essa conta
mesmo em outro computador. Mensagens anteriores continuam no histórico, sem
atribuição automática a contas novas.

## Correção do participante que permanecia na chamada

O app abortava a tarefa de conexão sem aguardar um encerramento WebSocket.
Agora sinaliza o supervisor, envia LeaveRoom e fecha o socket antes de terminar,
com espera limitada e abort apenas como fallback. Isso também vale ao substituir
uma conexão. A mídia é encerrada no fluxo de saída existente.

O servidor remove presença e sessão quando o socket fecha, o writer falha ou
fica 90 segundos sem atividade de aplicação. Ping/Pong do transporte não renova
sozinho esse prazo. Um guard limpa também cancelamento/aborto do handler.

Só uma conexão WebSocket da mesma conta fica ativa. Uma conexão nova remove a
presença antiga e avisa o cliente anterior com session_replaced fatal, evitando
um ciclo de reconexões. Cada transporte tem sua própria chave; a limpeza de uma
conexão antiga não pode derrubar a nova. Frames atrasados do socket antigo não
podem operar na sessão nova, e logout de um token antigo não encerra uma sessão
que entrou com outro token.

## Persistência e proteção das credenciais

- Neon: concord_accounts e concord_account_sessions, fora da limpeza do chat.
  Migração automática na inicialização; usuários já gravados não são descartados.
  Até 1000 contas e oito sessões de login por conta; sessões expiradas são limpas
  ao criar novas sessões. Apenas um transporte WebSocket por conta permanece ativo.
- Senhas: PBKDF2-HMAC-SHA256 com 600.000 iterações e salt aleatório de 32 bytes,
  via ring, comparado pela rotina de verificação da biblioteca. Trabalho fora
  do executor assíncrono, no máximo dois cálculos simultâneos. Sem regras de
  composição arbitrárias e sem cortar/normalizar a senha recebida.
- Tokens: 32 bytes aleatórios, somente o hash com domínio próprio no banco.
  Sessão salva em account-session.json nativo, com modo 0600 no Unix e escrita
  atômica. Operações em disco são serializadas mesmo se o pedido for cancelado,
  para uma gravação atrasada não desfazer um logout. Ela não aparece em snapshots, eventos nem localStorage. O arquivo
  e chat-identity.json não devem ser compartilhados.
- HTTPS para contas em servidores públicos; HTTP fica restrito à rede local
  explícita/loopback. O cliente não segue redirecionamentos com as credenciais.
  O tráfego HTTP local deve acontecer apenas em uma rede de confiança.
- Limites de tentativas por origem e falhas por origem/usuário. Erro de login
  genérico. A resposta para usuário inexistente também executa a derivação.
- O autor do chat vem da conta autenticada na sessão do servidor, sem confiar
  no usuário/nome/hash escolhido no cliente. Rotas de chat ainda exigem o token
  HTTP do transporte atual e presença na sala.

Referências: [OWASP — armazenamento de senhas](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html)
e [ring — PBKDF2](https://docs.rs/ring/0.17.14/ring/pbkdf2/index.html).

## Publicar e conferir

Publique o conteúdo atualizado de ConcordServer no repositório usado pelo Render.
O banco já configurado deve permitir criar as duas tabelas, índices e funções
concord_account_*. Após deploy, /healthz deve mostrar accounts:true. Nesse modo,
clientes antigos que entram só com convite devem ser atualizados para usar conta.
Sem banco, o modo legado por convite permanece para ferramentas/servidores locais;
a UI de contas informa que o banco precisa ser configurado.

Use o executável novo. Cadastre uma conta: deve voltar ao login sem entrar no
servidor. Faça login, entre na sala/call, use Sair da conta e entre novamente:
a presença anterior deve sumir e o ID da conta deve continuar o mesmo. Feche pelo
menu Sair da bandeja, reabra e confira a entrada automática sem senha. Em outro
computador, a entrada na mesma conta deve encerrar a conexão anterior.

Os testes automatizados cobrem cadastro sem sessão, login/identidade estável,
revogação/expiração, hash/salt, limites de tentativas, autoria entre transportes,
saída WebSocket em memória, limpeza de presença e guard atrasado. A migração SQL
e o fluxo visual ainda precisam ser exercitados no Neon/Render e no desktop real;
não há credenciais do provedor disponíveis ao assistente. Windows real permanece
por validar. Não houve commit, push ou deploy automático.

## Verificações desta revisão

141 testes Rust no workspace (5 ignorados por dependerem de hardware/ambiente),
64 no servidor independente, testes frontend, check Svelte sem erros/avisos,
formatação, Clippy sem warnings e builds de produção Linux. A verificação inclui
uma gravação de sessão cancelada que termina após o logout, e autenticação lenta
em um socket já fechado. O teste de encerramento usa WebSocket sobre memória,
sem depender de TCP/display. Não equivale a validação visual ou do provedor real.

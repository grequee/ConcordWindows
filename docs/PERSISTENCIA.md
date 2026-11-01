# Mensagens e anexos no Neon Free

Implementação de 31/10/2026. O servidor grava texto e metadados no Postgres
do Neon e anexos de qualquer tipo em um bucket privado. O app confirma o envio depois da
gravação; se falhar, mantém o texto e o arquivo selecionado para tentar novamente enquanto a sala estiver aberta.
Repetir a mesma tentativa não duplica a mensagem persistida.

Os limites anunciados do Free nesta data são **1 GB de banco por projeto** e
**5 GB de Object Storage**. Confira o painel da conta se o plano mudar.
Fontes: [preços](https://neon.com/pricing),
[configuração do storage](https://neon.com/docs/storage/get-started) e
[credenciais S3](https://neon.com/docs/storage/authentication).

## Ativar no Render

1. Publique os arquivos atualizados da pasta `ConcordServer/` no repositório
   que seu serviço Render já usa. Esta implementação não fez commit, push nem
   deploy. Essa pasta compila de forma independente do aplicativo.
2. No Neon, selecione o banco e a branch de produção. Em **Connect**, copie a
   connection string PostgreSQL com usuário, senha e `sslmode=require`.
   O endereço com `-pooler` também funciona.
3. No storage dessa mesma branch, use um bucket **privado**, de preferência
   dedicado ao Concord. Anote o nome e o endpoint S3. Crie uma credencial
   com escopo `storage:write`, que permite ler, gravar e excluir objetos.
   O servidor não cria o bucket.
4. No serviço existente do Render, abra **Environment** e acrescente:

| Variável | Valor a copiar do Neon |
| --- | --- |
| `DATABASE_URL` | Connection string PostgreSQL completa |
| `AWS_ENDPOINT_URL_S3` | Origem HTTPS do endpoint da branch, sem nome do bucket no caminho |
| `AWS_REGION` | Região S3 fornecida pelo Neon; exemplo: `us-east-2` |
| `AWS_ACCESS_KEY_ID` | Campo `token_id`, como `nak_live_...` |
| `AWS_SECRET_ACCESS_KEY` | Campo `s3_secret_access_key`, como `nsk_live_...` |
| `CONCORD_STORAGE_BUCKET` | Nome exato do bucket existente |

Não coloque esses valores no app, no GitHub ou na conversa. O `api_token`
administrativo do Neon não substitui a chave secreta S3. As variáveis do
convite e do STUN que já existem no Render continuam necessárias.

5. Salve e publique a versão atualizada do servidor. Na primeira inicialização,
   ele cria as tabelas e funções `concord_chat_*` usando o SQL versionado em
   `crates/signaling-server/src/storage/schema.sql`. O usuário do banco
   precisa de permissão para criá-las no schema padrão.
6. Confira `/healthz`: `chat_persistent: true`, `image_uploads: true` e `attachment_uploads: true`
   indicam que os recursos foram configurados. A revisão de chat completo também informa `rich_chat: true`. Esse endpoint não executa
   um teste de leitura/escrita do bucket a cada chamada.
7. Abra o **app atualizado**, reconecte e entre na sala. Use o botão de anexo
   ao lado da caixa de mensagem. Envie texto e um arquivo; reinicie o serviço
   Render e reconecte o app para conferir se ambos reaparecem.

Com apenas `DATABASE_URL`, texto persiste e o botão de anexo fica oculto.
Sem banco nem variáveis de bucket, o modo temporário em memória continua.
Configuração parcial ou falha ao inicializar um banco configurado impede
a partida, evitando uma troca silenciosa para memória. Mensagens que existiam
só na memória antes desta atualização não são migradas automaticamente.

## Espaço e exclusão automática

Os padrões deixam margem em relação ao plano Free:

| Variável opcional | Padrão | O que limita |
| --- | --- | --- |
| `CONCORD_CHAT_MAX_BYTES` | `134217728` | 128 MiB estimados: JSON + reserva de 4 KiB por mensagem |
| `CONCORD_CHAT_MAX_MESSAGES` | `20000` | Total de mensagens de todas as salas |
| `CONCORD_BUCKET_MAX_BYTES` | `4000000000` | 4 GB decimais de anexos gerenciadas pelo Concord |

Ao ultrapassar um orçamento, remove as mensagens mais antigas de todas as
salas até admitir a nova, incluindo seus anexos. A gravação e essa retenção
acontecem numa transação; falha na transação desfaz as exclusões planejadas.
Os apps conectados recebem os IDs exatos removidos.

Se o provedor responder explicitamente que está sem espaço, há novas tentativas
com limpeza limitada a três mensagens por etapa. Para liberar espaço no
bucket, a recuperação procura primeiro a mensagem mais antiga com arquivo.
Erros de rede, senha, permissão, limite de requisições ou respostas desconhecidas
não acionam essa limpeza de emergência.

A exclusão de objetos usa uma fila persistente. Se falhar, tenta de novo na
próxima gravação e a cada cinco minutos. Limpeza pendente impede novas
exclusões em sequência quando o orçamento precisa de espaço. Uploads
interrompidos são registrados; órfãos são encaminhados para limpeza depois
de uma hora após reiniciar o servidor, ou após 30 minutos sem atividade no processo atual. A limpeza de arquivos em partes registra o progresso no banco para continuar após falhas. Só acessa tabelas `concord_chat_*` e chaves `concord-images/`.

**Excluir não garante liberação imediata da cota física.** O Neon documenta
`DeleteObject` como exclusão lógica na branch; o Postgres também reutiliza
espaço e pode manter páginas/histórico. A margem e a retenção antecipada
reduzem o risco. Se o provedor continuar bloqueando, o app informa o erro e
mantém o rascunho, sem apagar todo o histórico numa tentativa sem fim.
Veja [compatibilidade S3](https://neon.com/docs/storage/s3-compatibility).

O orçamento acompanha dados do Concord, não outros arquivos, tabelas,
branches ou histórico do projeto. Se compartilhar o projeto, reduza os limites
conforme o espaço restante. Não há `VACUUM FULL`, remoção de branches ou
limpeza geral do bucket automática.

## Arquivos, histórico e acesso

- **Qualquer tipo de arquivo**, incluindo PDF, ZIP, áudio, vídeo, executável,
  arquivo sem extensão e imagens. O arquivo original é preservado.
- **Sem limite fixo de tamanho por arquivo no novo fluxo.** Envia em blocos
  de 1 MiB e baixa em blocos para Downloads. O arquivo inteiro não é carregado
  em memória nem enviado em um JSON gigante. Um bloco não é o tamanho máximo
  do arquivo. O espaço total configurado para o chat e a cota do Neon continuam
  valendo: o padrão reserva 4 GB para todos os anexos. Arquivo maior que todo
  esse orçamento é recusado antes de apagar mensagens antigas.
- A seleção mostra nome e tamanho, e o envio mostra progresso. Tentar de novo
  com o mesmo arquivo e legenda, sem sair da sala, retoma as partes recebidas
  quando a sessão de upload ainda existe. Reiniciar o servidor descarta a sessão
  de transferência, exigindo reenvio; mensagens já confirmadas persistem.
- PNG, JPEG e WebP de até 2 MiB também recebem prévia no histórico. Arquivos
  maiores são aceitos e aparecem como anexos para download. Esse valor limita
  apenas a prévia automática e o endpoint legado, não o upload novo.
- O botão de download salva em Downloads sem abrir o arquivo automaticamente.
  Nomes são normalizados para Linux/Windows; arquivos existentes recebem um
  sufixo numérico e não são sobrescritos. Erros de transferência removem o
  arquivo parcial. Arquivos nunca são executados pelo app.
- Histórico em páginas de até 40 mensagens, com botão para buscar anteriores.
  Imagens do histórico são baixadas perto da área visível.
- Chaves do banco e bucket ficam no servidor. O processo nativo usa um token
  temporário da sessão WebSocket, fora do snapshot da interface e do disco.
- HTTP exige sessão autenticada e participação na sala. Binários são servidos
  como download com `application/octet-stream`, sem links públicos.
- Atualize servidor e app para o novo fluxo. Clientes antigos continuam com
  texto e imagens do protocolo anterior; não transferem anexos novos.
  Nenhuma variável nova no Render é necessária; mantenha as seis existentes.

## Correção de “Loading error” ao selecionar imagem

A prévia local usava `blob:`, mas a CSP de produção permitia só `self` e
`data:` para imagens. A configuração agora inclui `blob:` em `img-src` e a
seleção não depende mais de decodificar e converter o arquivo inteiro.
A política continua restrita às imagens locais e ao IPC existente.
Referência: [CSP img-src](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/img-src).

## Validação

Os testes locais cobrem confirmação de envio, deduplicação, limites, limpeza
de anexos, falha de exclusão e recuperação, erros que não devem apagar dados,
autorização HTTP, tamanho de requisições e proteção de credenciais no IPC.
Check/build do frontend, testes unitários e Clippy verificam o workspace.

Nesta revisão passaram **115 testes unitários** no workspace (cinco testes
de hardware/display ignorados), **55** no servidor independente e três testes
do frontend para CSP, leitura de partes binárias e erros de leitura. Clippy passou sem warnings nos
dois workspaces; frontend passou com zero erros/avisos e gerou a versão de
produção. O servidor independente também compilou em release.
O app Linux foi recompilado pelo Tauri em modo de produção, com a interface
embutida, em `target/release/concord-app`. Instaladores anteriores não foram
reconstruídos. O ZIP de código-fonte Windows foi atualizado; build Windows
e validação nessa plataforma continuam pendentes.

O usuário publicou a revisão anterior e confirmou os recursos habilitados no
health. **A nova transferência de anexos precisa de deploy e teste real conforme
o passo 7**; a assinatura S3, o fluxo visual e arquivos grandes na conta real
não foram validados nesta sessão. Os testes de storage
usam serviços simulados e não comprovam liberação física de cota. O arquivo
`.env.example` contém somente nomes e exemplos.

## Respostas, edição, exclusão e busca

A revisão de usabilidade adiciona coluna `owner_id` e funções SQL de composição
segura de citações, edição e exclusão. A migração roda na inicialização. Nenhuma
variável de ambiente nova é necessária. A posse usa hash de chave aleatória
mantida somente no app; o header privado não é devolvido por nenhuma rota.
Mensagens antigas sem owner_id continuam ligadas à sessão original; não se
atribui autoria pelo nome. Rotas exigem sessão HTTP ativa e presença na sala.

PATCH/DELETE `/api/rooms/{room}/messages/{id}` alteram somente mensagens próprias;
GET desse caminho lê uma mensagem; GET `.../messages/search?q=...&before=...`
busca trechos literais de texto/nome de arquivo (40 resultados por página).
Respostas enviam `reply_to`. Citações são resolvidas na leitura a partir do ID,
sem manter uma cópia permanente do texto excluído em outras mensagens. Edições
publicam updated_messages; exclusões publicam deleted_messages no WebSocket,
e ambos atualizam o cache local. Downloads/limpeza mantêm o esquema em partes.
A exclusão enfileira o anexo na limpeza durável existente. Editar não apaga
mensagens alheias para abrir espaço; se a edição exceder o orçamento, falha.

## Contas de usuário

A revisão de contas usa o mesmo DATABASE_URL para cadastro/login persistentes,
sem configuração extra de bucket nem remoção automática de contas. /healthz
informa accounts:true. Clientes com banco configurado precisam usar a versão
com login. Veja [cadastro, sessão salva e saída da chamada](CONTAS.md).

# ADR 0003 — Acesso somente por token de convite

- **Status:** aceita
- **Data:** 2026-10-25
- **Marco:** M1
- **Substitui:** —

## Contexto

Concord é para um grupo fechado. Cadastro aberto, e-mail, recuperação de senha e diretório público criariam complexidade de identidade e abuso que não são o produto. O operador do servidor conhece pessoalmente quem deve entrar.

O servidor é auto-hospedado, muitas vezes em uma LAN, e não deve depender de um provedor de identidade externo.

## Decisão

- Cada convite é um segredo compartilhado de longa duração, definido no TOML do servidor.
- Não há cadastro, senha, e-mail ou descoberta pública.
- O cliente envia `display_name`, `invite_token` e metadados em `hello`.
- O servidor compara o token em tempo constante (`subtle::ConstantTimeEq`) com todos os tokens configurados, sem early return.
- Tokens precisam ter 16–256 caracteres ASCII imprimíveis, sem espaços nas bordas e sem duplicatas.
- Falhas de autenticação são contadas por IP; cinco falhas em 60 s bloqueiam o IP por 60 s.
- Tokens são redigidos em `Debug`, não são enviados à UI e não são persistidos pelo cliente.
- TLS é obrigatório em qualquer rede não confiável; o servidor em si escuta WS atrás do proxy.

## Consequências

**Boas**

- Modelo mental simples: quem tem token entra; quem não tem, não entra.
- Nenhum banco de usuários ou fluxo de recuperação no M1.
- O operador tem controle direto sobre quem recebe convite.

**Custos e riscos**

- Token é um segredo de longa duração: se vazar, continua válido até o operador editar o TOML e reiniciar.
- Não há revogação individual em runtime, expiração ou limite de uso por token.
- Todos os convites são equivalentes; não existem papéis ou escopos.
- O bloqueio por IP é local e pode afetar pessoas atrás do mesmo NAT.
- Tokens em texto claro no arquivo exigem permissão `0600` e cuidado com backup.

## Evolução planejada

- Expiração opcional e identificador de convite para auditoria.
- Revogação sem reiniciar, se houver necessidade operacional real.
- Rotação documentada e comando de validação de configuração.
- Possível integração com proxy de identidade no futuro, sem alterar o modelo fechado.

## Alternativas consideradas

- **Cadastro e senha:** mais superfície, recuperação, hashing e suporte; desnecessário para grupo fechado.
- **OAuth/Discord:** contradiz a independência do servidor e adiciona dependência externa.
- **Token de uso único:** melhor contra replay, mas péssimo para amigos que reinstalam o app; pode ser um modo futuro, não o padrão.
- **mTLS:** forte, mas custo de distribuição e renovação de certificados para usuários não técnicos é alto demais no MVP.

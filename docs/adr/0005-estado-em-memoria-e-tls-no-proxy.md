# ADR 0005 — Estado em memória e TLS no proxy reverso

- **Status:** aceita
- **Data:** 2026-10-25
- **Marco:** M1
- **Substitui:** —

## Contexto

O MVP precisa entregar salas, chat e presença rapidamente, com uma superfície operacional pequena. Também precisa de TLS para proteger tokens e sinalização, mas implementar certificados e ACME dentro do binário Rust adicionaria complexidade e responsabilidades que um proxy já resolve melhor.

## Decisão

### Estado

- Salas, presença, histórico e sessões vivem em memória no processo do servidor.
- Reiniciar o servidor limpa tudo. O cliente reconecta manualmente no M1.
- Configuração de salas e convites é um arquivo TOML lido na inicialização.
- Não há banco, fila ou cache externo.

### TLS

- O binário escuta WS em `127.0.0.1:8787` ou `0.0.0.0:8787` conforme configuração.
- Em produção, um proxy (Caddy ou nginx) termina TLS e encaminha `/ws` e `/healthz` para o servidor.
- O cliente aceita `wss://` e valida o certificado do sistema.
- O exemplo de deploy usa Caddy com certificado automático e HSTS.
- HTTP puro é aceito apenas para desenvolvimento e LAN controlada; a UI/documentação não recomendam exposição pública.

## Consequências

**Boas**

- Operação previsível: um binário, um arquivo, um proxy.
- Atualização de certificado, HSTS e headers ficam no proxy.
- Chat e presença respondem sem latência de banco.
- Nenhum schema/migração no M1.

**Custos**

- Perda de histórico e sessões no restart.
- Sem escala horizontal: duas instâncias não compartilham estado.
- O cliente precisa reconectar sozinho no futuro; no M1 é manual.
- O proxy é parte crítica da segurança; mal configurado, o app fica exposto.
- `healthz` reporta o estado local, não a saúde do proxy/TURN.

## Gatilhos para revisar

- Necessidade real de histórico persistente ou múltiplas instâncias.
- Uso por grupos que não conseguem operar Caddy/nginx.
- Requisito de TLS mútuo ou certificados gerenciados pelo app.
- Crescimento para mais de 8 pessoas ou múltiplas salas com tráfego relevante.

## Alternativas consideradas

- **SQLite desde o início:** resolveria histórico, mas adiciona schema, migração, backup e concorrência sem requisito atual.
- **Redis/Postgres:** escala e durabilidade que o MVP não precisa.
- **TLS embutido com rustls + ACME:** reduz dependências de proxy, mas aumenta a superfície e a complexidade operacional; reavaliar se houver demanda por um servidor “turnkey”.

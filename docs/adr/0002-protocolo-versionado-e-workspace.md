# ADR 0002 — Protocolo versionado e fronteiras de crates

- **Status:** aceita
- **Data:** 2026-10-25
- **Marco:** M0/M1
- **Substitui:** —

## Contexto

Cliente e servidor evoluirão juntos, mas não necessariamente com a mesma versão instalada. Mensagens de sinalização são pequenas, legíveis e precisam ser depuráveis. Ao mesmo tempo, o domínio não deve depender de Axum, Tauri ou WebRTC.

## Decisão

- Definir um envelope JSON com `v`, `request_id` opcional e `message` com tag `type`.
- Fixar `PROTOCOL_VERSION = 1`; incompatibilidade de versão é erro explícito e fatal.
- Validar tamanho e forma **antes** do domínio: `MAX_FRAME_BYTES = 128 KiB`, limites de nome, chat, SDP, ICE e IDs.
- Emitir `UserId` no servidor, nunca aceitar identidade escolhida pelo cliente.
- Separar o workspace em:
  - `protocol`: tipos e validação do fio;
  - `core`: regras síncronas de salas/chat/presença, sem I/O;
  - `signaling-server`: transporte, auth e sessões;
  - `app`: Tauri e cliente nativo;
  - `media`/`rtc`: contratos e implementação futura.
- A UI conhece apenas o snapshot e comandos; não importa tipos de rede do servidor.

## Consequências

**Boas**

- Dois clientes e servidor compartilham a mesma definição; drift de schema aparece em compilação/testes.
- O domínio é rápido de testar e não precisa de async ou sockets.
- Um erro de protocolo tem código e mensagem estáveis, exibíveis na UI.
- A camada de signaling pode evoluir sem reescrever regras de sala.

**Custos**

- Toda mudança de mensagem exige atualizar protocolo, validação e testes.
- JSON é maior que binário; para sinalização isso é irrelevante, mas não serve para mídia.
- A versão única ainda não oferece negociação de features; um campo novo opcional é a única evolução compatível até existir capability negotiation.

## Regras práticas

- Adicionar campos opcionais com `#[serde(default)]` é compatível; remover/renomear exige subir a versão.
- Nunca colocar token em erro, `Debug` ou log; `Invite` tem `Debug` redigido.
- Testes de integração usam o protocolo público, não atalhos internos do servidor.
- Mídia nunca entra nesse envelope.

## Alternativas consideradas

- **Protobuf/MessagePack:** mais eficiente, pior depuração manual no M1; JSON atende com folga.
- **Um crate único:** mais simples no início, mas acoplaria domínio, rede e UI e dificultaria testes.
- **Negociação de features desde o início:** complexidade prematura para dois lados implantados juntos.

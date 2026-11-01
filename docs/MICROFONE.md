# Áudio: dispositivos, teste de voz e volume por participante

O botão **Áudio**, no rodapé da barra lateral, abre o painel antes de conectar
ou durante a chamada. Abrir o painel consulta os dispositivos disponíveis;
a captura começa somente ao testar a voz ou entrar na chamada.

## Escolher microfone e fone

1. Abra **Áudio** e escolha **Microfone** e **Fone ou alto-falante**.
2. **Padrão do sistema** usa o dispositivo padrão da respectiva direção.
3. Clique em **Aplicar dispositivos**. A escolha fica salva para as próximas
   chamadas e também depois de fechar e abrir o app.
4. Durante a chamada, use os mesmos controles. A troca prepara apenas as
   direções alteradas e mantém as conexões WebRTC, presença e transmissão.
   Se não conseguir abrir o novo dispositivo, conserva a seleção ativa anterior.
5. Ao conectar um headset novo, use **Atualizar lista**. Uma seleção salva que
   não estiver disponível é indicada no seletor para você escolher outra.

A configuração fica em `audio-devices.json` na pasta de configurações do app,
normalmente `~/.config/chat.concord.desktop/` no Linux. Só contém IDs de entrada
/saída; não contém gravações. `CONCORD_AUDIO_INPUT` e `CONCORD_AUDIO_OUTPUT`
continuam como configuração inicial quando esse arquivo ainda não existe.

## Testar a própria voz antes da chamada

1. Escolha os dispositivos, o ganho e a redução de ruído no painel.
2. Clique em **Testar voz**: grava cinco segundos e depois reproduz nos fones
   escolhidos. O indicador mostra nível, tempo restante e eventual saturação.
3. **Parar teste**, fechar o painel ou entrar na chamada encerra o teste.

O teste usa as escolhas atuais do painel, inclusive antes de salvá-las.
As amostras ficam apenas num buffer limitado em memória, são descartadas ao
encerrar e não são enviadas ao servidor nem aos participantes. O teste fica
indisponível durante a chamada; nela, o painel mostra o nível transmitido.

## Ganho e ruído

- **Volume de entrada:** 0% a 200%, padrão 100%. Zero silencia o PCM; acima
  de 100% amplifica o sinal. O volume do sistema operacional não é alterado.
- **Supressão de ruído:** SpeexDSP opcional, desativado por padrão, para ruído
  constante. Não implementa cancelamento de eco nem ganho automático.
- **Salvar ganho e ruído:** aplica imediatamente na chamada e persiste em
  `microphone.json`. Os ajustes anteriores do usuário são preservados.
- **Nível enviado:** atualizado com metadados de atividade, sem PCM no IPC.
  O nível fica zerado no mudo; a saturação sugere diminuir o ganho.

O processamento continua nativo:

```text
CPAL → mono 48 kHz → supressão opcional → ganho → limite ±1 → Opus → WebRTC
```

## Quem está falando e volume individual

Na lista da sala, quem está falando recebe contorno no avatar e a indicação
**falando agora**. A detecção usa o nível real do áudio antes do volume individual,
com histerese e retenção de cerca de 260 ms entre quadros. Quadros sintetizados
para perda de pacotes não iniciam uma nova indicação de fala. Os indicadores
remotos aparecem quando você está na mesma chamada e recebe seu áudio.

Cada participante remoto da chamada tem controle **Para você**, de 0% a 200%.
Zero silencia essa pessoa na sua reprodução; 100% mantém o nível original.
Isso não altera o áudio transmitido nem o que os demais participantes ouvem.
Os volumes valem para a chamada atual e sobrevivem à reconstrução de um peer
com o mesmo ID; uma nova chamada começa em 100%. Mudanças de ganho fazem uma
rampa de um quadro de 20 ms, e a mistura mantém o limitador existente.

## Ciclo de vida e desempenho

- A abertura de dispositivos ocorre fora do ator da chamada, numa tarefa
  bloqueante. O ator continua processando a chamada enquanto prepara a troca.
- A gravação das escolhas em disco termina antes da instalação dos novos
  streams. Uma falha ao salvar descarta os substitutos e mantém os anteriores.
- A troca de uma direção não tenta reabrir a direção inalterada, importante
  para dispositivos com acesso exclusivo.
- Atividade de voz usa o evento pequeno `concord:audio-activity` a cada 100 ms,
  identificado por chamada. Não aumenta a frequência dos snapshots completos
  nem leva áudio ao WebView, servidor ou banco.
- `concord:audio-test` envia apenas fase, nível e tempo. O teste abre e fecha
  seus próprios streams e não anuncia presença na chamada.
- Nenhuma atualização do Render ou do protocolo de sinalização é necessária.

## Validação

Nesta revisão passaram **123 testes unitários**, com cinco testes dependentes
de hardware/display ignorados, Clippy com `-D warnings`, formatação e
check/build do frontend. Os testes cobrem, entre outros pontos: troca de apenas
uma direção, liberação dos streams preparados em caso de falha, persistência
sem sobrescrever configuração válida com entrada inválida, gravação antes da
reprodução, ganho no teste, cancelamento, falha de dispositivo, histerese de
atividade e ganho individual com silêncio imediato em zero.

O executável Linux foi recompilado com interface embutida. Instaladores antigos
não foram reconstruídos; o código-fonte Windows foi atualizado, mas o build
Windows e a validação com dispositivos reais ainda estão pendentes. Esta sessão
não validou captura, reprodução, troca física de headset nem o visual no desktop.
Os testes usam dados e dispositivos simulados para essas transições.

Para conferir no desktop real: teste a voz antes de conectar, troque entrada e
saída durante uma chamada com outra pessoa, teste volumes individuais 0/50/100/200%,
mudo/sem áudio, destaque de fala e reinício do app para conferir a seleção salva.
Fones continuam recomendados porque cancelamento de eco não foi implementado.

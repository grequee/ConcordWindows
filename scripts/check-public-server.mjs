// Node.js 24+: checks public HTTPS + authenticated WSS, without opening audio.
// The invitation is read from the environment and never printed.
const argument = process.argv[2];
const token = process.env.CONCORD_INVITE_TOKEN;

async function main() {
  if (!argument || !token) {
    throw new Error('Uso: CONCORD_INVITE_TOKEN=... node scripts/check-public-server.mjs https://seu-servico.onrender.com');
  }
  const base = new URL(argument.includes('://') ? argument : `https://${argument}`);
  if (!['https:', 'http:', 'wss:', 'ws:'].includes(base.protocol) || base.username || base.password) {
    throw new Error('Informe a URL pública do servidor, sem usuário ou senha.');
  }
  const secure = ['https:', 'wss:'].includes(base.protocol);
  const health = new URL('/healthz', base);
  health.protocol = secure ? 'https:' : 'http:';
  const deadline = Date.now() + 90_000;
  let response;
  // A free Render service may be waking up. Only retry transient HTTP errors.
  do {
    try {
      response = await fetch(health, { signal: AbortSignal.timeout(10_000) });
      if (response.ok) break;
      if (![502, 503, 504].includes(response.status)) {
        throw new Error(`Health check respondeu HTTP ${response.status}.`);
      }
    } catch (error) {
      if (Date.now() + 3000 >= deadline || error.message.startsWith('Health check')) throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 3000));
  } while (Date.now() < deadline);
  if (!response?.ok) throw new Error('O servidor não ficou disponível em 90 segundos. Confira os logs do Render.');
  const status = await response.json();
  if (status.status !== 'ok') throw new Error('A URL respondeu, mas o health check não é do Concord.');
  console.log('Health check: OK');

  const wsUrl = new URL(base);
  wsUrl.protocol = secure ? 'wss:' : 'ws:';
  if (wsUrl.pathname === '/') wsUrl.pathname = '/ws';
  await new Promise((resolve, reject) => {
    const socket = new WebSocket(wsUrl);
    const timer = setTimeout(() => finish(new Error('Timeout no WebSocket/autenticação.')), 15_000);
    let finished = false;
    function finish(error) {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      socket.close();
      if (error) reject(error); else resolve();
    }
    socket.addEventListener('open', () => {
      socket.send(JSON.stringify({ v: 1, message: {
        type: 'hello', display_name: 'Diagnóstico', invite_token: token,
        client: { name: 'concord-connectivity-check', version: '0.1.0', os: process.platform, arch: process.arch },
      } }));
    });
    socket.addEventListener('error', () => finish(new Error('Falha na conexão WebSocket.')));
    socket.addEventListener('close', () => finish(new Error('WebSocket encerrado antes da autenticação.')));
    socket.addEventListener('message', ({ data }) => {
      try {
        const frame = JSON.parse(String(data));
        const message = frame.message;
        if (message?.type === 'error') {
          finish(new Error(`Servidor rejeitou a conexão (${message.code ?? 'erro'}). Confira o convite.`));
        } else if (message?.type === 'welcome') {
          if (frame.v !== 1) throw new Error('Versão de protocolo incompatível.');
          const servers = message.ice_servers ?? [];
          const turn = servers.some((server) => server.urls.some((url) => /^turns?:/.test(url)));
          console.log(`WSS/autenticação: OK; salas: ${message.rooms.length}; servidores ICE: ${servers.length}`);
          console.log(`TURN: ${turn ? 'configurado' : 'ausente — chamadas em redes restritivas ainda podem falhar'}`);
          if (process.env.CONCORD_EXPECT_TURN === 'true' && !turn) {
            finish(new Error('Era esperado um TURN, mas o servidor não enviou essa configuração.'));
          } else {
            finish();
          }
        }
      } catch (error) { finish(error); }
    });
  });
  console.log('Conectividade de sinalização validada. Voz/tela ainda exigem teste entre duas redes reais.');
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});

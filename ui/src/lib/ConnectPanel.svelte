<script lang="ts">
  import { describeError, isTauri, getSavedAccount, type AccountRequest } from './api';
  import type { UiError, AccountProfile } from './types';
  import Icon from './Icon.svelte';
  interface Props {
    connecting: boolean;
    connectionMessage: string | null;
    connectionAttempt: number;
    reconnecting: boolean;
    serverError: UiError | null;
    externalError: string | null;
    onauthenticate: (request: AccountRequest) => Promise<AccountProfile | null>;
    onclearerror: () => void;
    oncancel: () => Promise<void>;
  }
  let { connecting, connectionMessage, connectionAttempt, reconnecting, serverError, externalError,
    onauthenticate, onclearerror, oncancel }: Props = $props();
  function initial(key: string, fallback = ''): string { try { return localStorage.getItem(key) ?? fallback; } catch { return fallback; } }
  let serverUrl = $state(initial('concord.serverUrl', import.meta.env.VITE_CONCORD_SERVER_URL ?? ''));
  let username = $state(initial('concord.username'));
  let password = $state('');
  let confirmation = $state('');
  let registering = $state(false);
  let inviteToken = $state('');
  let revealPassword = $state(false);
  let submitting = $state(false);
  let cancelling = $state(false);
  let localError = $state<string | null>(null);
  let success = $state<string | null>(null);
  let passwordInput = $state<HTMLInputElement>();
  let savedAccount = $state<AccountProfile | null>(null);
  const desktop = isTauri();
  const busy = $derived(submitting || connecting);
  const displayedError = $derived(localError ?? externalError ?? serverError?.message ?? null);
  const inviteServer = $derived(serverUrl.trim());
  $effect(() => {
    const server = inviteServer;
    const waiting = connecting;
    let alive = true;
    if (desktop && !waiting) {
      void getSavedAccount(server).then(info => {
        if (!alive) return;
        savedAccount = info?.account ?? null;
        if (!username.trim() && info) username = info.account.username;
      }).catch(() => { if (alive) savedAccount = null; });
    }
    return () => { alive = false; };
  });
  $effect(() => {
    try { const saved = JSON.parse(localStorage.getItem('concord.invite') ?? 'null'); inviteToken = saved?.serverUrl === inviteServer && typeof saved?.token === 'string' ? saved.token : ''; }
    catch { inviteToken = ''; }
  });
  function changeMode() {
    registering = !registering; password = ''; confirmation = ''; localError = null; success = null; revealPassword = false; onclearerror();
  }
  async function submit(event: SubmitEvent) {
    event.preventDefault(); if (busy) return;
    localError = null; success = null; onclearerror();
    if (!serverUrl.trim()) { localError = 'Informe o endereço do servidor.'; return; }
    if (!username.trim()) { localError = 'Informe seu nome de usuário.'; return; }
    if (!password) { localError = 'Informe sua senha.'; return; }
    if (registering) {
      if (!/^[a-zA-Z0-9._-]{3,32}$/.test(username.trim())) { localError = 'Use de 3 a 32 caracteres: letras, números, ponto, hífen ou sublinhado.'; return; }
      if (Array.from(password).length < 12) { localError = 'Escolha uma senha com pelo menos 12 caracteres.'; return; }
      if (password !== confirmation) { localError = 'As senhas não são iguais.'; return; }
      if (!inviteToken.trim()) { localError = 'Informe o convite recebido para criar sua conta.'; return; }
    }
    submitting = true;
    const wasRegistration = registering;
    try {
      try { localStorage.setItem('concord.serverUrl', serverUrl.trim()); localStorage.setItem('concord.username', username.trim().toLowerCase());
        if (registering) localStorage.setItem('concord.invite', JSON.stringify({ serverUrl: serverUrl.trim(), token: inviteToken.trim() }));
      } catch { /* Authentication still works if WebView storage is unavailable. */ }
      const account = await onauthenticate({ server_url: serverUrl.trim(), action: wasRegistration ? 'register' : 'login', username: username.trim(), password, ...(wasRegistration ? { invite_token: inviteToken.trim() } : {}) });
      if (wasRegistration && account) { username = account.username; registering = false; success = 'Conta criada. Entre com seu usuário e senha para continuar.'; }
    } catch (cause) { localError = describeError(cause); }
    finally { password = ''; confirmation = ''; revealPassword = false; submitting = false; if (wasRegistration && !registering) requestAnimationFrame(() => passwordInput?.focus()); }
  }
  async function cancel() { cancelling = true; try { await oncancel(); } catch (cause) { localError = describeError(cause); } finally { cancelling = false; } }
  async function resume() {
    if (busy) return;
    submitting = true; localError = null; success = null; onclearerror();
    try { await onauthenticate({ server_url: serverUrl.trim(), action: 'resume' }); }
    catch (cause) { localError = describeError(cause); }
    finally { submitting = false; }
  }
  function dismissError() { localError = null; onclearerror(); }
</script>
<section class="connect-card">
  <div class="card-head"><span class="mono-label">{registering ? 'Cadastro por convite' : 'Sua conta, seu lugar'}</span><span class="signal-lights" aria-hidden="true"><i></i><i></i><i></i></span></div>
  <div class="intro"><h2 class="display">{registering ? 'Faça parte da conversa.' : 'Bom ter você por aqui.'}</h2><p>{registering ? 'Crie sua conta uma vez. Seu usuário acompanha você nas próximas entradas.' : 'Entre com sua conta. O Concord lembra sua sessão neste computador.'}</p></div>
  {#if !desktop}<div class="browser-note" role="note"><Icon name="server" size={16} /><span>Para entrar, abra o aplicativo Concord.</span></div>{/if}
  {#if connecting || submitting}
    <div class="connection-progress" role="status" aria-live="polite" aria-atomic="true"><span class="waiting-spinner" aria-hidden="true"></span><span class="mono-label">{reconnecting ? 'Voltando à conversa' : registering ? 'Criando sua conta' : 'Entrando na sua conta'}</span><strong>{connectionMessage ?? 'Aguarde um momento…'}</strong><p>{reconnecting ? 'Sua sala será recuperada quando a conexão voltar. Retome a chamada pelos controles de voz.' : 'Se o servidor estiver iniciando, a entrada pode levar um pouco mais de tempo.'}</p><span class="attempt">Tentativa {Math.max(1, connectionAttempt)}</span></div>
    <button class="cancel-connect" type="button" onclick={cancel} disabled={cancelling}>{cancelling ? 'Cancelando…' : 'Cancelar'}</button>
  {:else}
    <form onsubmit={submit} novalidate>
      {#if !registering && savedAccount && !success}<button class="resume-session" type="button" onclick={resume}>Continuar como @{savedAccount.username}<Icon name="arrow-right" size={16} /></button>{/if}
      <label class="field"><span class="label-text">Servidor</span><span class="input-shell"><Icon name="server" size={16} class="input-icon" /><input bind:value={serverUrl} type="text" inputmode="url" autocomplete="off" autocapitalize="off" spellcheck="false" placeholder="https://seu-concord.onrender.com" disabled={busy} /></span></label>
      <label class="field"><span class="label-text">Usuário</span><span class="input-shell"><span class="name-mark" aria-hidden="true">@</span><input bind:value={username} type="text" autocomplete="username" autocapitalize="off" spellcheck="false" maxlength="32" placeholder="seu.usuario" disabled={busy} /></span>{#if registering}<span class="hint">De 3 a 32 caracteres; letras, números, ponto, hífen ou sublinhado.</span>{/if}</label>
      <label class="field"><span class="label-text">Senha</span><span class="input-shell"><Icon name="lock" size={16} class="input-icon" /><input bind:this={passwordInput} bind:value={password} type={revealPassword ? 'text' : 'password'} autocomplete={registering ? 'new-password' : 'current-password'} maxlength="128" placeholder={registering ? 'Pelo menos 12 caracteres' : 'Sua senha'} disabled={busy} /><button type="button" class="reveal" aria-label={revealPassword ? 'Ocultar senha' : 'Mostrar senha'} onclick={() => revealPassword = !revealPassword}><Icon name={revealPassword ? 'eye-off' : 'eye'} size={16} /></button></span></label>
      {#if registering}
        <label class="field"><span class="label-text">Confirme a senha</span><span class="input-shell"><Icon name="lock" size={16} class="input-icon" /><input bind:value={confirmation} type="password" autocomplete="new-password" maxlength="128" placeholder="Digite a senha novamente" disabled={busy} /></span></label>
        <label class="field"><span class="label-text">Convite da turma</span><span class="input-shell"><Icon name="lock" size={16} class="input-icon" /><input bind:value={inviteToken} type="password" autocomplete="off" spellcheck="false" placeholder="Convite recebido para criar sua conta" disabled={busy} /></span><span class="hint">O convite só é necessário no cadastro.</span></label>
      {/if}
      {#if success}<p class="account-success" role="status">{success}</p>{/if}
      {#if displayedError}<div class="error" role="status"><Icon name="warning" size={17} /><span>{displayedError}</span><button type="button" class="error-dismiss" onclick={dismissError} aria-label="Fechar aviso"><Icon name="close" size={14} /></button></div>{/if}
      <button class="submit" type="submit" disabled={busy}>{registering ? 'Criar minha conta' : 'Entrar no Concord'}<Icon name="arrow-right" size={17} /></button>
      <p class="mode-switch">{registering ? 'Já tem uma conta?' : 'Ainda não tem conta?'} <button type="button" onclick={changeMode}>{registering ? 'Ir para o login' : 'Criar conta'}</button></p>
      <p class="fine-print"><Icon name="lock" size={12} />{registering ? 'Após o cadastro, você volta para o login.' : 'A sessão fica salva neste computador. Para removê-la, use Sair da conta.'}</p>
    </form>
  {/if}
</section>
<style>
  .resume-session { display:flex; gap:8px; justify-content:space-between; align-items:center; padding:12px; border:1px solid var(--lime); color:var(--lime); background:var(--lime-soft); border-radius:9px; font-size:12px; font-weight:600; }
  .mode-switch { margin:3px 0; color:var(--muted); font-size:12px; text-align:center; }
  .mode-switch button { color:var(--lime); font-weight:600; padding:3px 5px; text-decoration:underline; text-underline-offset:3px; }
  .account-success { margin:0; padding:10px; border-left:2px solid var(--lime); background:var(--lime-soft); color:var(--lime); font-size:12px; line-height:1.5; }

  .connection-progress {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 15px;
    min-height: 235px;
    padding: 24px 0;
    border-top: 1px solid var(--line);
  }
  .connection-progress strong {
    color: var(--ink);
    font-family: var(--font-display);
    font-size: 22px;
    font-weight: 500;
    line-height: 1.25;
  }
  .connection-progress p {
    margin: 0;
    color: var(--ink-soft);
    font-size: 12px;
    line-height: 1.65;
  }
  .attempt {
    color: var(--faint);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .waiting-spinner {
    width: 26px;
    height: 26px;
    border: 2px solid var(--line-strong);
    border-top-color: var(--lime);
    border-radius: 50%;
    animation: spin 1s linear infinite;
  }
  .cancel-connect {
    width: 100%;
    min-height: 44px;
    border: 1px solid var(--line-strong);
    border-radius: 10px;
    color: var(--ink-soft);
    font-size: 13px;
  }
  .cancel-connect:hover:not(:disabled) { background: var(--surface); color: var(--ink); }
  .cancel-connect:disabled { opacity: 0.6; }
  @media (prefers-reduced-motion: reduce) {
    .waiting-spinner { animation: none; }
  }
  .connect-card {
    position: relative;
    width: 100%;
    max-width: 430px;
    padding: 26px 26px 22px;
    border-radius: var(--radius-lg);
    border: 1px solid var(--line-strong);
    background:
      linear-gradient(155deg, rgba(241, 232, 215, 0.045), transparent 42%),
      var(--surface);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
  }

  .connect-card::before {
    content: '';
    position: absolute;
    inset: 0 0 auto;
    height: 1px;
    background: linear-gradient(90deg, transparent, var(--lime), transparent);
    opacity: 0.55;
  }

  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 22px;
  }

  .signal-lights {
    display: flex;
    gap: 5px;
  }

  .signal-lights i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--faint);
  }

  .signal-lights i:first-child {
    background: var(--lime);
    box-shadow: 0 0 10px rgba(217, 242, 107, 0.6);
  }

  .intro h2 {
    margin: 0 0 8px;
    font-size: clamp(30px, 3.2vw, 38px);
    line-height: 1.04;
  }

  .intro p {
    margin: 0 0 22px;
    max-width: 34ch;
    color: var(--muted);
    font-size: 13.5px;
    line-height: 1.55;
  }

  .browser-note {
    display: flex;
    align-items: flex-start;
    gap: 9px;
    margin-bottom: 18px;
    padding: 10px 12px;
    border-radius: var(--radius-sm);
    border: 1px solid color-mix(in srgb, var(--amber) 25%, transparent);
    background: color-mix(in srgb, var(--amber) 8%, transparent);
    color: var(--ink-soft);
    font-size: 12px;
    line-height: 1.45;
  }

  .browser-note :global(svg) {
    margin-top: 1px;
    color: var(--amber);
  }



  form {
    display: flex;
    flex-direction: column;
    gap: 15px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 7px;
  }

  .label-text {
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .input-shell {
    position: relative;
    display: flex;
    align-items: center;
    border-radius: 10px;
    border: 1px solid var(--line);
    background: rgba(8, 10, 9, 0.48);
    transition:
      border-color 140ms ease,
      box-shadow 140ms ease,
      background 140ms ease;
  }

  .input-shell:focus-within {
    border-color: color-mix(in srgb, var(--lime) 55%, transparent);
    box-shadow: 0 0 0 3px rgba(217, 242, 107, 0.09);
    background: rgba(8, 10, 9, 0.72);
  }

  .input-shell :global(.input-icon) {
    position: absolute;
    left: 12px;
    color: var(--faint);
    pointer-events: none;
  }

  .name-mark {
    position: absolute;
    left: 12px;
    width: 17px;
    color: var(--faint);
    font-family: var(--font-display);
    font-size: 12px;
    font-weight: 600;
    letter-spacing: -0.04em;
    text-align: center;
    pointer-events: none;
  }

  input {
    width: 100%;
    height: 43px;
    padding: 0 14px 0 38px;
    border: 0;
    border-radius: 10px;
    background: transparent;
    color: var(--ink);
    font-size: 13.5px;
    outline: none;
  }

  input::placeholder {
    color: var(--faint);
  }

  input:disabled {
    opacity: 0.58;
  }

  input[type='password'],
  input[type='text'] {
    letter-spacing: 0.005em;
  }

  .reveal {
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    margin-right: 3px;
    border-radius: 7px;
    color: var(--faint);
    transition: color 120ms ease, background 120ms ease;
  }

  .reveal:hover {
    color: var(--ink);
    background: rgba(241, 232, 215, 0.06);
  }

  .hint {
    color: var(--faint);
    font-size: 10.5px;
    line-height: 1.4;
  }

  .error {
    display: flex;
    align-items: flex-start;
    gap: 9px;
    padding: 10px 10px 10px 12px;
    border-radius: var(--radius-sm);
    border: 1px solid color-mix(in srgb, var(--coral) 30%, transparent);
    background: color-mix(in srgb, var(--coral) 8%, transparent);
    color: var(--coral);
    font-size: 12px;
    line-height: 1.45;
  }

  .error :global(svg) {
    margin-top: 1px;
  }

  .error span {
    flex: 1;
    min-width: 0;
  }

  .error-dismiss {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    margin: -2px -2px 0 0;
    border-radius: 5px;
    color: inherit;
    opacity: 0.75;
  }

  .error-dismiss:hover {
    opacity: 1;
    background: rgba(255, 159, 139, 0.12);
  }

  .submit {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 9px;
    height: 46px;
    margin-top: 2px;
    border-radius: 10px;
    background: var(--lime);
    color: #12160f;
    font-size: 13.5px;
    font-weight: 700;
    letter-spacing: 0.01em;
    transition:
      transform 140ms ease,
      background 140ms ease,
      box-shadow 140ms ease;
  }

  .submit:hover:not(:disabled) {
    background: var(--lime-strong);
    transform: translateY(-1px);
    box-shadow: 0 8px 26px rgba(217, 242, 107, 0.17);
  }

  .submit:active:not(:disabled) {
    transform: translateY(0);
  }

  .submit:disabled {
    opacity: 0.6;
  }



  .fine-print {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    margin: 1px 0 0;
    color: var(--faint);
    font-size: 10.5px;
    line-height: 1.5;
  }

  .fine-print :global(svg) {
    margin-top: 2px;
  }
</style>

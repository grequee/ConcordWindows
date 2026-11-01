<script lang="ts">
  import { describeError, setMicrophoneSettings, listAudioDevices, setAudioDevices, startAudioTest, stopAudioTest, listenAudioTest, setDesktopSettings, type DesktopStatus } from './api';
  import { onMount } from 'svelte';
  import { captureShortcut } from './shortcut';
  import Icon from './Icon.svelte';
  import type { CallInfo, MicrophoneSettings, AudioDeviceSettings, AudioDeviceInfo, AudioTestEvent, AudioActivityEvent } from './types';

  interface Props {
    settings: MicrophoneSettings;
    noiseSuppressionAvailable: boolean;
    call: CallInfo | null;
    audioDevices: AudioDeviceSettings;
    activity: AudioActivityEvent | null;
    selfId: string;
    desktop: DesktopStatus | null;
    ondesktopupdate: (status: DesktopStatus) => void;
  }

  let { settings, noiseSuppressionAvailable, call, audioDevices, activity, selfId, desktop, ondesktopupdate }: Props = $props();
  let dialog: HTMLDialogElement;
  let volume = $state(100);
  let noiseSuppression = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let saved = $state(false);
  let inputDevice = $state('');
  let outputDevice = $state('');
  let devices = $state<AudioDeviceInfo[]>([]);
  let loadingDevices = $state(false);
  let applyingDevices = $state(false);
  let devicesSaved = $state(false);
  let test = $state<AudioTestEvent | null>(null);
  let testId: string | null = null;
  let listening = $state(false);
  let shortcutSaving = $state(false);
  let shortcutError = $state<string | null>(null);
  const inputs = $derived(devices.filter(device => device.input_channels !== null));
  const outputs = $derived(devices.filter(device => device.output_channels !== null));
  const devicesDirty = $derived(inputDevice !== (audioDevices.input_device ?? '') || outputDevice !== (audioDevices.output_device ?? ''));
  const testing = $derived(test !== null && ['opening', 'recording', 'playing'].includes(test.phase));
  const blocked = $derived(saving || applyingDevices || testing);
  const selfLevel = $derived(activity?.call_id === call?.id ? activity?.users.find(user => user.user_id === selfId)?.level : null);

  onMount(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    void listenAudioTest(event => { if (alive && event.id === testId) { test = event; if (event.error) error = event.error; } })
      .then(stop => { if (alive) { unlisten = stop; listening = true; } else stop(); })
      .catch(cause => { if (alive) error = describeError(cause); });
    return () => { alive = false; unlisten?.(); if (testId) void stopAudioTest().catch(() => {}); };
  });

  async function refreshDevices() {
    loadingDevices = true;
    try { devices = await listAudioDevices(); }
    catch (cause) { error = describeError(cause); }
    finally { loadingDevices = false; }
  }
  function selectedDevices(): AudioDeviceSettings { return { input_device: inputDevice || null, output_device: outputDevice || null }; }
  async function applyDevices() {
    if (blocked) return;
    applyingDevices = true; error = null; devicesSaved = false;
    try { await setAudioDevices(selectedDevices()); devicesSaved = true; }
    catch (cause) { error = describeError(cause); }
    finally { applyingDevices = false; }
  }
  async function testVoice() {
    if (call || blocked || !listening) return;
    error = null;
    testId = crypto.randomUUID();
    const startedId = testId;
    test = { id: testId, phase: 'opening', remaining_ms: 0, level: 0, clipped: false, error: null };
    try { await startAudioTest(testId, selectedDevices(), { volume_percent: volume, noise_suppression: noiseSuppression }); }
    catch (cause) { if (testId === startedId) { error = describeError(cause); test = null; } }
  }
  async function stopTest() {
    const stoppingId = testId;
    try { await stopAudioTest(); }
    catch (cause) { error = describeError(cause); }
    finally { if (testId === stoppingId) { test = null; testId = null; } }
  }

  const dirty = $derived(volume !== settings.volume_percent || noiseSuppression !== settings.noise_suppression);
  const level = $derived(Math.max(0, Math.min(1, testing ? (test?.level ?? 0) : (selfLevel ?? call?.audio.microphone_level ?? 0))));
  const levelDb = $derived(level > 0.0001 ? Math.round(20 * Math.log10(level)) : null);
  const meterPercent = $derived(Math.max(0, Math.min(100, ((levelDb ?? -60) + 60) / 60 * 100)));
  const clipped = $derived(testing ? (test?.clipped ?? false) : (call?.audio.microphone_clipped ?? false));

  function open() {
    volume = settings.volume_percent;
    noiseSuppression = settings.noise_suppression;
    error = null;
    saved = false;
    inputDevice = audioDevices.input_device ?? '';
    outputDevice = audioDevices.output_device ?? '';
    devicesSaved = false;
    test = null;
    testId = null;
    dialog.showModal();
    void refreshDevices();
  }

  function close() {
    if (!saving && !applyingDevices) dialog.close();
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (blocked) return;
    saving = true;
    error = null;
    saved = false;
    try {
      await setMicrophoneSettings({ volume_percent: volume, noise_suppression: noiseSuppression });
      saved = true;
    } catch (cause) {
      error = describeError(cause);
    } finally {
      saving = false;
    }
  }

  async function saveMuteShortcut(muteShortcut: string) {
    if (!desktop || shortcutSaving) return;
    shortcutSaving = true;
    shortcutError = null;
    try {
      const status = await setDesktopSettings({ ...desktop.settings, mute_shortcut: muteShortcut });
      ondesktopupdate(status);
    } catch (cause) {
      shortcutError = describeError(cause);
    } finally {
      shortcutSaving = false;
    }
  }

  function captureMuteShortcut(event: KeyboardEvent) {
    if (event.key === 'Tab' || event.key === 'Escape') return;
    event.preventDefault();
    event.stopPropagation();
    const shortcut = captureShortcut(event);
    if (shortcut && !event.repeat) void saveMuteShortcut(shortcut);
  }

  function reset() {
    volume = 100;
    noiseSuppression = false;
    saved = false;
  }
</script>

<button class="settings-entry" onclick={open} aria-haspopup="dialog" title="Configurar áudio">
  <span class="entry-icon"><Icon name="mic" size={16} /></span>
  <span class="entry-copy"><strong>Áudio</strong><span>dispositivos e teste de voz</span></span>
  <span class="entry-value">{settings.volume_percent}%</span>
</button>

<dialog bind:this={dialog} aria-labelledby="microphone-title" onclose={() => { if (testId) void stopTest(); }} oncancel={(event) => { if (saving || applyingDevices) event.preventDefault(); }}>
  <form onsubmit={save}>
    <div class="panel-heading">
      <span class="eyebrow"><span class="live-dot"></span> Ajustes de voz</span>
      <button type="button" class="close-button" onclick={close} disabled={saving || applyingDevices} aria-label="Fechar configurações de áudio">
        <Icon name="close" size={18} />
      </button>
    </div>

    <h2 id="microphone-title">Sua voz, do seu jeito.</h2>
    <p class="intro">Escolha onde falar e ouvir. Os ajustes valem só para o Concord.</p>

    <section class="devices-section" aria-label="Dispositivos de áudio">
      <div class="section-title"><strong>Dispositivos</strong><button class="refresh-devices" type="button" onclick={refreshDevices} disabled={loadingDevices || blocked}><Icon name="refresh" size={12} />{loadingDevices ? 'Buscando…' : 'Atualizar lista'}</button></div>
      <div class="device-selectors">
        <label for="audio-input"><span>Microfone</span><select id="audio-input" bind:value={inputDevice} disabled={blocked || loadingDevices} onchange={() => devicesSaved = false}>
          <option value="">Padrão do sistema</option>
          {#if inputDevice && !inputs.some(device => device.id === inputDevice)}<option value={inputDevice}>Microfone salvo · indisponível</option>{/if}
          {#each inputs as device (device.id)}<option value={device.id}>{device.name}{device.is_default_input ? ' · padrão' : ''}</option>{/each}
        </select></label>
        <label for="audio-output"><span>Fone ou alto-falante</span><select id="audio-output" bind:value={outputDevice} disabled={blocked || loadingDevices} onchange={() => devicesSaved = false}>
          <option value="">Padrão do sistema</option>
          {#if outputDevice && !outputs.some(device => device.id === outputDevice)}<option value={outputDevice}>Saída salva · indisponível</option>{/if}
          {#each outputs as device (device.id)}<option value={device.id}>{device.name}{device.is_default_output ? ' · padrão' : ''}</option>{/each}
        </select></label>
      </div>
      <div class="device-actions"><span>{devicesSaved ? 'Dispositivos salvos.' : call ? 'Troque sem sair da chamada.' : 'Sua escolha vale nas próximas chamadas.'}</span><button type="button" onclick={applyDevices} disabled={blocked || !devicesDirty}>{applyingDevices ? 'Aplicando…' : 'Aplicar dispositivos'}</button></div>
    </section>

    <section class="voice-test" class:active={testing} aria-label="Teste de voz">
      <div class="test-copy"><strong>{test?.phase === 'recording' ? `Gravando… ${Math.ceil(test.remaining_ms / 1000)} s` : test?.phase === 'playing' ? `Ouça sua voz · ${Math.ceil(test.remaining_ms / 1000)} s` : test?.phase === 'opening' ? 'Abrindo os dispositivos…' : 'Ouça como você chega.'}</strong>
      <p>{call ? 'O teste fica disponível antes da chamada. Acompanhe seu nível abaixo.' : testing ? 'Este teste é só seu. Nada é enviado para a sala.' : 'Grave cinco segundos e ouça em seguida nos fones escolhidos.'}</p></div>
      {#if testing}<button type="button" class="test-button" onclick={stopTest}>Parar teste</button>{:else}<button type="button" class="test-button" onclick={testVoice} disabled={call !== null || blocked || !listening}><Icon name="mic" size={14} />Testar voz</button>{/if}
    </section>

    <section class="shortcut-section" aria-label="Atalho do microfone">
      <div class="shortcut-heading"><strong>Atalho para mutar e desmutar</strong><span>{desktop?.global_shortcut_active ? 'Ativo em segundo plano' : 'Atalho de teclado'}</span></div>
      <input class="shortcut-input" aria-label="Pressione o novo atalho do microfone" readonly value={desktop?.settings.mute_shortcut || 'Desativado'} onkeydown={captureMuteShortcut} disabled={!desktop || shortcutSaving} />
      <p class="field-hint">Selecione o campo e pressione Ctrl, Alt ou Meta junto com uma letra, número ou F1 a F12. Shift é opcional.</p>
      {#if desktop}<p class="shortcut-status" role="status">{desktop.shortcut_message}</p>{/if}
      {#if shortcutError}<p class="message error" role="alert">{shortcutError}</p>{/if}
      <div class="shortcut-actions"><button type="button" onclick={() => saveMuteShortcut('')} disabled={!desktop || shortcutSaving}>Desativar atalho</button><button type="button" onclick={() => saveMuteShortcut('Ctrl+Shift+M')} disabled={!desktop || shortcutSaving}>Restaurar padrão</button></div>
    </section>

    <fieldset disabled={blocked}>
      <div class="volume-header">
        <label for="microphone-volume">Volume de entrada</label>
        <output for="microphone-volume" class:boosted={volume > 100}>{volume}<span>%</span></output>
      </div>
      <input id="microphone-volume" type="range" min="0" max="200" step="1" bind:value={volume}
        oninput={() => (saved = false)} aria-valuetext={`${volume}%`} />
      <div class="range-labels"><span>0 · silêncio</span><span>100 · original</span><span>200 · amplificado</span></div>
      <p class="field-hint">Acima de 100%, o app amplifica a entrada. Se a voz distorcer, diminua o volume.</p>

      <div class="noise-row">
        <div class="noise-copy">
          <label for="noise-suppression">Supressão de ruído</label>
          <p>Reduz chiado e ruídos constantes, como ventilador.</p>
        </div>
        <input id="noise-suppression" class="noise-switch" type="checkbox" role="switch"
          bind:checked={noiseSuppression} disabled={!noiseSuppressionAvailable}
          onchange={() => (saved = false)} />
      </div>
      {#if !noiseSuppressionAvailable}
        <p class="field-hint unavailable">Supressão indisponível nesta versão do app. O volume continua funcionando.</p>
      {/if}
    </fieldset>

    <div class="level-card" class:clipped>
      <div class="level-heading">
        <span>{testing ? 'Nível do teste' : 'Nível enviado'}</span>
        <span class="level-value">{testing ? levelDb === null ? 'silêncio' : `${levelDb} dB` : !call ? 'fora da chamada' : call.muted ? 'microfone silenciado' : levelDb === null ? 'silêncio' : `${levelDb} dB`}</span>
      </div>
      <div class="level-track" role="meter" aria-label="Nível do microfone após os ajustes salvos"
        aria-valuemin="0" aria-valuemax="100" aria-valuenow={call?.muted ? 0 : meterPercent}>
        <div class="level-fill" style:width={`${call?.muted ? 0 : meterPercent}%`}></div>
      </div>
      <p>{testing ? 'Use fones para ouvir a reprodução com clareza.' : !call ? 'O teste usa os dispositivos, ganho e redução de ruído escolhidos neste painel.' : clipped && !call.muted ? 'Entrada saturando. Reduza o volume para evitar distorção.' : 'O indicador mostra o áudio com os ajustes já salvos.'}</p>
    </div>

    {#if error}<p class="message error" role="alert">{error}</p>{/if}
    <p class="message" role="status">{saved ? 'Ajustes salvos e aplicados.' : 'Ao salvar, a mudança vale na chamada atual e nas próximas.'}</p>
    <div class="panel-actions">
      <button type="button" class="reset-button" onclick={reset} disabled={blocked}>Restaurar microfone</button>
      <button type="submit" class="save-button" disabled={blocked || !dirty}>{saving ? 'Salvando…' : 'Salvar ganho e ruído'}<Icon name="check" size={15} /></button>
    </div>
  </form>
</dialog>

<style>
  .devices-section { margin: 24px 0; padding-bottom: 20px; border-bottom: 1px solid var(--line); }
  .shortcut-section { margin: 0 0 24px; padding: 0 0 20px; border-bottom: 1px solid var(--line); }
  .shortcut-heading { display:flex; align-items:center; justify-content:space-between; gap:12px; margin-bottom:10px; }
  .shortcut-heading strong { color:var(--ink-soft); font-size:12px; }
  .shortcut-heading span, .shortcut-status { color:var(--muted); font-size:10px; }
  .shortcut-input { width:100%; padding:11px 12px; border:1px solid var(--line-strong); border-radius:8px; background:var(--bg); color:var(--lime); font:inherit; font-size:12px; }
  .shortcut-input:focus-visible { outline:2px solid var(--lime); outline-offset:2px; }
  .shortcut-actions { display:flex; align-items:center; gap:16px; margin-top:10px; }
  .shortcut-actions button { color:var(--lime); font-size:10px; padding:6px 0; }
  .shortcut-actions button:disabled { opacity:.45; cursor:default; }
  .shortcut-status { margin:8px 0 0; }
  .section-title { display: flex; justify-content: space-between; align-items: center; margin-bottom: 14px; }
  .section-title strong { color: var(--ink-soft); font-size: 12px; }
  .refresh-devices { display: inline-flex; align-items: center; gap: 6px; color: var(--muted); font-size: 10px; }
  .device-selectors { display: grid; gap: 14px; }
  .device-selectors label { display: grid; gap: 7px; font-size: 11px; color: var(--muted); }
  .device-selectors select { min-width: 0; width: 100%; padding: 11px 12px; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--ink); font: inherit; font-size: 12px; }
  .device-selectors select:focus-visible { outline: 2px solid var(--lime); outline-offset: 2px; }
  .device-actions { display: flex; gap: 14px; justify-content: space-between; align-items: center; margin-top: 14px; }
  .device-actions span { font-size: 10px; color: var(--faint); }
  .device-actions button { flex: none; padding: 8px 10px; border: 1px solid var(--line-strong); border-radius: 7px; color: var(--lime); font-size: 10px; }
  .voice-test { display: flex; align-items: center; gap: 14px; padding: 15px; margin-bottom: 24px; border: 1px solid var(--line-strong); border-radius: 10px; background: var(--lime-soft); }
  .voice-test.active { border-color: color-mix(in srgb, var(--lime) 40%, transparent); }
  .test-copy { flex: 1; }
  .test-copy strong { color: var(--ink-soft); font-size: 12px; }
  .test-copy p { margin: 6px 0 0; color: var(--muted); font-size: 10px; line-height: 1.6; }
  .test-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; flex: none; padding: 9px 11px; border-radius: 7px; border: 1px solid var(--line-strong); color: var(--lime); font-size: 11px; }
  .test-button:disabled, .device-actions button:disabled { opacity: 0.4; cursor: default; }

  .settings-entry {
    flex-shrink: 0;
    margin-top: 14px;
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: 10px;
    color: var(--ink-soft);
    background: transparent;
    cursor: pointer;
    text-align: left;
    transition: background 140ms, border-color 140ms;
  }
  .settings-entry:hover {
    background: var(--surface-hover);
    border-color: var(--line-strong);
  }
  .entry-icon {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 9px;
    color: var(--lime);
    background: var(--lime-soft);
  }
  .entry-copy {
    display: grid;
    gap: 2px;
    flex: 1;
  }
  .entry-copy strong {
    font-size: 12px;
    font-weight: 600;
  }
  .entry-copy > span {
    font-size: 10px;
    color: var(--muted);
  }
  .entry-value {
    color: var(--muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  dialog {
    width: min(560px, calc(100vw - 32px));
    max-height: calc(100vh - 40px);
    padding: 0;
    border: 1px solid var(--line-strong);
    border-radius: 18px;
    color: var(--ink);
    background: var(--surface);
    box-shadow: var(--shadow-lg);
  }
  dialog::backdrop {
    background: rgb(4 8 5 / 76%);
    backdrop-filter: blur(5px);
  }
  form {
    padding: 28px;
    max-height: calc(100vh - 40px);
    overflow-y: auto;
  }
  .panel-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .eyebrow {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--muted);
    font-size: 10px;
    font-weight: 600;
    letter-spacing: .13em;
    text-transform: uppercase;
  }
  .live-dot {
    width: 6px;
    height: 6px;
    background: var(--lime);
    border-radius: 50%;
  }
  .close-button {
    display: grid;
    place-items: center;
    padding: 6px;
    color: var(--muted);
    background: transparent;
    border: 0;
    border-radius: 6px;
    cursor: pointer;
  }
  .close-button:hover {
    color: var(--ink);
    background: var(--surface-hover);
  }
  h2 {
    margin: 20px 0 8px;
    font: 400 36px/1.15 var(--font-display);
    letter-spacing: -.04em;
  }
  .intro {
    margin: 0 0 28px;
    color: var(--muted);
    font-size: 12px;
    line-height: 1.6;
  }
  fieldset {
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }
  .volume-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  label {
    font-size: 13px;
    font-weight: 600;
  }
  output {
    font-size: 26px;
    font-variant-numeric: tabular-nums;
    letter-spacing: -.05em;
    color: var(--lime);
  }
  output span {
    margin-left: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  output.boosted {
    color: var(--amber);
  }
  input[type='range'] {
    display: block;
    width: 100%;
    margin: 16px 0 10px;
    accent-color: var(--lime);
    cursor: pointer;
  }
  .range-labels {
    display: flex;
    justify-content: space-between;
    color: var(--faint);
    font-size: 9px;
  }
  .field-hint {
    margin: 13px 0 0;
    font-size: 11px;
    line-height: 1.65;
    color: var(--muted);
  }
  .noise-row {
    display: flex;
    align-items: center;
    gap: 20px;
    margin-top: 24px;
    padding-top: 24px;
    border-top: 1px solid var(--line);
  }
  .noise-copy {
    flex: 1;
  }
  .noise-copy p {
    color: var(--muted);
    font-size: 11px;
    line-height: 1.6;
    margin: 6px 0 0;
  }
  .noise-switch {
    appearance: none;
    flex: none;
    position: relative;
    width: 36px;
    height: 21px;
    margin: 0;
    border: 1px solid var(--line-strong);
    border-radius: 20px;
    background: var(--bg);
    cursor: pointer;
    transition: background 140ms;
  }
  .noise-switch::after {
    content: '';
    position: absolute;
    width: 13px;
    height: 13px;
    top: 3px;
    left: 3px;
    border-radius: 50%;
    background: var(--muted);
    transition: transform 140ms;
  }
  .noise-switch:checked {
    background: var(--lime);
    border-color: var(--lime);
  }
  .noise-switch:checked::after {
    transform: translateX(15px);
    background: var(--bg);
  }
  .noise-switch:disabled {
    opacity: .4;
    cursor: not-allowed;
  }
  .level-card {
    margin-top: 26px;
    padding: 14px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--bg);
  }
  .level-heading {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    color: var(--ink-soft);
    font-size: 10px;
  }
  .level-value {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .level-track {
    overflow: hidden;
    height: 5px;
    margin: 10px 0;
    border-radius: 4px;
    background: var(--surface-hover);
  }
  .level-fill {
    height: 100%;
    background: var(--lime);
    transition: width 200ms ease;
  }
  .clipped .level-fill {
    background: var(--coral);
  }
  .level-card p {
    color: var(--faint);
    font-size: 10px;
    line-height: 1.6;
    margin: 0;
  }
  .message {
    margin: 18px 0;
    min-height: 16px;
    color: var(--muted);
    font-size: 11px;
    line-height: 1.5;
  }
  .error {
    color: var(--coral);
    margin-bottom: 0;
  }
  .unavailable {
    color: var(--amber);
  }
  .panel-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding-top: 18px;
    border-top: 1px solid var(--line);
  }
  .reset-button {
    padding: 9px 0;
    border: 0;
    color: var(--muted);
    background: transparent;
    font-size: 11px;
    cursor: pointer;
  }
  .save-button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 10px 14px;
    border: 1px solid var(--lime);
    border-radius: 7px;
    color: var(--bg);
    background: var(--lime);
    font-size: 11px;
    font-weight: 650;
    cursor: pointer;
  }
  button:disabled {
    opacity: .5;
    cursor: default;
  }
  button:focus-visible, input:focus-visible {
    outline: 2px solid var(--lime);
    outline-offset: 4px;
  }
  @media (max-width: 460px) {
    form {
      padding: 22px;
    }
    h2 {
      font-size: 30px;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .settings-entry, .noise-switch, .noise-switch::after, .level-fill {
      transition: none;
    }
  }
</style>

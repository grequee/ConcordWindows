<script lang="ts">
  export type IconName =
    | 'hash'
    | 'search'
    | 'reply'
    | 'edit'
    | 'trash'
    | 'settings'
    | 'bell'
    | 'image'
    | 'attach'
    | 'file'
    | 'download'
    | 'users'
    | 'refresh'
    | 'logout'
    | 'mic'
    | 'mic-off'
    | 'phone'
    | 'phone-off'
    | 'headphones'
    | 'headphones-off'
    | 'screen'
    | 'screen-off'
    | 'send'
    | 'close'
    | 'warning'
    | 'lock'
    | 'server'
    | 'arrow-right'
    | 'check'
    | 'chevron-down'
    | 'eye'
    | 'eye-off'
    | 'maximize';

  interface Props {
    name: IconName;
    size?: number;
    strokeWidth?: number;
    class?: string;
  }

  let { name, size = 18, strokeWidth = 1.7, class: className = '' }: Props = $props();

  const icons: Record<IconName, string[]> = {
    attach: ['m21 11-8 8a6 6 0 0 1-9-9l9-9a4 4 0 0 1 6 6l-9 9a2 2 0 0 1-3-3l8-8'],
    file: ['M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z', 'M14 2v6h6'],
    download: ['M12 3v12', 'm7 10 5 5 5-5', 'M5 17v4h14v-4'],
    image: ['M3 3h18v18H3z', 'm3 16 5-5 4 4 3-3 6 6', 'M15 7h.01'],
    search: ['M21 21l-5-5', 'M10 3a7 7 0 1 0 0 14 7 7 0 0 0 0-14'],
    reply: ['m9 10-5 5 5 5', 'M4 15h10a6 6 0 0 0 6-6V5'],
    edit: ['m16 3 5 5-12 12-6 1 1-6z', 'm14 5 5 5'],
    trash: ['M3 6h18', 'M9 6V3h6v3', 'M5 6l1 15h12l1-15', 'M10 10v7', 'M14 10v7'],
    settings: ['M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8', 'M4 4l2 2m12 12 2 2M4 20l2-2M18 6l2-2M12 1v3m0 16v3M1 12h3m16 0h3'],
    bell: ['M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9', 'M10 21h4'],
    hash: ['M4 9h16', 'M4 15h16', 'M10 3 8 21', 'M16 3l-2 18'],
    users: [
      'M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2',
      'M9 3a4 4 0 1 0 0 8 4 4 0 0 0 0-8',
      'M22 21v-2a4 4 0 0 0-3-3.87',
      'M16 3.13a4 4 0 0 1 0 7.75',
    ],
    refresh: [
      'M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8',
      'M21 3v5h-5',
      'M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16',
      'M8 16H3v5',
    ],
    logout: ['M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4', 'm16 17 5-5-5-5', 'M21 12H9'],
    mic: [
      'M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z',
      'M19 10v2a7 7 0 0 1-14 0v-2',
      'M12 19v3',
    ],
    'mic-off': [
      'M2 2l20 20',
      'M18.89 13.23A7.12 7.12 0 0 0 19 12v-2',
      'M5 10v2a7 7 0 0 0 12 5',
      'M15 9.34V5a3 3 0 0 0-5.68-1.33',
      'M9 9v3a3 3 0 0 0 5.12 2.12',
      'M12 19v3',
    ],
    phone: [
      'M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92Z',
    ],
    'phone-off': [
      'M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7 2 2 0 0 1 1.72 2v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.42 19.42 0 0 1-3.33-2.67',
      'M5.31 9.86a19.79 19.79 0 0 1-1.13-5.7A2 2 0 0 1 6.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L10.09 9.91',
      'M2 2l20 20',
    ],
    headphones: [
      'M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3',
    ],
    'headphones-off': [
      'M2 2l20 20',
      'M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 13.6-7.75',
      'M21 14v5a2 2 0 0 1-.2.9',
    ],
    screen: [
      'M2 3h20v14H2z',
      'M8 21h8',
      'M12 17v4',
    ],
    'screen-off': ['M2 2l20 20', 'M2 3h20v14H2z', 'M8 21h8', 'M12 17v4'],
    send: ['m22 2-7 20-4-9-9-4Z', 'M22 2 11 13'],
    close: ['M18 6 6 18', 'm6 6 12 12'],
    warning: [
      'm21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z',
      'M12 9v4',
      'M12 17h.01',
    ],
    lock: [
      'M5 11h14v9a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1z',
      'M7 11V7a5 5 0 0 1 10 0v4',
    ],
    server: [
      'M3 3h18v7H3z',
      'M3 14h18v7H3z',
      'M7 6.5h.01',
      'M7 17.5h.01',
    ],
    'arrow-right': ['M5 12h14', 'm12 5 7 7-7 7'],
    check: ['M20 6 9 17l-5-5'],
    'chevron-down': ['m6 9 6 6 6-6'],
    eye: [
      'M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0',
      'M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6',
    ],
    'eye-off': [
      'M2 2l20 20',
      'M10.58 10.58a2 2 0 0 0 2.84 2.84',
      'M9.88 5.09A10.94 10.94 0 0 1 12 4c7 0 10 8 10 8a18.5 18.5 0 0 1-2.16 3.19',
      'M6.61 6.61A18.15 18.15 0 0 0 2 12s3 8 10 8a10.9 10.9 0 0 0 5.39-1.61',
    ],
    maximize: [
      'M8 3H5a2 2 0 0 0-2 2v3',
      'M21 8V5a2 2 0 0 0-2-2h-3',
      'M3 16v3a2 2 0 0 0 2 2h3',
      'M16 21h3a2 2 0 0 0 2-2v-3',
    ],
  };
</script>

<svg
  class="icon {className}"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={strokeWidth}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  {#each icons[name] as path}
    <path d={path} />
  {/each}
</svg>

<style>
  .icon {
    display: block;
    flex: none;
  }
</style>

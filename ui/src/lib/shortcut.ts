export function captureShortcut(event: KeyboardEvent): string | null {
  if (!(event.ctrlKey || event.altKey || event.metaKey)) return null;
  const key = event.key.toUpperCase();
  if (!/^[A-Z0-9]$|^F([1-9]|1[0-2])$/.test(key)) return null;
  return [event.ctrlKey && 'Ctrl', event.altKey && 'Alt', event.shiftKey && 'Shift', event.metaKey && 'Meta', key].filter(Boolean).join('+');
}
export function matchesShortcut(event: KeyboardEvent, shortcut: string): boolean {
  return !!shortcut && !event.repeat && !event.isComposing && captureShortcut(event) === shortcut;
}

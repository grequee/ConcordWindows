export type TextPart = { text: string; url?: string };
export function linkify(text: string): TextPart[] {
  const parts: TextPart[] = [];
  let offset = 0;
  for (const match of text.matchAll(/https?:\/\/[^\s<>"']+/gi)) {
    const index = match.index ?? 0;
    const candidate = match[0].replace(/[.,!?;:)\]}]+$/, '');
    let url: URL;
    try { url = new URL(candidate); } catch { continue; }
    if (!['http:', 'https:'].includes(url.protocol)) continue;
    if (index > offset) parts.push({ text: text.slice(offset, index) });
    parts.push({ text: candidate, url: url.href });
    offset = index + candidate.length;
  }
  if (offset < text.length) parts.push({ text: text.slice(offset) });
  return parts;
}

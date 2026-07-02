// Rendering helpers shared with the TUI's conventions: human-friendly byte
// sizes for the `<bytes>N</bytes>` prose convention and the byte-count columns,
// plus minimal HTML escaping for prose and table cells.

export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return String(n);
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  const num = i === 0 ? v.toFixed(0) : v.toFixed(v < 10 ? 1 : 0);
  return `${num} ${units[i]}`;
}

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

// Render agent prose: extract `<bytes>N</bytes>` tags into a styled size span,
// escape the surrounding text, and preserve line breaks. Mirrors the TUI's
// inline byte-size rendering so a model that says "the largest table is
// <bytes>4831838208</bytes>" reads "4.5 GiB" inline.
export function renderProse(text: string): string {
  const parts: string[] = [];
  const re = /<bytes>(\d+)<\/bytes>/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    parts.push(escapeHtml(text.slice(last, m.index)));
    parts.push(`<span class="bytes">${formatBytes(Number(m[1]))}</span>`);
    last = re.lastIndex;
  }
  parts.push(escapeHtml(text.slice(last)));
  return parts.join('').replace(/\n/g, '<br>');
}

export function renderCell(value: string | null, isByte: boolean): string {
  if (value === null) return '<span class="null">∅</span>';
  if (isByte && /^\d+$/.test(value)) return formatBytes(Number(value));
  return escapeHtml(value);
}
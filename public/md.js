// Minimal, safe markdown renderer (no deps). Supports the common subset that
// Claude Code assistant messages produce: code fences, inline code, bold,
// italic, headings, lists, links, blockquotes, hr, tables, paragraphs.
// Escapes HTML first, then applies transforms.

function escapeHtml(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function inline(s) {
  // inline code
  s = s.replace(/`([^`]+)`/g, (_, c) => `\u0000IC${btoa(unescape(encodeURIComponent(c)))}IC\u0000`);
  // links [text](url)
  s = s.replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
  // bold
  s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  s = s.replace(/__([^_]+)__/g, "<strong>$1</strong>");
  // italic
  s = s.replace(/(^|[^*])\*([^*]+)\*/g, "$1<em>$2</em>");
  s = s.replace(/(^|[^_])_([^_]+)_/g, "$1<em>$2</em>");
  // restore inline code
  s = s.replace(/\u0000IC([A-Za-z0-9+/=]+)IC\u0000/g, (_, b) => `<code>${escapeHtml(decodeURIComponent(escape(atob(b))))}</code>`);
  return s;
}

export function renderMarkdown(src) {
  if (!src) return "";
  const lines = escapeHtml(src).split("\n");
  let html = "";
  let i = 0;
  let inList = null; // "ul" | "ol" | null
  const closeList = () => { if (inList) { html += `</${inList}>`; inList = null; } };

  while (i < lines.length) {
    let line = lines[i];

    // fenced code
    const fence = line.match(/^```(\w+)?\s*$/);
    if (fence) {
      closeList();
      const lang = fence[1] || "";
      let code = "";
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) { code += (code ? "\n" : "") + lines[i]; i++; }
      i++; // skip closing ```
      html += `<pre><code data-lang="${lang}">${code}</code></pre>`;
      continue;
    }

    // hr
    if (/^(\s*[-*_]){3,}\s*$/.test(line)) { closeList(); html += "<hr/>"; i++; continue; }

    // heading
    const h = line.match(/^(#{1,3})\s+(.*)$/);
    if (h) { closeList(); const lvl = h[1].length; html += `<h${lvl}>${inline(h[2])}</h${lvl}>`; i++; continue; }

    // blockquote
    if (/^>\s?/.test(line)) {
      closeList();
      const block = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) { block.push(lines[i].replace(/^>\s?/, "")); i++; }
      html += `<blockquote>${inline(block.join(" "))}</blockquote>`;
      continue;
    }

    // table (simple: header | sep | rows)
    if (i + 1 < lines.length && /\|/.test(line) && /^\s*\|?[\s:|-]+\|[\s:|-]+\s*$/.test(lines[i + 1])) {
      closeList();
      const splitRow = (r) => r.replace(/^\s*\|/, "").replace(/\|\s*$/, "").split("|").map((c) => c.trim());
      const head = splitRow(line); i += 2;
      html += "<table><thead><tr>" + head.map((c) => `<th>${inline(c)}</th>`).join("") + "</tr></thead><tbody>";
      while (i < lines.length && /\|/.test(lines[i])) {
        const row = splitRow(lines[i]);
        html += "<tr>" + row.map((c) => `<td>${inline(c)}</td>`).join("") + "</tr>";
        i++;
      }
      html += "</tbody></table>";
      continue;
    }

    // list items
    const ul = line.match(/^\s*[-*+]\s+(.*)$/);
    const ol = line.match(/^\s*\d+\.\s+(.*)$/);
    if (ul) { if (inList !== "ul") { closeList(); html += "<ul>"; inList = "ul"; } html += `<li>${inline(ul[1])}</li>`; i++; continue; }
    if (ol) { if (inList !== "ol") { closeList(); html += "<ol>"; inList = "ol"; } html += `<li>${inline(ol[1])}</li>`; i++; continue; }

    // blank
    if (/^\s*$/.test(line)) { closeList(); i++; continue; }

    // paragraph (gather consecutive non-empty, non-special lines)
    closeList();
    const para = [];
    while (i < lines.length && !/^\s*$/.test(lines[i]) && !/^(#{1,3}\s|```|>\s?|\s*[-*+]\s|\s*\d+\.\s)/.test(lines[i])
      && !(i + 1 < lines.length && /\|/.test(lines[i]) && /^\s*\|?[\s:|-]+\|[\s:|-]+\s*$/.test(lines[i + 1]))) {
      para.push(lines[i]); i++;
    }
    html += `<p>${inline(para.join(" "))}</p>`;
  }
  closeList();
  return html;
}

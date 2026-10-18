/** The little HTML the editor allows. Everything else is dropped before the text is saved. */
const ALLOWED = new Set(["P", "BR", "B", "STRONG", "I", "EM", "A", "UL", "OL", "LI", "IMG"]);

function clean(node: Node, doc: Document): Node {
  if (node.nodeType === Node.TEXT_NODE) return doc.createTextNode(node.textContent ?? "");
  const out = doc.createDocumentFragment();
  if (node.nodeType !== Node.ELEMENT_NODE) return out;
  const el = node as Element;
  const kids = (target: Node) => {
    for (const c of Array.from(el.childNodes)) target.appendChild(clean(c, doc));
  };
  if (!ALLOWED.has(el.tagName)) {
    // drop the tag, keep the text (but never script or style)
    if (el.tagName !== "SCRIPT" && el.tagName !== "STYLE") kids(out);
    return out;
  }
  const copy = doc.createElement(el.tagName.toLowerCase());
  if (el.tagName === "A") {
    const href = el.getAttribute("href") ?? "";
    if (/^(https?:|mailto:)/i.test(href.trim())) copy.setAttribute("href", href.trim());
  }
  if (el.tagName === "IMG") {
    // only a picture on the web; no data or script addresses
    const src = (el.getAttribute("src") ?? "").trim();
    if (!/^https:\/\//i.test(src)) return out;
    copy.setAttribute("src", src);
    copy.setAttribute("alt", el.getAttribute("alt") ?? "");
    copy.setAttribute("style", "max-width:100%;height:auto");
    return copy;
  }
  kids(copy);
  return copy;
}

export function sanitizeHtml(html: string): string {
  const doc = new DOMParser().parseFromString(`<body>${html}</body>`, "text/html");
  const wrap = doc.createElement("div");
  for (const c of Array.from(doc.body.childNodes)) wrap.appendChild(clean(c, doc));
  return wrap.innerHTML;
}

/** A plain version for the text part of the email: links become `text (url)`. */
export function htmlToText(html: string): string {
  const doc = new DOMParser().parseFromString(`<body>${html}</body>`, "text/html");
  for (const a of Array.from(doc.querySelectorAll("a[href]"))) {
    const href = a.getAttribute("href") ?? "";
    a.replaceWith(`${a.textContent ?? ""} (${href})`);
  }
  for (const li of Array.from(doc.querySelectorAll("li"))) li.prepend("- ");
  for (const br of Array.from(doc.querySelectorAll("br"))) br.replaceWith("\n");
  for (const b of Array.from(doc.querySelectorAll("p, li"))) b.append("\n");
  return (doc.body.textContent ?? "").replace(/\n{3,}/g, "\n\n").trim();
}

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** Plain text to HTML: blank-line separated blocks become paragraphs, single line breaks stay. */
export function textToHtml(text: string): string {
  return text
    .split(/\n{2,}/)
    .map((p) => p.trim())
    .filter(Boolean)
    .map((p) => `<p>${esc(p).replace(/\n/g, "<br>")}</p>`)
    .join("");
}

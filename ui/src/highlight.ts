// Visual highlighting for the script editor. Mirrors the Rust parser's rules closely enough
// for colouring; the authoritative parse (warnings, segments) always comes from Rust.
import { esc } from "./api";

type Tag = "excited" | "calm" | "mystery" | "emph";

export function highlight(src: string): string {
  // First pass: find which tag characters are matched (same stack rules as Rust).
  const n = src.length;
  const matched = new Array<boolean>(n).fill(false);
  const stack: { tag: Tag; i: number }[] = [];
  const isEsc = new Array<boolean>(n).fill(false);
  for (let i = 0; i < n; i++) {
    const c = src[i];
    if (c === "\\" && i + 1 < n && `"[]{}*-\\`.includes(src[i + 1])) {
      isEsc[i] = true;
      i++;
      continue;
    }
    if (c === "\n" && /^\n[ \t\r]*\n/.test(src.slice(i))) {
      stack.length = 0; // tags never cross paragraphs
      continue;
    }
    const open = c === "[" ? "calm" : c === "{" ? "mystery" : null;
    const close = c === "]" ? "calm" : c === "}" ? "mystery" : null;
    const toggle = c === '"' ? "excited" : c === "*" ? "emph" : null;
    if (open) stack.push({ tag: open, i });
    else if (toggle || close) {
      const tag = (toggle || close) as Tag;
      const k = stack.map((s) => s.tag).lastIndexOf(tag);
      if (k >= 0) {
        matched[stack[k].i] = true;
        matched[i] = true;
        stack.length = k;
      } else if (toggle) stack.push({ tag, i });
    }
  }
  // Second pass: emit spans.
  let out = "";
  const active: Tag[] = [];
  const cls = () => {
    const emo = [...active].reverse().find((t) => t !== "emph");
    return [emo ?? "", active.includes("emph") ? "emph" : ""].filter(Boolean).join(" ");
  };
  let buf = "";
  let bufCls = "";
  const flush = () => {
    if (buf) out += bufCls ? `<span class="${bufCls}">${esc(buf)}</span>` : esc(buf);
    buf = "";
  };
  const push = (text: string, c: string) => {
    if (c !== bufCls) {
      flush();
      bufCls = c;
    }
    buf += text;
  };
  for (let i = 0; i < n; i++) {
    const c = src[i];
    if (isEsc[i]) {
      push("", cls());
      flush();
      out += `<span class="esc">\\</span>`;
      push(src[i + 1], cls());
      i++;
      continue;
    }
    if (c === "-" && src[i + 1] === "-") {
      let j = i;
      while (src[j] === "-") j++;
      flush();
      out += `<span class="pause">${esc(src.slice(i, j))}</span>`;
      i = j - 1;
      continue;
    }
    if (`"[]{}*`.includes(c)) {
      flush();
      if (!matched[i]) {
        out += `<span class="m bad" title="unmatched — ignored">${esc(c)}</span>`;
        continue;
      }
      const tag: Tag = c === '"' ? "excited" : c === "[" || c === "]" ? "calm" : c === "{" || c === "}" ? "mystery" : "emph";
      const opening = c === "[" || c === "{" || ((c === '"' || c === "*") && !active.includes(tag));
      if (opening) {
        active.push(tag);
        out += `<span class="m ${cls()}">${esc(c)}</span>`;
      } else {
        out += `<span class="m ${cls()}">${esc(c)}</span>`;
        const k = active.lastIndexOf(tag);
        if (k >= 0) active.length = k;
      }
      continue;
    }
    if (c === "\n" && /^\n[ \t\r]*\n/.test(src.slice(i))) active.length = 0;
    push(c, cls());
  }
  flush();
  // A trailing newline needs a visible character so the backdrop matches textarea height.
  return out + "\n";
}

/** Wrap the current selection of a textarea with markup. */
export function wrapSelection(ta: HTMLTextAreaElement, tag: string) {
  const s = ta.selectionStart;
  const e = ta.selectionEnd;
  const v = ta.value;
  const sel = v.slice(s, e);
  let ins: string;
  let caretStart: number;
  let caretEnd: number;
  const pairs: Record<string, [string, string]> = { excited: ['"', '"'], calm: ["[", "]"], mystery: ["{", "}"], emphasis: ["*", "*"] };
  if (tag === "short" || tag === "long") {
    const mark = tag === "short" ? "--" : "---";
    const before = s > 0 && !/\s/.test(v[s - 1]) ? " " : "";
    const after = e < v.length && !/\s/.test(v[e]) ? " " : "";
    ins = sel ? `${sel}${before ? "" : " "}${mark}${after}` : `${before}${mark}${after || " "}`;
    caretStart = caretEnd = s + ins.length;
  } else {
    const [o, c] = pairs[tag];
    // Keep surrounding whitespace outside the tags.
    const lead = sel.match(/^\s*/)![0];
    const trail = sel.match(/\s*$/)![0];
    const core = sel.slice(lead.length, sel.length - trail.length);
    ins = `${lead}${o}${core}${c}${trail}`;
    caretStart = s + lead.length + o.length;
    caretEnd = caretStart + core.length;
  }
  ta.focus();
  ta.setRangeText(ins, s, e, "end");
  ta.setSelectionRange(caretStart, caretEnd);
  ta.dispatchEvent(new Event("input", { bubbles: true }));
}

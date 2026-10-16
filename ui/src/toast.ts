let timer: number | undefined;

export function toast(msg: string, kind: "ok" | "error" | "info" = "info", ms = 4500) {
  const el = document.getElementById("toast")!;
  el.textContent = msg;
  el.className = `toast ${kind}`;
  el.hidden = false;
  clearTimeout(timer);
  timer = window.setTimeout(() => (el.hidden = true), kind === "error" ? Math.max(ms, 8000) : ms);
  el.onclick = () => (el.hidden = true);
}

/** Small text prompt (window.prompt is not available in every webview). */
export function promptText(title: string, value = ""): Promise<string | null> {
  return new Promise((resolve) => {
    const d = document.createElement("dialog");
    d.className = "modal narrow";
    d.innerHTML = `<div class="modal-head"><h3></h3></div><div class="modal-body"><input type="text" style="width:100%" />
      <div class="row" style="margin-top:12px;justify-content:flex-end"><button data-x="cancel">Cancel</button><button data-x="ok" class="primary">OK</button></div></div>`;
    d.querySelector("h3")!.textContent = title;
    const input = d.querySelector("input")!;
    input.value = value;
    const done = (v: string | null) => {
      d.close();
      d.remove();
      resolve(v);
    };
    d.querySelector<HTMLButtonElement>('[data-x="ok"]')!.onclick = () => done(input.value.trim() || null);
    d.querySelector<HTMLButtonElement>('[data-x="cancel"]')!.onclick = () => done(null);
    input.onkeydown = (e) => {
      if (e.key === "Enter") done(input.value.trim() || null);
    };
    d.oncancel = () => done(null);
    document.body.appendChild(d);
    d.showModal();
    input.select();
  });
}

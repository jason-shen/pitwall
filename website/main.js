document.documentElement.classList.add("js");

const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

for (const btn of document.querySelectorAll("[data-copy]")) {
  btn.addEventListener("click", async () => {
    const label = btn.querySelector(".copy-label");
    try {
      await navigator.clipboard.writeText(btn.dataset.copy);
      btn.classList.add("copied");
      label.textContent = "copied";
    } catch {
      label.textContent = "select it";
    }
    setTimeout(() => {
      btn.classList.remove("copied");
      label.textContent = "copy";
    }, 1800);
  });
}

// Types the brief's Cause section out the way Finish with AI streams it in.
function stream(el) {
  const text = el.dataset.stream;
  if (reduced) {
    el.textContent = text;
    el.classList.add("finished");
    return;
  }
  let i = 0;
  const tick = () => {
    el.textContent = text.slice(0, ++i);
    if (i < text.length) setTimeout(tick, 18 + Math.random() * 30);
    else el.classList.add("finished");
  };
  tick();
}

const seen = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    e.target.classList.add("in", "on");
    const s = e.target.querySelector("[data-stream]");
    if (s) setTimeout(() => stream(s), 500);
    seen.unobserve(e.target);
  }
}, { threshold: 0.25 });

document.querySelectorAll(".reveal, .strike").forEach((el) => seen.observe(el));

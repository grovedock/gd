// Grovedock docs, on top of mdBook's own script: keyboard fixes, ⌘K search, "On this page",
// and numbered steps drawn on the trunk.
(function () {
  // The contents toggle is a <label>; make Enter and Space work on it like a button.
  const toggle = document.getElementById("mdbook-sidebar-toggle");
  if (toggle) {
    toggle.addEventListener("keydown", function (e) {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle.click();
      }
    });
  }
  // Code blocks and tables that scroll sideways must be reachable from the keyboard.
  document.querySelectorAll("pre > code, .table-wrapper").forEach(function (el) {
    if (el.scrollWidth > el.clientWidth) {
      el.setAttribute("tabindex", "0");
      el.setAttribute("role", "region");
      el.setAttribute("aria-label", el.tagName === "CODE" ? "Code" : "Table");
    }
  });

  // ⌘K / Ctrl+K opens search, as in the app.
  const search = document.getElementById("mdbook-search-toggle");
  document.addEventListener("keydown", function (e) {
    if (search && (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      search.click();
    }
  });

  const main = document.querySelector("#mdbook-content main");
  if (!main) return;

  // Numbered steps ("1. Sign up", "2. Install gd"...) are a sequence: draw them on the trunk,
  // each step's text beside its line, the last step (the landing) tipped in gold.
  const steps = Array.from(main.querySelectorAll(":scope > h2")).filter(function (h) {
    return /^\s*\d+\.\s/.test(h.textContent);
  });
  if (steps.length >= 3) {
    main.classList.add("gd-steps");
    steps.forEach(function (h, i) {
      h.classList.add("gd-step");
      const body = document.createElement("div");
      body.className = "gd-step-body";
      let n = h.nextElementSibling;
      while (n && !(n.tagName === "H2" || n.tagName === "H1")) {
        const next = n.nextElementSibling;
        body.appendChild(n);
        n = next;
      }
      h.after(body);
      if (i === steps.length - 1) {
        h.classList.add("gd-step-last");
        body.classList.add("gd-step-last");
      }
    });
  }

  // "On this page": the article's sections, the one being read marked.
  const heads = Array.from(main.querySelectorAll("h2[id], h3[id]"));
  if (heads.length < 3) return;
  const nav = document.createElement("nav");
  nav.className = "gd-toc";
  nav.setAttribute("aria-labelledby", "gd-toc-title");
  nav.innerHTML = '<h2 id="gd-toc-title">On this page</h2><ol></ol>';
  const list = nav.querySelector("ol");
  const links = heads.map(function (h) {
    const li = document.createElement("li");
    if (h.tagName === "H3") li.className = "gd-toc-sub";
    const a = document.createElement("a");
    a.href = "#" + h.id;
    a.textContent = h.textContent.trim();
    li.appendChild(a);
    list.appendChild(li);
    return a;
  });
  document.querySelector("#mdbook-content").appendChild(nav);
  document.documentElement.classList.add("gd-has-toc");
  // The section being read: the last heading above a line just under the bar.
  let ticking = false;
  function mark() {
    ticking = false;
    const line = 120;
    let at = 0;
    heads.forEach(function (h, i) {
      if (h.getBoundingClientRect().top <= line) at = i;
    });
    links.forEach(function (a, i) {
      a.setAttribute("aria-current", i === at ? "true" : "false");
    });
    const current = links[at];
    if (current && nav.scrollHeight > nav.clientHeight) current.scrollIntoView({ block: "nearest" });
  }
  window.addEventListener(
    "scroll",
    function () {
      if (!ticking) {
        ticking = true;
        requestAnimationFrame(mark);
      }
    },
    { passive: true },
  );
  mark();
})();

(function () {
  const themeScriptUrl = document.currentScript?.src;

  function markPage() {
    if (document.querySelector("main .landing-hero")) {
      document.body.classList.add("home-page");
    }
  }

  function enhanceBrand() {
    const title = document.querySelector(".menu-title");
    if (!title) return;
    const iconUrl = themeScriptUrl
      ? new URL("../../theme/rustmux-icon.svg", themeScriptUrl).href
      : "theme/rustmux-icon.svg";
    title.innerHTML = `
      <span class="rustmux-brand">
        <img class="rustmux-header-icon" src="${iconUrl}" alt="" aria-hidden="true">
        <span>Rus<span class="hero-gradient-letter">t</span><span class="hero-wordmark-accent">mux</span></span>
        <span class="rustmux-docs-label">Docs</span>
        <span class="rustmux-version">v0.1</span>
      </span>`;
  }

  function addCopyButtons() {
    for (const pre of document.querySelectorAll("main pre")) {
      const code = pre.querySelector("code");
      if (
        !code ||
        pre.closest(".terminal-window") ||
        pre.querySelector(".copy-button")
      )
        continue;
      const button = document.createElement("button");
      button.className = "copy-button";
      button.type = "button";
      button.textContent = "COPY";
      button.setAttribute("aria-label", "Copy code");
      button.addEventListener("click", async () => {
        await navigator.clipboard.writeText(code.textContent || "");
        button.textContent = "COPIED";
        window.setTimeout(() => (button.textContent = "COPY"), 1200);
      });
      pre.append(button);
    }
  }

  function addTableOfContents() {
    const headings = [...document.querySelectorAll("main h2, main h3")];
    if (headings.length < 2) return;
    const aside = document.createElement("aside");
    aside.className = "right-side-toc";
    const label = "On this page";
    aside.setAttribute("aria-label", label);
    const title = document.createElement("strong");
    title.textContent = label;
    aside.append(title);

    const links = headings.map((heading) => {
      const link = document.createElement("a");
      link.href = `#${heading.id}`;
      link.textContent = heading.textContent || "";
      link.dataset.level = heading.tagName.slice(1);
      aside.append(link);
      return link;
    });
    document.body.append(aside);

    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries.find((entry) => entry.isIntersecting);
        if (!visible) return;
        for (const link of links)
          link.classList.toggle(
            "active",
            link.hash === `#${visible.target.id}`,
          );
      },
      { rootMargin: "-20% 0px -70% 0px" },
    );
    headings.forEach((heading) => observer.observe(heading));
  }

  function enhancePageNavigation() {
    const chapters = [...document.querySelectorAll(".sidebar .chapter a[href]")];
    for (const link of document.querySelectorAll(".nav-wrapper a")) {
      const previous = link.classList.contains("previous");
      const direction = previous ? "Previous" : "Next";
      const chapter = chapters.find((chapter) => chapter.href === link.href);
      const label = document.createElement("span");
      label.className = "page-nav-direction";
      label.textContent = previous ? "← Previous" : "Next →";
      const title = document.createElement("strong");
      title.textContent = chapter?.textContent.trim() || `${direction} chapter`;
      link.replaceChildren(label, title);
      link.setAttribute("aria-label", `${direction}: ${title.textContent}`);
    }
  }

  function bindSearchShortcut() {
    document.addEventListener("keydown", (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        document.querySelector("#mdbook-search-toggle, #search-toggle")?.click();
      }
    });
  }

  function markExternalLinks() {
    for (const link of document.querySelectorAll("main a[href^='http']")) {
      link.target = "_blank";
      link.rel = "noreferrer";
    }
  }

  document.addEventListener("DOMContentLoaded", () => {
    markPage();
    enhanceBrand();
    addCopyButtons();
    addTableOfContents();
    enhancePageNavigation();
    bindSearchShortcut();
    markExternalLinks();
  });
})();

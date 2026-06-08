    function qs(id) {
      return document.getElementById(id);
    }

    const viewNames = new Set([
      "overview",
      "identity",
      "wallet",
      "missions",
      "swarm",
      "social",
      "nearby",
      "organizations",
      "servicenet",
      "runtime",
      "logs",
      "settings",
    ]);

    function pageFromHash() {
      const page = window.location.hash.replace(/^#/, "").trim();
      return viewNames.has(page) ? page : "overview";
    }

    function showPage(page, updateHash = true) {
      const nextPage = viewNames.has(page) ? page : "overview";
      document.querySelectorAll("[data-page]").forEach((section) => {
        section.hidden = section.dataset.page !== nextPage;
      });
      document.querySelectorAll("[data-view]").forEach((link) => {
        const active = link.dataset.view === nextPage;
        link.classList.toggle("active", active);
        if (active) link.setAttribute("aria-current", "page");
        else link.removeAttribute("aria-current");
      });
      if (updateHash) {
        history.replaceState(null, "", `#${nextPage}`);
      }
      if (nextPage === "servicenet") {
        showServiceNetList();
        if (!servicenetAgents.length) {
          refreshServiceNetAgents().catch((error) => setStatus(error.message, true));
        }
      }
    }

    function normalizeToken(raw) {
      let token = String(raw || "").trim();
      if (token.startsWith("Bearer ")) {
        token = token.slice(7).trim();
      }
      if ((token.startsWith('"') && token.endsWith('"')) || (token.startsWith("'") && token.endsWith("'"))) {
        token = token.slice(1, -1).trim();
      }
      return token;
    }

    function syncSwarmConsoleLink() {
      const protocol = window.location.protocol === "https:" ? "https:" : "http:";
      const host = window.location.hostname || "127.0.0.1";
      const href = `${protocol}//${host}:7788`;
      qs("open-swarm-console").href = href;
      qs("side-open-swarm-console").href = href;
    }

    function loadSettings() {
      try {
        const saved = JSON.parse(localStorage.getItem(storageKey) || "{}");
        if (Object.prototype.hasOwnProperty.call(saved, "token")) {
          delete saved.token;
          localStorage.setItem(storageKey, JSON.stringify(saved));
        }
        if (saved.publicId) publicIdEl.dataset.savedPublicId = saved.publicId;
        if (saved.limit) limitEl.value = saved.limit;
      } catch (_) {}
      tokenEl.value = normalizeToken(bootstrapControlToken);
    }

    function saveSettings() {
      const saved = readStoredSettings();
      saved.publicId = publicIdEl.value;
      saved.limit = limitEl.value;
      localStorage.setItem(storageKey, JSON.stringify(saved));
      setStatus("Local console settings saved.");
    }

    const themeOptions = ["teal", "emerald", "forest", "blue-royal", "blue-sky", "indigo"];
    const defaultTheme = "forest";

    function readStoredSettings() {
      try {
        return JSON.parse(localStorage.getItem(storageKey) || "{}") || {};
      } catch (_) {
        return {};
      }
    }

    function applyTheme(theme) {
      const next = themeOptions.includes(theme) ? theme : defaultTheme;
      document.documentElement.setAttribute("data-theme", next);
      document.querySelectorAll("[data-theme-swatch]").forEach((swatch) => {
        const active = swatch.dataset.themeSwatch === next;
        swatch.classList.toggle("active", active);
        if (active) swatch.setAttribute("aria-current", "true");
        else swatch.removeAttribute("aria-current");
      });
    }

    function saveTheme(theme) {
      const saved = readStoredSettings();
      saved.theme = theme;
      localStorage.setItem(storageKey, JSON.stringify(saved));
    }

    function initThemePicker() {
      applyTheme(readStoredSettings().theme || defaultTheme);
      document.querySelectorAll("[data-theme-swatch]").forEach((swatch) => {
        swatch.addEventListener("click", () => {
          const theme = swatch.dataset.themeSwatch;
          applyTheme(theme);
          saveTheme(theme);
        });
      });
    }

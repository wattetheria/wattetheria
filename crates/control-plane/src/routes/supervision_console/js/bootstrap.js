    document.getElementById("load-identities").addEventListener("click", loadIdentities);
    document.getElementById("refresh").addEventListener("click", refreshConsole);
    document.getElementById("save-settings").addEventListener("click", saveSettings);
    qs("identity-display-edit")?.addEventListener("click", editIdentityDisplayName);
    qs("identity-display-cancel")?.addEventListener("click", cancelIdentityDisplayNameEdit);
    qs("identity-display-form")?.addEventListener("submit", saveIdentityDisplayName);
    publicIdEl.addEventListener("change", () => {
      identityDisplayEditing = false;
      syncIdentityDisplayForm();
    });
    document.getElementById("refresh-diagnostics").addEventListener("click", () => {
      refreshDiagnostics().catch((error) => setStatus(error.message, true));
    });
    document.getElementById("export-diagnostics").addEventListener("click", exportDiagnostics);
    qs("missions-search")?.addEventListener("input", (event) => {
      missionSearchQuery = event.target.value;
      missionPage = 1;
      if (lastConsolePayload) renderMissions(lastConsolePayload);
    });
    qs("missions-prev")?.addEventListener("click", () => {
      missionPage = Math.max(1, missionPage - 1);
      if (lastConsolePayload) renderMissions(lastConsolePayload);
    });
    qs("missions-next")?.addEventListener("click", () => {
      missionPage += 1;
      if (lastConsolePayload) renderMissions(lastConsolePayload);
    });
    document.querySelectorAll("[data-log-mode]").forEach((button) => {
      button.addEventListener("click", () => {
        activeLogMode = button.dataset.logMode || "all";
        document.querySelectorAll("[data-log-mode]").forEach((item) => {
          item.classList.toggle("active", item === button);
        });
        renderDiagnostics(lastDiagnosticPayload || { local: {}, swarm: {} }, lastDiagnosticEntries);
      });
    });
    document.querySelectorAll("[data-view]").forEach((link) => {
      link.addEventListener("click", (event) => {
        event.preventDefault();
        showPage(link.dataset.view);
      });
    });
    window.addEventListener("hashchange", () => showPage(pageFromHash(), false));
    tokenEl.addEventListener("change", () => {
      tokenEl.value = normalizeToken(tokenEl.value);
    });
    tokenEl.addEventListener("blur", () => {
      tokenEl.value = normalizeToken(tokenEl.value);
    });

    bindServiceNetControls();
    initThemePicker();
    enhanceAllSelects();
    observeDynamicSelects();
    syncSwarmConsoleLink();
    loadSettings();
    showPage(pageFromHash(), false);
    if (tokenEl.value.trim()) {
      loadIdentities().then(() => {
        if (publicIdEl.value) { refreshConsole(); loadBrainConfig(); }
        else loadBrainConfig();
      });
    }

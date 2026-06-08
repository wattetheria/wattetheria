    async function loadBrainConfig() {
      if (!tokenEl.value.trim()) {
        document.getElementById("brain-config-status").textContent = "Control token required.";
        document.getElementById("brain-config-status").className = "status-text";
        return;
      }
      try {
        const data = await fetchJson("/v1/brain/config", { auth: true });
        const cfg = data.config || {};
        const kind = (cfg && cfg.kind) || "openai-compatible";
        let runtimeLabel = "not configured";
        if (kind === "openai-compatible") {
          document.getElementById("brain-openai-base-url").value = cfg.base_url || "";
          document.getElementById("brain-openai-model").value = cfg.model || "";
          const apiKeyInput = document.getElementById("brain-api-key");
          apiKeyInput.value = "";
          apiKeyInput.placeholder = data.has_api_key
            ? "Configured - enter a new key to replace"
            : "Enter API key";
          runtimeLabel = data.label || kind;
        }
        document.getElementById("brain-provider-label").textContent = runtimeLabel;
        document.getElementById("side-runtime").textContent = runtimeLabel;
        document.getElementById("brain-config-status").textContent = "";
        document.getElementById("brain-config-status").className = "status-text";
      } catch (err) {
        document.getElementById("brain-config-status").textContent = "Load failed: " + err.message;
        document.getElementById("brain-config-status").className = "status-text error";
      }
    }

    async function saveBrainConfig() {
      const kind = "openai-compatible";
      const body = { kind };
      body.base_url = document.getElementById("brain-openai-base-url").value.trim();
      body.model = document.getElementById("brain-openai-model").value.trim();
      const apiKey = document.getElementById("brain-api-key").value.trim();
      if (apiKey) body.api_key = apiKey;
      try {
        const data = await fetchJson("/v1/brain/config", {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
          auth: true,
        });
        document.getElementById("brain-config-status").textContent = data.ok
          ? "Saved to deploy env. Restart required."
          : "Error";
        document.getElementById("brain-config-status").className = data.ok ? "status-text ok" : "status-text error";
        document.getElementById("brain-provider-label").textContent = data.label || "";
        document.getElementById("side-runtime").textContent = data.label || kind;
        if (data.ok) {
          const apiKeyInput = document.getElementById("brain-api-key");
          apiKeyInput.value = "";
          apiKeyInput.placeholder = data.has_api_key
            ? "Configured - enter a new key to replace"
            : "Enter API key";
        }
      } catch (err) {
        document.getElementById("brain-config-status").textContent = "Save failed: " + err.message;
        document.getElementById("brain-config-status").className = "status-text error";
      }
    }

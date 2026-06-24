    let defaultRuntimeBaseUrl = "";
    let supportedRuntimeAdapters = [];
    let configuredSessionHeaderName = "";
    let configuredRuntimeAdapter = "hermes";
    let configuredRuntimeHasApiKey = false;
    const OTHER_RUNTIME_MODEL_PLACEHOLDER = "pi-agent";

    function selectedRuntimeAdapter() {
      return document.querySelector('input[name="brain-runtime-adapter"]:checked')?.value || "hermes";
    }

    function selectedRuntimeSessionMode() {
      return document.querySelector('input[name="runtime-session-mode"]:checked')?.value || "stable";
    }

    function defaultRuntimeModel(adapter) {
      return supportedRuntimeAdapters.find((item) => item.key === adapter)?.default_model || "";
    }

    function isDefaultRuntimeModel(value) {
      const normalized = value.trim();
      return supportedRuntimeAdapters.some((item) => item.default_model && item.default_model === normalized);
    }

    function runtimeSessionHeaderName(adapter) {
      return supportedRuntimeAdapters.find((item) => item.key === adapter)?.session_header_name || "";
    }

    function isDefaultRuntimeHeader(value) {
      const normalized = value.trim();
      return supportedRuntimeAdapters.some((item) => item.session_header_name && item.session_header_name === normalized);
    }

    function runtimeAdapterLabel(adapter) {
      return supportedRuntimeAdapters.find((item) => item.key === adapter)?.label || adapter;
    }

    function renderRuntimeAdapterMetadata(selectedAdapter) {
      const target = document.getElementById("runtime-adapter-options");
      target.innerHTML = "";
      supportedRuntimeAdapters.forEach((adapter) => {
        const option = document.createElement("label");
        option.className = "runtime-adapter-option";
        const input = document.createElement("input");
        input.type = "radio";
        input.name = "brain-runtime-adapter";
        input.value = adapter.key;
        input.dataset.previousAdapter = selectedAdapter || "hermes";
        input.checked = adapter.key === (selectedAdapter || "hermes");
        const name = document.createElement("span");
        name.className = "runtime-adapter-name";
        name.textContent = adapter.label || adapter.key;
        option.append(input, name);
        target.append(option);
      });
      target.querySelectorAll('input[name="brain-runtime-adapter"]').forEach((input) => {
        input.addEventListener("change", (event) => {
          applyRuntimeAdapterDefaults(event.target?.dataset?.previousAdapter || "hermes");
          target.querySelectorAll('input[name="brain-runtime-adapter"]').forEach((option) => {
            option.dataset.previousAdapter = selectedRuntimeAdapter();
          });
        });
      });

      const headerExamples = document.getElementById("runtime-session-header-examples");
      headerExamples.innerHTML = "";
      headerExamples.append(document.createTextNode("Default session headers"));
      supportedRuntimeAdapters.forEach((adapter) => {
        if (!adapter.session_header_name) return;
        const line = document.createElement("span");
        line.className = "runtime-example-code";
        line.textContent = `${adapter.label || adapter.key}: ${adapter.session_header_name}`;
        headerExamples.append(line);
      });
      const othersLine = document.createElement("span");
      othersLine.className = "runtime-example-code";
      othersLine.textContent = "Others: enter the exact HTTP header name your runtime uses for session or thread continuity.";
      headerExamples.append(othersLine);
    }

    function updateApiKeyPlaceholder() {
      const apiKeyInput = document.getElementById("brain-api-key");
      apiKeyInput.placeholder = selectedRuntimeAdapter() === configuredRuntimeAdapter && configuredRuntimeHasApiKey
        ? "Configured - enter a new key to replace"
        : "Enter API key";
    }

    function updateModelPlaceholder() {
      const modelInput = document.getElementById("brain-openai-model");
      modelInput.placeholder = defaultRuntimeModel(selectedRuntimeAdapter()) || OTHER_RUNTIME_MODEL_PLACEHOLDER;
    }

    function applyRuntimeAdapterDefaults(previousAdapter) {
      const adapter = selectedRuntimeAdapter();
      const headerInput = document.getElementById("brain-session-header-name");
      const modelInput = document.getElementById("brain-openai-model");
      const baseUrlInput = document.getElementById("brain-openai-base-url");

      if (!baseUrlInput.value.trim()) {
        baseUrlInput.value = defaultRuntimeBaseUrl;
      }

      const previousDefaultModel = defaultRuntimeModel(previousAdapter);
      const nextDefaultModel = defaultRuntimeModel(adapter);
      if (!modelInput.value.trim()
        || modelInput.value.trim() === previousDefaultModel
        || (!nextDefaultModel && isDefaultRuntimeModel(modelInput.value))) {
        modelInput.value = nextDefaultModel;
      }

      const previousHeader = runtimeSessionHeaderName(previousAdapter);
      const nextHeader = runtimeSessionHeaderName(adapter);
      if (!headerInput.value.trim()
        || headerInput.value.trim() === previousHeader
        || (!nextHeader && isDefaultRuntimeHeader(headerInput.value))) {
        headerInput.value = nextHeader;
      }
      updateApiKeyPlaceholder();
      updateModelPlaceholder();
    }

    function setRuntimeAdapter(adapter) {
      const value = adapter || "hermes";
      const input = document.querySelector(`input[name="brain-runtime-adapter"][value="${value}"]`);
      (input || document.querySelector('input[name="brain-runtime-adapter"][value="hermes"]')).checked = true;
    }

    function setRuntimeSessionMode(mode) {
      const value = mode || "stable";
      const input = document.querySelector(`input[name="runtime-session-mode"][value="${value}"]`);
      (input || document.querySelector('input[name="runtime-session-mode"][value="stable"]')).checked = true;
    }

    async function loadBrainConfig() {
      if (!tokenEl.value.trim()) {
        document.getElementById("brain-config-status").textContent = "Control token required.";
        document.getElementById("brain-config-status").className = "status-text";
        return;
      }
      try {
        const data = await fetchJson("/v1/brain/config", { auth: true });
        supportedRuntimeAdapters = data.supported_runtime_adapters || [];
        defaultRuntimeBaseUrl = data.default_runtime_base_url || "";
        setRuntimeSessionMode(data.runtime_session_mode || "stable");
        const cfg = data.config || {};
        const kind = (cfg && cfg.kind) || "openai-compatible";
        let runtimeLabel = "not configured";
        if (kind === "openai-compatible") {
          const adapter = cfg.runtime_adapter || {};
          const adapterKind = adapter.kind || data.runtime_adapter || "hermes";
          configuredRuntimeAdapter = adapterKind;
          configuredRuntimeHasApiKey = data.has_api_key === true;
          configuredSessionHeaderName = adapter.session_header_name || data.session_header_name || "";
          renderRuntimeAdapterMetadata(adapterKind);
          setRuntimeAdapter(adapterKind);
          document.querySelectorAll('input[name="brain-runtime-adapter"]').forEach((option) => {
            option.dataset.previousAdapter = adapterKind;
          });
          document.getElementById("brain-session-header-name").value =
            configuredSessionHeaderName || runtimeSessionHeaderName(adapterKind);
          document.getElementById("brain-openai-base-url").value = cfg.base_url || defaultRuntimeBaseUrl;
          document.getElementById("brain-openai-model").value = cfg.model || defaultRuntimeModel(adapterKind);
          const apiKeyInput = document.getElementById("brain-api-key");
          apiKeyInput.value = "";
          updateApiKeyPlaceholder();
          updateModelPlaceholder();
          runtimeLabel = data.label || kind;
        } else {
          configuredRuntimeAdapter = "hermes";
          configuredRuntimeHasApiKey = false;
          configuredSessionHeaderName = "";
          renderRuntimeAdapterMetadata("hermes");
          setRuntimeAdapter("hermes");
          document.querySelectorAll('input[name="brain-runtime-adapter"]').forEach((option) => {
            option.dataset.previousAdapter = "hermes";
          });
          document.getElementById("brain-session-header-name").value = runtimeSessionHeaderName("hermes");
          document.getElementById("brain-openai-base-url").value = defaultRuntimeBaseUrl;
          document.getElementById("brain-openai-model").value = defaultRuntimeModel("hermes");
          document.getElementById("brain-api-key").value = "";
          updateApiKeyPlaceholder();
          updateModelPlaceholder();
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
      body.adapter = selectedRuntimeAdapter();
      body.session_header_name = document.getElementById("brain-session-header-name").value.trim();
      body.runtime_session_mode = selectedRuntimeSessionMode();
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
          configuredRuntimeAdapter = body.adapter;
          configuredRuntimeHasApiKey = apiKey ? true : data.has_api_key === true;
          configuredSessionHeaderName = body.session_header_name || "";
          const apiKeyInput = document.getElementById("brain-api-key");
          apiKeyInput.value = "";
          updateApiKeyPlaceholder();
        }
      } catch (err) {
        document.getElementById("brain-config-status").textContent = "Save failed: " + err.message;
        document.getElementById("brain-config-status").className = "status-text error";
      }
    }

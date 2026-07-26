    let agentIdentityModalGeneration = 0;

    function agentIdentityStatus(message, isError = false) {
      const target = qs("agent-identity-status");
      if (!target) return;
      target.textContent = message;
      target.className = isError ? "status-text error" : "status-text";
    }

    function showAgentIdentityKind(kind) {
      const selectedKind = ["runtime", "service", "provider"].includes(kind) ? kind : "runtime";
      qs("runtime-agent-identity-view").hidden = selectedKind !== "runtime";
      qs("service-agent-identities-view").hidden = selectedKind !== "service";
      qs("provider-identity-view").hidden = selectedKind !== "provider";
      document.querySelectorAll("[data-agent-identity-kind]").forEach((button) => {
        const active = button.dataset.agentIdentityKind === selectedKind;
        button.classList.toggle("active", active);
        button.setAttribute("aria-selected", String(active));
      });
    }

    function showIdentitySubTab(kind, tab) {
      document.querySelectorAll(`[data-${kind}-identity-tab]`).forEach((button) => {
        const active = button.dataset[`${kind}IdentityTab`] === tab;
        button.classList.toggle("active", active);
        button.setAttribute("aria-selected", String(active));
      });
      if (kind === "runtime") {
        document.querySelectorAll("[data-runtime-identity-panel]").forEach((panel) => {
          panel.hidden = panel.dataset.runtimeIdentityPanel !== tab;
        });
        if (tab === "credentials") renderRuntimeAgentCredentials();
        return;
      }
      if (kind === "provider") {
        document.querySelectorAll("[data-provider-identity-panel]").forEach((panel) => {
          panel.hidden = panel.dataset.providerIdentityPanel !== tab;
        });
        if (tab === "credentials") renderProviderCredentials();
        return;
      }
      renderSelectedServiceAgentIdentity(tab);
    }

    function renderRuntimeIdentityManagement() {
      const pending = runtimeAgentIdentity?.pending_import;
      const notice = qs("runtime-identity-pending");
      const pendingDid = qs("runtime-identity-pending-did");
      if (!notice) return;
      notice.hidden = !pending;
      if (pendingDid) pendingDid.textContent = pending
        ? `${compactId(pending.agent_did, 42)} will activate when the node restarts.`
        : "";
    }

    function serviceAgentIdentityRow(identity) {
      const selected = identity.service_agent_identity_id === selectedServiceAgentIdentityId;
      const agentName = String(identity.agent_name || "").trim();
      const serviceAddress = String(identity.service_address || "").trim();
      return `
        <tr data-service-agent-identity="${escapeHtml(identity.service_agent_identity_id)}" class="${selected ? "selected" : ""}">
          <td class="service-agent-identity-summary">
            ${agentName ? `<strong>${escapeHtml(agentName)}</strong>` : ""}
            ${serviceAddress ? `<span>${escapeHtml(serviceAddress)}</span>` : ""}
          </td>
          <td class="mono">${escapeHtml(compactId(identity.service_did, 28))}</td>
          <td>${escapeHtml(identity.bound_agent_id ? compactId(identity.bound_agent_id, 24) : "Unbound")}</td>
          <td>${escapeHtml(identity.key_origin || "-")}</td>
          <td><span class="identity-card-state ${escapeHtml(identity.agent_card_status || "draft")}">${escapeHtml(identity.agent_card_status || "draft")}</span></td>
          <td class="mono">${escapeHtml(compactId(identity.public_key, 22))}</td>
          <td class="service-agent-identity-actions">
            <button class="secondary" type="button" data-service-agent-export="${escapeHtml(identity.service_agent_identity_id)}">Export</button>
            <button class="secondary danger" type="button" data-service-agent-delete="${escapeHtml(identity.service_agent_identity_id)}">Delete</button>
          </td>
        </tr>
      `;
    }

    function renderServiceAgentIdentities() {
      const target = qs("service-agent-identities-list");
      const count = qs("service-agent-identity-count");
      if (!target) return;
      if (count) count.textContent = `${serviceAgentIdentities.length} identities`;
      if (!serviceAgentIdentities.length) {
        target.innerHTML = '<div class="identity-empty-state"><strong>No Service Agent DIDs</strong><span>Generate a new DID or import an existing private key.</span></div>';
        qs("service-agent-identity-detail").hidden = true;
        return;
      }
      target.innerHTML = `
        <table class="service-agent-identity-table">
          <thead><tr><th>Service Agent</th><th>Service DID</th><th>Binding</th><th>Key Origin</th><th>Agent Card</th><th>Public Key</th><th>Actions</th></tr></thead>
          <tbody>${serviceAgentIdentities.map(serviceAgentIdentityRow).join("")}</tbody>
        </table>
      `;
      target.querySelectorAll("[data-service-agent-export]").forEach((button) => {
        button.addEventListener("click", (event) => {
          event.stopPropagation();
          exportIdentityBackup("service", button.dataset.serviceAgentExport)
            .catch((error) => agentIdentityStatus(error.message, true));
        });
      });
      target.querySelectorAll("[data-service-agent-delete]").forEach((button) => {
        button.addEventListener("click", (event) => {
          event.stopPropagation();
          deleteServiceAgentIdentity(button.dataset.serviceAgentDelete)
            .catch((error) => agentIdentityStatus(error.message, true));
        });
      });
      target.querySelectorAll("[data-service-agent-identity]").forEach((row) => {
        row.addEventListener("click", () => {
          selectedServiceAgentIdentityId = row.dataset.serviceAgentIdentity;
          renderServiceAgentIdentities();
          qs("service-agent-identity-detail").hidden = false;
          showIdentitySubTab("service", "overview");
        });
      });
      if (selectedServiceAgentIdentityId) {
        qs("service-agent-identity-detail").hidden = false;
        renderSelectedServiceAgentIdentity("overview");
      }
    }

    function renderSelectedServiceAgentIdentity(tab = "overview") {
      const identity = serviceAgentIdentities.find(
        (item) => item.service_agent_identity_id === selectedServiceAgentIdentityId,
      );
      const detail = qs("service-agent-identity-detail");
      const body = qs("service-agent-identity-detail-body");
      if (!identity || !detail || !body) return;
      detail.hidden = false;
      document.querySelectorAll("[data-service-identity-tab]").forEach((button) => {
        const active = button.dataset.serviceIdentityTab === tab;
        button.classList.toggle("active", active);
        button.setAttribute("aria-selected", String(active));
      });
      if (tab === "credentials") {
        renderServiceAgentCredentials(identity, body);
        return;
      }
      if (tab === "presentations") {
        body.innerHTML = '<div class="identity-empty-state"><strong>No verifiable presentations</strong><span>This Service Agent has no Verifiable Presentations.</span></div>';
        return;
      }
      if (tab === "agent-card") {
        body.innerHTML = `
          <div class="identity-spec-grid">
            ${identitySpecSection("Agent Card", identitySpecRows([
              ["status", identity.agent_card_status],
              ["endpoint", identity.endpoint_url || "Not published"],
              ["service_did", compactId(identity.service_did, 34), true],
            ]))}
          </div>
        `;
        return;
      }
      body.innerHTML = `
        <div class="identity-spec-grid">
          ${identitySpecSection("Service Agent", identitySpecRows([
            ["service_did", compactId(identity.service_did, 34), true],
            ["public_key", compactId(identity.public_key, 30), true],
            ["key_origin", identity.key_origin],
          ]))}
          ${identitySpecSection("Publication", identitySpecRows([
            ["binding", identity.bound_agent_id || "Unbound"],
            ["agent_card", identity.agent_card_status],
            ["endpoint", identity.endpoint_url || "Not published"],
          ]))}
        </div>
      `;
    }

    function renderProviderIdentityManagement() {
      const target = qs("provider-identity-overview");
      if (!target) return;
      if (!providerIdentity) {
        target.innerHTML = '<div class="identity-empty-state"><strong>Provider Identity unavailable</strong></div>';
        return;
      }
      target.innerHTML = `
        <div class="identity-spec-grid">
          ${identitySpecSection("Provider Identity", identitySpecRows([
            ["status", providerIdentity.status],
            ["provider_did", compactId(providerIdentity.provider_did, 42), true],
            ["fingerprint", providerIdentity.fingerprint, true],
            ["public_key", compactId(providerIdentity.public_key, 34), true],
          ]))}
          ${identitySpecSection("Provider Authority", identitySpecRows([
            ["identity_uri", providerIdentity.identity_uri, true],
            ["managed_service_agents", providerIdentity.managed_service_agents],
            ["scope", "Service Agent publication"],
          ]))}
        </div>
      `;
    }

    async function loadManagedAgentIdentities() {
      const [runtime, services, provider, runtimeCredentials, providerCredentialResponse] = await Promise.all([
        fetchJson("/v1/wattetheria/agent-identities/runtime", { auth: true }),
        fetchJson("/v1/wattetheria/agent-identities/service-agents", { auth: true }),
        fetchJson("/v1/wattetheria/provider-identity", { auth: true }),
        fetchJson("/v1/wattetheria/agent-identities/runtime/credentials", { auth: true }),
        fetchJson("/v1/wattetheria/provider-identity/credentials", { auth: true }),
      ]);
      runtimeAgentIdentity = runtime;
      providerIdentity = provider;
      runtimeAgentCredentials = safeArray(runtimeCredentials.items);
      providerCredentials = safeArray(providerCredentialResponse.items);
      serviceAgentIdentities = safeArray(services.items);
      if (!serviceAgentIdentities.some((item) => item.service_agent_identity_id === selectedServiceAgentIdentityId)) {
        selectedServiceAgentIdentityId = "";
      }
      renderRuntimeIdentityManagement();
      renderProviderIdentityManagement();
      renderRuntimeAgentCredentials();
      renderProviderCredentials();
      renderServiceAgentIdentities();
      syncServiceAgentIdentitySelect();
    }

    function syncServiceAgentIdentitySelect(selected) {
      const select = qs("servicenet-identity-id");
      if (!select) return;
      const current = selected === undefined ? select.value : selected;
      const availableIdentities = serviceAgentIdentities.filter((identity) => (
        identity.service_agent_identity_id === current
          || (!identity.bound_agent_id && identity.agent_card_status !== "published")
      ));
      select.innerHTML = [
        `<option value="">${availableIdentities.length ? "Select Service Agent DID" : "No unpublished Service Agent DID available"}</option>`,
        ...availableIdentities.map((identity) => (
          `<option value="${escapeHtml(identity.service_agent_identity_id)}">${escapeHtml(compactId(identity.service_did, 31))}${identity.service_agent_identity_id === current && identity.bound_agent_id ? ` - ${escapeHtml(`Bound to ${compactId(identity.bound_agent_id, 18)}`)}` : ""}</option>`
        )),
      ].join("");
      if (availableIdentities.some((identity) => identity.service_agent_identity_id === current)) {
        select.value = current;
      }
    }

    function openAgentIdentityModal(operation) {
      const runtime = operation === "runtime-import";
      agentIdentityModalGeneration += 1;
      qs("agent-identity-operation").value = operation;
      qs("agent-identity-confirmed-did").value = "";
      qs("agent-identity-modal-title").textContent = runtime
        ? "Import Runtime Agent DID"
        : "Import Service Agent DID";
      qs("agent-identity-did-label").textContent = runtime
        ? "Expected Agent DID (optional)"
        : "Expected Service DID (optional)";
      qs("runtime-identity-import-warning").hidden = !runtime;
      qs("agent-identity-did-field").hidden = false;
      qs("agent-identity-private-key-field").hidden = false;
      qs("agent-identity-import-preview").hidden = true;
      qs("agent-identity-did").value = "";
      qs("agent-identity-private-key").value = "";
      qs("agent-identity-key-form").dataset.phase = "input";
      qs("agent-identity-modal-back").hidden = true;
      qs("agent-identity-modal-submit").textContent = "Review Import";
      qs("agent-identity-modal-status").textContent = "";
      qs("agent-identity-modal-status").className = "status-text";
      qs("agent-identity-modal-submit").disabled = false;
      setAgentIdentityModalDismissalDisabled(false);
      qs("agent-identity-modal").hidden = false;
      qs("agent-identity-private-key")?.focus();
    }

    function setAgentIdentityModalDismissalDisabled(disabled) {
      qs("agent-identity-modal-close").disabled = disabled;
      qs("agent-identity-modal-cancel").disabled = disabled;
      qs("agent-identity-modal-back").disabled = disabled;
    }

    function resetAgentIdentityModal() {
      agentIdentityModalGeneration += 1;
      qs("agent-identity-did").value = "";
      qs("agent-identity-confirmed-did").value = "";
      qs("agent-identity-private-key").value = "";
      qs("agent-identity-import-preview").hidden = true;
      qs("agent-identity-key-form").dataset.phase = "input";
      qs("agent-identity-modal-status").textContent = "";
      qs("agent-identity-modal-status").className = "status-text";
      setAgentIdentityModalDismissalDisabled(false);
      qs("agent-identity-modal").hidden = true;
    }

    function closeAgentIdentityModal() {
      if (qs("agent-identity-key-form").dataset.phase === "submitting") return;
      resetAgentIdentityModal();
    }

    function showRuntimeIdentityImportInput() {
      const runtime = qs("agent-identity-operation").value === "runtime-import";
      qs("agent-identity-confirmed-did").value = "";
      qs("runtime-identity-import-warning").hidden = !runtime;
      qs("agent-identity-did-field").hidden = false;
      qs("agent-identity-private-key-field").hidden = false;
      qs("agent-identity-import-preview").hidden = true;
      qs("agent-identity-key-form").dataset.phase = "input";
      qs("agent-identity-modal-title").textContent = runtime
        ? "Import Runtime Agent DID"
        : "Import Service Agent DID";
      qs("agent-identity-modal-back").hidden = true;
      qs("agent-identity-modal-submit").textContent = "Review Import";
      qs("agent-identity-modal-status").textContent = "";
      qs("agent-identity-modal-status").className = "status-text";
      qs("agent-identity-private-key")?.focus();
    }

    function showRuntimeIdentityImportPreview(preview, runtime) {
      const did = runtime ? preview.agent_did : preview.service_did;
      qs("agent-identity-confirmed-did").value = did || "";
      qs("agent-identity-preview-title").textContent = runtime
        ? "Derived Runtime Agent Identity"
        : "Derived Service Agent Identity";
      qs("agent-identity-preview-did-label").textContent = runtime
        ? "agent_did"
        : "service_did";
      qs("agent-identity-preview-did").textContent = did || "";
      qs("agent-identity-preview-uri").textContent = preview.identity_uri || "";
      qs("agent-identity-preview-fingerprint").textContent = preview.fingerprint || "";
      qs("agent-identity-did-field").hidden = true;
      qs("agent-identity-private-key-field").hidden = true;
      qs("agent-identity-import-preview").hidden = false;
      qs("agent-identity-key-form").dataset.phase = "preview";
      qs("agent-identity-modal-title").textContent = runtime
        ? "Confirm Runtime Agent DID"
        : "Confirm Service Agent DID";
      qs("agent-identity-modal-back").hidden = false;
      qs("agent-identity-modal-submit").textContent = "Confirm Import";
      qs("agent-identity-modal-status").textContent = "Nothing has been imported yet.";
      qs("agent-identity-modal-status").className = "status-text";
      qs("agent-identity-modal-submit")?.focus();
    }

    function credentialRows(credentials, ownerKind, agentId = "") {
      if (!credentials.length) {
        return '<div class="identity-empty-state"><strong>No verifiable credentials</strong><span>No W3C Verifiable Credentials are stored for this Agent DID.</span></div>';
      }
      return `
        <div class="credential-list">
          <table class="credential-table">
            <thead><tr><th>Credential</th><th>Format</th><th>Verification</th><th>Imported</th><th>Action</th></tr></thead>
            <tbody>
              ${credentials.map((credential) => `
                <tr>
                  <td class="mono">${escapeHtml(compactId(credential.credential_id, 30))}</td>
                  <td>${escapeHtml(credential.format || "-")}</td>
                  <td>
                    <span class="credential-state ${escapeHtml(credential.verification_status)}">${escapeHtml(credential.verification_status)}</span>
                    <span class="credential-verification-detail">${escapeHtml([
                      credential.proof_outcome && `proof: ${credential.proof_outcome}`,
                      credential.credential_state && `status: ${credential.credential_state}`,
                      credential.trust_outcome && `trust: ${credential.trust_outcome}`,
                    ].filter(Boolean).join(" / ") || "adapter verification pending")}</span>
                  </td>
                  <td>${escapeHtml(credential.imported_at || "-")}</td>
                  <td><button class="secondary credential-delete" type="button" data-credential-delete="${escapeHtml(credential.credential_id)}" data-credential-owner="${escapeHtml(ownerKind)}" data-credential-agent-id="${escapeHtml(agentId)}">Delete</button></td>
                </tr>
              `).join("")}
            </tbody>
          </table>
        </div>
      `;
    }

    function bindCredentialDeleteControls(container) {
      container?.querySelectorAll("[data-credential-delete]").forEach((button) => {
        button.addEventListener("click", () => {
          deleteAgentCredential(
            button.dataset.credentialOwner,
            button.dataset.credentialAgentId,
            button.dataset.credentialDelete,
          ).catch((error) => agentIdentityStatus(error.message, true));
        });
      });
    }

    function renderRuntimeAgentCredentials() {
      const target = qs("runtime-agent-credentials");
      if (!target) return;
      target.innerHTML = credentialRows(runtimeAgentCredentials, "runtime");
      bindCredentialDeleteControls(target);
    }

    function renderProviderCredentials() {
      const target = qs("provider-agent-credentials");
      if (!target) return;
      target.innerHTML = credentialRows(providerCredentials, "provider");
      bindCredentialDeleteControls(target);
    }

    function renderServiceAgentCredentials(identity, target) {
      const credentials = serviceAgentCredentialsById.get(identity.service_agent_identity_id);
      target.innerHTML = `
        <div class="credential-toolbar">
          <div>
            <h3>${escapeHtml(compactId(identity.service_did, 34))} Credentials</h3>
            <span class="subtle">Credentials bound to ${escapeHtml(compactId(identity.service_did, 34))}</span>
          </div>
          <button id="service-agent-credential-import" type="button">Import Credential</button>
        </div>
        <div id="service-agent-credentials">
          ${credentials ? credentialRows(credentials, "service", identity.service_agent_identity_id) : '<div class="identity-empty-state"><span>Loading credentials...</span></div>'}
        </div>
      `;
      qs("service-agent-credential-import")?.addEventListener("click", () => {
        openAgentCredentialModal("service", identity.service_agent_identity_id);
      });
      bindCredentialDeleteControls(qs("service-agent-credentials"));
      if (!credentials) {
        loadServiceAgentCredentials(identity.service_agent_identity_id).catch((error) => {
          target.innerHTML = `<div class="identity-empty-state"><strong>Could not load credentials</strong><span>${escapeHtml(error.message)}</span></div>`;
        });
      }
    }

    async function loadServiceAgentCredentials(agentId) {
      const response = await fetchJson(
        `/v1/wattetheria/agent-identities/service-agents/${encodeURIComponent(agentId)}/credentials`,
        { auth: true },
      );
      serviceAgentCredentialsById.set(agentId, safeArray(response.items));
      if (selectedServiceAgentIdentityId === agentId) {
        renderSelectedServiceAgentIdentity("credentials");
      }
    }

    function openAgentCredentialModal(ownerKind, agentId = "") {
      const identity = ownerKind === "runtime"
        ? runtimeAgentIdentity?.identity
        : ownerKind === "provider"
          ? providerIdentity
          : serviceAgentIdentities.find((item) => item.service_agent_identity_id === agentId);
      qs("agent-credential-owner-kind").value = ownerKind;
      qs("agent-credential-agent-id").value = agentId;
      qs("agent-credential-modal-title").textContent = {
        runtime: "Import Runtime Agent Credential",
        provider: "Import Provider Credential",
        service: "Import Service Agent Credential",
      }[ownerKind];
      qs("agent-credential-format").value = "w3c_vc_json";
      qs("agent-credential-media-type").value = "application/vc+ld+json";
      qs("agent-credential-payload").value = "";
      qs("agent-credential-modal-status").textContent = identity
        ? `Credential will be bound to ${compactId(identity.provider_did || identity.agent_did || identity.service_did, 42)}.`
        : "";
      qs("agent-credential-modal").hidden = false;
      qs("agent-credential-payload").focus();
    }

    function closeAgentCredentialModal() {
      qs("agent-credential-payload").value = "";
      qs("agent-credential-modal-status").textContent = "";
      qs("agent-credential-modal").hidden = true;
    }

    async function submitAgentCredential(event) {
      event.preventDefault();
      const ownerKind = qs("agent-credential-owner-kind").value;
      const agentId = qs("agent-credential-agent-id").value;
      const payload = qs("agent-credential-payload").value;
      const body = {
        format: qs("agent-credential-format").value,
        media_type: qs("agent-credential-media-type").value.trim() || null,
        payload,
      };
      const path = ownerKind === "runtime"
        ? "/v1/wattetheria/agent-identities/runtime/credentials"
        : ownerKind === "provider"
          ? "/v1/wattetheria/provider-identity/credentials"
          : `/v1/wattetheria/agent-identities/service-agents/${encodeURIComponent(agentId)}/credentials`;
      qs("agent-credential-payload").value = "";
      qs("agent-credential-modal-status").textContent = "Importing...";
      try {
        await fetchJson(path, {
          method: "POST",
          auth: true,
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        });
        if (ownerKind === "runtime") {
          const response = await fetchJson(path, { auth: true });
          runtimeAgentCredentials = safeArray(response.items);
          renderRuntimeAgentCredentials();
        } else if (ownerKind === "provider") {
          const response = await fetchJson(path, { auth: true });
          providerCredentials = safeArray(response.items);
          renderProviderCredentials();
        } else {
          await loadServiceAgentCredentials(agentId);
        }
        closeAgentCredentialModal();
        agentIdentityStatus("Verifiable Credential imported and awaiting adapter verification.");
      } catch (error) {
        qs("agent-credential-modal-status").textContent = error.message;
        qs("agent-credential-modal-status").className = "status-text error";
      } finally {
        qs("agent-credential-payload").value = "";
      }
    }

    async function deleteAgentCredential(ownerKind, agentId, credentialId) {
      if (!window.confirm("Delete this stored Verifiable Credential?")) return;
      const base = ownerKind === "runtime"
        ? "/v1/wattetheria/agent-identities/runtime/credentials"
        : ownerKind === "provider"
          ? "/v1/wattetheria/provider-identity/credentials"
          : `/v1/wattetheria/agent-identities/service-agents/${encodeURIComponent(agentId)}/credentials`;
      await fetchJson(`${base}/${encodeURIComponent(credentialId)}`, {
        method: "DELETE",
        auth: true,
      });
      if (ownerKind === "runtime") {
        const response = await fetchJson(base, { auth: true });
        runtimeAgentCredentials = safeArray(response.items);
        renderRuntimeAgentCredentials();
      } else if (ownerKind === "provider") {
        const response = await fetchJson(base, { auth: true });
        providerCredentials = safeArray(response.items);
        renderProviderCredentials();
      } else {
        serviceAgentCredentialsById.delete(agentId);
        await loadServiceAgentCredentials(agentId);
      }
      agentIdentityStatus("Verifiable Credential deleted.");
    }

    async function submitAgentIdentityOperation(event) {
      event.preventDefault();
      const operation = qs("agent-identity-operation").value;
      const did = qs("agent-identity-did").value.trim();
      const privateKey = qs("agent-identity-private-key").value.trim();
      const status = qs("agent-identity-modal-status");
      const submit = qs("agent-identity-modal-submit");
      const phase = qs("agent-identity-key-form").dataset.phase || "input";
      const confirmedDid = qs("agent-identity-confirmed-did").value.trim();
      const generation = agentIdentityModalGeneration;
      const runtime = operation === "runtime-import";
      const isPreviewRequest = phase === "input";
      let path;
      let body;
      if (phase === "preview" && !confirmedDid) {
        status.textContent = `Preview the ${runtime ? "Runtime" : "Service"} Agent DID before confirming.`;
        status.className = "status-text error";
        return;
      }
      if (runtime) {
        path = phase === "input"
          ? "/v1/wattetheria/agent-identities/runtime/import/preview"
          : "/v1/wattetheria/agent-identities/runtime/import";
        body = {
          agent_did: phase === "preview" ? confirmedDid : did || null,
          private_key: privateKey,
        };
      } else {
        path = phase === "input"
          ? "/v1/wattetheria/agent-identities/service-agents/import/preview"
          : "/v1/wattetheria/agent-identities/service-agents/import";
        body = {
          service_did: phase === "preview" ? confirmedDid : did || null,
          private_key: privateKey,
        };
      }
      status.textContent = phase === "input"
        ? "Parsing private key..."
        : "Importing...";
      status.className = "status-text";
      submit.disabled = true;
      if (!isPreviewRequest) {
        qs("agent-identity-key-form").dataset.phase = "submitting";
        setAgentIdentityModalDismissalDisabled(true);
      }
      try {
        let result;
        try {
          result = await fetchJson(path, {
            method: "POST",
            auth: true,
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          });
        } catch (error) {
          if (generation !== agentIdentityModalGeneration) return;
          qs("agent-identity-key-form").dataset.phase = phase;
          setAgentIdentityModalDismissalDisabled(false);
          status.textContent = error.message;
          status.className = "status-text error";
          return;
        }
        if (
          generation !== agentIdentityModalGeneration
          || qs("agent-identity-operation").value !== operation
        ) return;
        if (phase === "input") {
          showRuntimeIdentityImportPreview(result, runtime);
          return;
        }
        resetAgentIdentityModal();
        try {
          await loadManagedAgentIdentities();
        } catch (error) {
          agentIdentityStatus(
            `Import succeeded, but the identity view could not refresh. Reload the page. ${error.message}`,
            true,
          );
          return;
        }
        agentIdentityStatus(
          operation === "runtime-import"
            ? ""
            : "Service Agent identity saved.",
        );
      } finally {
        if (generation === agentIdentityModalGeneration) submit.disabled = false;
      }
    }

    async function deleteServiceAgentIdentity(identityId) {
      const identity = serviceAgentIdentities.find((item) => item.service_agent_identity_id === identityId);
      if (!identity) return;
      const confirmed = await confirmDialog({
        title: "Delete Service Agent DID",
        message: `Delete ${compactId(identity.service_did, 42)} and its locally stored private key? This cannot be undone.`,
        confirmText: "Delete",
        cancelText: "Cancel",
        danger: true,
      });
      if (!confirmed) return;
      agentIdentityStatus("Deleting Service Agent DID...");
      await fetchJson(
        `/v1/wattetheria/agent-identities/service-agents/${encodeURIComponent(identity.service_agent_identity_id)}`,
        { method: "DELETE", auth: true },
      );
      serviceAgentCredentialsById.delete(identity.service_agent_identity_id);
      if (selectedServiceAgentIdentityId === identity.service_agent_identity_id) {
        selectedServiceAgentIdentityId = "";
      }
      await loadManagedAgentIdentities();
      agentIdentityStatus("Service Agent DID deleted.");
    }

    function downloadIdentityBackup(backup, filename) {
      const blob = new Blob([`${JSON.stringify(backup, null, 2)}\n`], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      document.body.appendChild(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
    }

    function identityBackupFilename(ownerKind, did) {
      const suffix = String(did || "")
        .replace(/[^a-zA-Z0-9]/g, "")
        .slice(-16) || "identity";
      const identityKind = ownerKind === "provider" ? "provider" : `${ownerKind}-agent`;
      return `wattetheria-${identityKind}-did-${suffix}.json`;
    }

    async function exportIdentityBackup(ownerKind, identityId = "") {
      const runtime = ownerKind === "runtime";
      const provider = ownerKind === "provider";
      const identity = runtime
        ? runtimeAgentIdentity?.identity
        : provider
          ? providerIdentity
          : serviceAgentIdentities.find((item) => item.service_agent_identity_id === identityId);
      if (!identity) throw new Error("DID is not available for export.");
      const did = identity.provider_did || identity.agent_did || identity.service_did;
      const identityLabel = runtime ? "Runtime Agent" : provider ? "Provider" : "Service Agent";
      const confirmed = await confirmDialog({
        title: `Export ${identityLabel} DID`,
        message: `The exported file contains the private key for ${compactId(did, 42)}. Anyone with this file controls the DID. Store it securely.`,
        confirmText: "Export DID",
        cancelText: "Cancel",
      });
      if (!confirmed) return;
      const path = runtime
        ? "/v1/wattetheria/agent-identities/runtime/export"
        : provider
          ? "/v1/wattetheria/provider-identity/export"
          : `/v1/wattetheria/agent-identities/service-agents/${encodeURIComponent(identityId)}/export`;
      const backup = await fetchJson(path, {
        method: "POST",
        auth: true,
      });
      downloadIdentityBackup(backup, identityBackupFilename(ownerKind, did));
      agentIdentityStatus(`${identityLabel} DID exported.`);
    }

    async function generateServiceAgentIdentity() {
      agentIdentityStatus("Generating Service Agent DID...");
      await fetchJson("/v1/wattetheria/agent-identities/service-agents/generate", {
        method: "POST",
        auth: true,
        headers: { "content-type": "application/json" },
        body: JSON.stringify({}),
      });
      await loadManagedAgentIdentities();
      agentIdentityStatus("Service Agent DID generated. It is ready to bind during publication.");
    }

    function bindAgentIdentityControls() {
      document.querySelectorAll("[data-agent-identity-kind]").forEach((button) => {
        button.addEventListener("click", () => showAgentIdentityKind(button.dataset.agentIdentityKind));
      });
      document.querySelectorAll("[data-runtime-identity-tab]").forEach((button) => {
        button.addEventListener("click", () => showIdentitySubTab("runtime", button.dataset.runtimeIdentityTab));
      });
      document.querySelectorAll("[data-service-identity-tab]").forEach((button) => {
        button.addEventListener("click", () => showIdentitySubTab("service", button.dataset.serviceIdentityTab));
      });
      document.querySelectorAll("[data-provider-identity-tab]").forEach((button) => {
        button.addEventListener("click", () => showIdentitySubTab("provider", button.dataset.providerIdentityTab));
      });
      qs("runtime-identity-export")?.addEventListener("click", () => {
        exportIdentityBackup("runtime").catch((error) => agentIdentityStatus(error.message, true));
      });
      qs("provider-identity-export")?.addEventListener("click", () => {
        exportIdentityBackup("provider").catch((error) => agentIdentityStatus(error.message, true));
      });
      qs("runtime-identity-import")?.addEventListener("click", () => openAgentIdentityModal("runtime-import"));
      qs("runtime-credential-import")?.addEventListener("click", () => openAgentCredentialModal("runtime"));
      qs("provider-credential-import")?.addEventListener("click", () => openAgentCredentialModal("provider"));
      qs("service-agent-generate")?.addEventListener("click", () => {
        generateServiceAgentIdentity().catch((error) => agentIdentityStatus(error.message, true));
      });
      qs("service-agent-import")?.addEventListener("click", () => openAgentIdentityModal("service-import"));
      qs("agent-identity-modal-close")?.addEventListener("click", closeAgentIdentityModal);
      qs("agent-identity-modal-cancel")?.addEventListener("click", closeAgentIdentityModal);
      qs("agent-identity-modal-back")?.addEventListener("click", showRuntimeIdentityImportInput);
      qs("agent-identity-modal")?.addEventListener("click", (event) => {
        if (event.target === qs("agent-identity-modal")) closeAgentIdentityModal();
      });
      qs("agent-identity-key-form")?.addEventListener("submit", (event) => {
        submitAgentIdentityOperation(event).catch((error) => {
          agentIdentityStatus(error.message, true);
        });
      });
      qs("agent-credential-modal-close")?.addEventListener("click", closeAgentCredentialModal);
      qs("agent-credential-modal-cancel")?.addEventListener("click", closeAgentCredentialModal);
      qs("agent-credential-modal")?.addEventListener("click", (event) => {
        if (event.target === qs("agent-credential-modal")) closeAgentCredentialModal();
      });
      qs("agent-credential-form")?.addEventListener("submit", (event) => {
        submitAgentCredential(event).catch((error) => {
          agentIdentityStatus(error.message, true);
        });
      });
    }

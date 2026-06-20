    function serviceNetStatus(message, isError = false) {
      const target = qs("servicenet-status");
      if (!target) return;
      target.textContent = message;
      target.className = isError ? "status-text error" : "status-text";
    }

    function serviceNetListStatus(message, isError = false) {
      const target = qs("servicenet-list-status");
      if (!target) return;
      target.textContent = message;
      target.className = isError ? "status-text error" : "status-text";
    }

    function showServiceNetList() {
      const listView = qs("servicenet-list-view");
      const detailView = qs("servicenet-detail-view");
      if (listView) listView.hidden = false;
      if (detailView) detailView.hidden = true;
      renderServiceNetList();
      serviceNetStatus("");
    }

    function showServiceNetDetail() {
      const listView = qs("servicenet-list-view");
      const detailView = qs("servicenet-detail-view");
      if (listView) listView.hidden = true;
      if (detailView) detailView.hidden = false;
      detailView?.scrollIntoView({ block: "start" });
    }

    async function loadServiceNetTemplate() {
      if (servicenetTemplate) return servicenetTemplate;
      servicenetTemplate = await fetchJson("/v1/wattetheria/servicenet/agent-card-template", { auth: true });
      return servicenetTemplate;
    }

    function serviceNetScopeOptions(scope) {
      const fields = safeArray(servicenetTemplate?.fields);
      const field = fields.find((item) => item.name === "origin");
      return safeArray(field?.options_by_scope?.[scope]);
    }

    function serviceNetDomainOptions(scope) {
      const fields = safeArray(servicenetTemplate?.fields);
      const field = fields.find((item) => item.name === "domain");
      return safeArray(field?.options_by_scope?.[scope]);
    }

    function setSelectOptions(selectId, options, selected) {
      const target = qs(selectId);
      if (!target) return;
      target.innerHTML = safeArray(options).map((option) => {
        const value = String(option);
        return `<option value="${escapeHtml(value)}"${value === selected ? " selected" : ""}>${escapeHtml(value)}</option>`;
      }).join("");
    }

    function syncServiceNetClassification() {
      const scope = qs("servicenet-scope")?.value || "real_world";
      const origin = qs("servicenet-origin")?.value || (scope === "real_world" ? "custom_built" : "native_published");
      const domain = qs("servicenet-domain")?.value || "GENERAL";
      const origins = serviceNetScopeOptions(scope);
      const domains = serviceNetDomainOptions(scope);
      setSelectOptions("servicenet-origin", origins, origins.includes(origin) ? origin : origins[0]);
      setSelectOptions("servicenet-domain", domains, domains.includes(domain) ? domain : domains[0]);
    }

    function serviceNetSkillCard(skill = {}, index = 0) {
      return `
        <div class="skill-card">
          <div class="skill-card-head">
            <div class="skill-card-title">Skill ${String(index + 1).padStart(2, "0")}</div>
            <button class="secondary servicenet-remove-skill" type="button">Remove</button>
          </div>
          <div class="skill-card-fields">
            <label>
              Skill Name
              <input class="servicenet-skill-name" value="${escapeHtml(skill.name || "")}" placeholder="weather.lookup">
            </label>
            <label>
              Skill Description (optional)
              <textarea class="servicenet-skill-description" rows="6" placeholder="Optional details for callers: when to use this skill, inputs, and expected output.">${escapeHtml(skill.description || "")}</textarea>
            </label>
          </div>
        </div>
      `;
    }

    function renumberServiceNetSkillCards() {
      document.querySelectorAll(".skill-card-title").forEach((title, index) => {
        title.textContent = `Skill ${String(index + 1).padStart(2, "0")}`;
      });
    }

    function removeServiceNetSkillCard(button) {
      button.closest(".skill-card")?.remove();
      if (!document.querySelectorAll(".skill-card").length) {
        renderServiceNetSkills([{ name: "", description: "" }]);
      } else {
        renumberServiceNetSkillCards();
      }
    }

    function renderServiceNetSkills(skills) {
      const rows = safeArray(skills).length ? safeArray(skills) : [{ name: "", description: "" }];
      qs("servicenet-skills").innerHTML = rows.map(serviceNetSkillCard).join("");
      document.querySelectorAll(".servicenet-remove-skill").forEach((button) => {
        button.addEventListener("click", () => removeServiceNetSkillCard(button));
      });
    }

    function paymentAcceptFromAgentCard(card = {}) {
      return safeArray(card.capabilities?.extensions)
        .flatMap((extension) => safeArray(extension?.params?.accepts))
        .find(Boolean) || null;
    }

    async function resetServiceNetForm(card = null, agent = null) {
      await loadServiceNetTemplate();
      const defaults = servicenetTemplate.defaults || {};
      const nextCard = card || defaults;
      qs("servicenet-agent-id").value = agent?.agent_id || "";
      qs("servicenet-provider-id").value = agent?.provider_id || "";
      qs("servicenet-form-title").textContent = agent ? "Update Agent" : "Publish Agent";
      qs("servicenet-form-mode").textContent = agent ? "update" : "new";
      qs("servicenet-submit").textContent = agent ? "Update" : "Publish";
      qs("servicenet-name").value = nextCard.name || "";
      qs("servicenet-description").value = nextCard.description || "";
      qs("servicenet-url").value = nextCard.url || agent?.deployment?.endpoint?.url || "";
      qs("servicenet-scope").value = nextCard.scope || "real_world";
      syncServiceNetClassification();
      qs("servicenet-origin").value = nextCard.origin || qs("servicenet-origin").value;
      qs("servicenet-domain").value = nextCard.domain || qs("servicenet-domain").value;
      qs("servicenet-cost").value = String(nextCard.cost ?? 0);
      qs("servicenet-currency").value = nextCard.currency || "USDC";
      qs("servicenet-supports-task").value = String(nextCard.supportsTask === true);
      qs("servicenet-version").value = agent?.version || "0.1.0";
      qs("servicenet-risk").value = agent?.review?.risk_level || "low";
      renderServiceNetSkills(nextCard.skills);
      const accept = paymentAcceptFromAgentCard(nextCard);
      qs("servicenet-x402-enabled").checked = Boolean(accept);
      qs("servicenet-x402-fields").hidden = !accept;
      qs("servicenet-x402-network").value = accept?.network || "base";
      qs("servicenet-x402-pay-to").value = accept?.payTo || "";
      qs("servicenet-x402-amount").value = accept?.maxAmountRequired || "0";
      serviceNetStatus("");
    }

    function serviceNetSkillsFromForm() {
      return [...document.querySelectorAll(".skill-card")].map((row) => ({
        name: row.querySelector(".servicenet-skill-name")?.value.trim() || "",
        description: row.querySelector(".servicenet-skill-description")?.value.trim() || "",
      })).filter((skill) => skill.name || skill.description);
    }

    function serviceNetAgentCardFromForm() {
      const name = qs("servicenet-name").value.trim();
      const card = {
        name,
        description: qs("servicenet-description").value.trim(),
        url: qs("servicenet-url").value.trim(),
        preferredTransport: "JSONRPC",
        protocolVersion: "1.0",
        scope: qs("servicenet-scope").value,
        origin: qs("servicenet-origin").value,
        domain: qs("servicenet-domain").value,
        cost: Number(qs("servicenet-cost").value || 0),
        currency: qs("servicenet-currency").value,
        supportsTask: qs("servicenet-supports-task").value === "true",
        skills: serviceNetSkillsFromForm(),
        securitySchemes: { none: { type: "none" } },
        security: [{ none: [] }],
      };
      if (qs("servicenet-x402-enabled").checked) {
        card.capabilities = {
          extensions: [{
            uri: "https://github.com/google-a2a/a2a-x402/v0.1",
            required: false,
            description: "Supports x402 payments for ServiceNet invocation.",
            params: {
              accepts: [{
                scheme: "exact",
                network: qs("servicenet-x402-network").value.trim() || "base",
                payTo: qs("servicenet-x402-pay-to").value.trim(),
                maxAmountRequired: qs("servicenet-x402-amount").value.trim() || "0",
                resource: `servicenet:agent:${name || "agent"}`,
                description: "ServiceNet agent invocation",
                maxTimeoutSeconds: 600,
              }],
            },
          }],
        };
      }
      return card;
    }

    function renderServiceNetList() {
      const rows = servicenetAgents;
      renderList("servicenet-list", rows, "No locally published ServiceNet agents.", (row) => {
        const card = row.agent_card || {};
        const skills = skillTags(card.skills);
        return `
          <div class="row">
            <div class="row-head">
              <div class="row-title">${escapeHtml(card.name || row.agent_id)}</div>
              ${pill(row.status || "published", row.status || "ready")}
            </div>
            <div class="row-body">
              ${escapeHtml(card.description || "No description.")}
              <br>Agent ${escapeHtml(compactId(row.agent_id, 30))} | Provider ${escapeHtml(compactId(row.provider_id, 30))}
            </div>
            <div class="row-meta">
              <span>v ${escapeHtml(valueOrDash(row.version))}</span>
              <span>${escapeHtml(valueOrDash(card.domain))}</span>
              <span>${escapeHtml(valueOrDash(card.currency))} ${escapeHtml(valueOrDash(card.cost))}</span>
              ${skills}
            </div>
            <div class="row-actions">
              <button class="secondary" type="button" data-servicenet-update="${escapeHtml(row.agent_id)}">Update</button>
            </div>
          </div>
        `;
      });
      document.querySelectorAll("[data-servicenet-update]").forEach((button) => {
        button.addEventListener("click", () => {
          const agent = servicenetAgents.find((item) => item.agent_id === button.dataset.servicenetUpdate);
          if (agent) {
            resetServiceNetForm(agent.agent_card || {}, agent)
              .then(showServiceNetDetail)
              .catch((error) => serviceNetStatus(error.message, true));
          }
        });
      });
    }

    async function refreshServiceNetAgents() {
      await loadServiceNetTemplate();
      const payload = await fetchJson("/v1/wattetheria/servicenet/published-agents", { auth: true });
      servicenetAgents = safeArray(payload.items);
      renderServiceNetList();
      serviceNetListStatus(`Loaded ${servicenetAgents.length} ServiceNet agents.`);
    }

    async function publishServiceNetAgent(event) {
      event?.preventDefault();
      serviceNetStatus("Submitting ServiceNet publication...");
      const body = {
        agent_id: qs("servicenet-agent-id").value.trim() || null,
        provider_id: qs("servicenet-provider-id").value.trim() || null,
        version: qs("servicenet-version").value.trim() || "0.1.0",
        risk_level: qs("servicenet-risk").value,
        agent_card: serviceNetAgentCardFromForm(),
      };
      const response = await fetchJson("/v1/wattetheria/servicenet/publish", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
        auth: true,
      });
      serviceNetStatus(`Published ${compactId(response.agent_id, 28)}.`);
      await refreshServiceNetAgents();
      showServiceNetList();
    }

    function bindServiceNetControls() {
      qs("servicenet-refresh")?.addEventListener("click", () => {
        refreshServiceNetAgents().catch((error) => serviceNetListStatus(error.message, true));
      });
      qs("servicenet-new")?.addEventListener("click", () => {
        resetServiceNetForm()
          .then(showServiceNetDetail)
          .catch((error) => serviceNetStatus(error.message, true));
      });
      qs("servicenet-back")?.addEventListener("click", showServiceNetList);
      qs("servicenet-cancel")?.addEventListener("click", () => {
        showServiceNetList();
      });
      qs("servicenet-reset")?.addEventListener("click", () => {
        resetServiceNetForm().catch((error) => serviceNetStatus(error.message, true));
      });
      qs("servicenet-scope")?.addEventListener("change", syncServiceNetClassification);
      qs("servicenet-x402-enabled")?.addEventListener("change", (event) => {
        qs("servicenet-x402-fields").hidden = !event.target.checked;
      });
      qs("servicenet-add-skill")?.addEventListener("click", () => {
        const nextIndex = document.querySelectorAll(".skill-card").length;
        qs("servicenet-skills").insertAdjacentHTML("beforeend", serviceNetSkillCard({}, nextIndex));
        qs("servicenet-skills").lastElementChild
          ?.querySelector(".servicenet-remove-skill")
          ?.addEventListener("click", (event) => removeServiceNetSkillCard(event.target));
      });
      qs("servicenet-form")?.addEventListener("submit", (event) => {
        publishServiceNetAgent(event).catch((error) => serviceNetStatus(error.message, true));
      });
    }

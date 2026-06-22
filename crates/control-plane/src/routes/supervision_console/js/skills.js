    let agentSkills = [];

    function skillTagsFromInput() {
      return document.getElementById("skills-tags").value
        .split(",")
        .map((tag) => tag.trim())
        .filter(Boolean);
    }

    function skillModal() {
      return document.getElementById("skills-modal");
    }

    function setSkillFormTitle(title) {
      const target = document.getElementById("skills-form-title");
      if (target) target.textContent = title;
    }

    function showSkillForm() {
      const modal = skillModal();
      if (modal) modal.hidden = false;
    }

    function hideSkillForm() {
      const modal = skillModal();
      if (modal) modal.hidden = true;
    }

    function resetSkillForm({ keepStatus = false } = {}) {
      document.getElementById("skills-id").value = "";
      document.getElementById("skills-name").value = "";
      document.getElementById("skills-description").value = "";
      document.getElementById("skills-tags").value = "";
      document.getElementById("skills-sort-order").value = "100";
      document.getElementById("skills-visible").checked = true;
      if (!keepStatus) {
        document.getElementById("skills-status").textContent = "";
        document.getElementById("skills-status").className = "status-text";
      }
    }

    function openNewSkillForm() {
      resetSkillForm();
      setSkillFormTitle("New Skill");
      showSkillForm();
      document.getElementById("skills-name").focus();
    }

    function editAgentSkill(skillId) {
      const skill = agentSkills.find((item) => item.skill_id === skillId);
      if (!skill) return;
      setSkillFormTitle("Edit Skill");
      showSkillForm();
      document.getElementById("skills-id").value = skill.skill_id || "";
      document.getElementById("skills-name").value = skill.name || "";
      document.getElementById("skills-description").value = skill.description || "";
      document.getElementById("skills-tags").value = safeArray(skill.tags).join(", ");
      document.getElementById("skills-sort-order").value = String(skill.sort_order || 0);
      document.getElementById("skills-visible").checked = skill.visible !== false;
      document.getElementById("skills-name").focus();
    }

    async function toggleAgentSkill(skillId) {
      const skill = agentSkills.find((item) => item.skill_id === skillId);
      if (!skill) return;
      await saveAgentSkill({
        skill_id: skill.skill_id,
        name: skill.name,
        description: skill.description || "",
        tags: safeArray(skill.tags),
        visible: skill.visible === false,
        sort_order: skill.sort_order || 0,
      });
    }

    async function saveAgentSkill(body) {
      const data = await fetchJson("/v1/wattetheria/agent-skills", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
        auth: true,
      });
      document.getElementById("skills-status").textContent = data.ok ? "" : "Error";
      document.getElementById("skills-status").className = data.ok ? "status-text" : "status-text error";
      await loadAgentSkills();
      hideSkillForm();
      return data;
    }

    async function deleteAgentSkill(skillId) {
      const skill = agentSkills.find((item) => item.skill_id === skillId);
      if (!skill) return;
      const name = skill.name || skill.skill_id;
      const confirmed = await confirmDialog({
        title: "Delete skill",
        message: `Delete ${name}? This removes it from the advertised agent card.`,
        confirmText: "Delete",
        cancelText: "Cancel",
        danger: true,
      });
      if (!confirmed) return;
      const data = await fetchJson(`/v1/wattetheria/agent-skills/${encodeURIComponent(skill.skill_id)}`, {
        method: "DELETE",
        auth: true,
      });
      document.getElementById("skills-status").textContent = data.ok ? "" : "Error";
      document.getElementById("skills-status").className = data.ok ? "status-text" : "status-text error";
      await loadAgentSkills();
    }

    async function submitAgentSkill(event) {
      event.preventDefault();
      const skillId = document.getElementById("skills-id").value.trim();
      const sortValue = Number(document.getElementById("skills-sort-order").value || 0);
      try {
        await saveAgentSkill({
          skill_id: skillId || undefined,
          name: document.getElementById("skills-name").value.trim(),
          description: document.getElementById("skills-description").value.trim(),
          tags: skillTagsFromInput(),
          visible: document.getElementById("skills-visible").checked,
          sort_order: Number.isFinite(sortValue) ? sortValue : 0,
        });
        resetSkillForm({ keepStatus: true });
        hideSkillForm();
      } catch (error) {
        document.getElementById("skills-status").textContent = error.message;
        document.getElementById("skills-status").className = "status-text error";
      }
    }

    function skillMonogram(name) {
      const words = String(name || "").trim().split(/[^A-Za-z0-9]+/).filter(Boolean);
      if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
      return ((words[0] || "").slice(0, 2) || "?").toUpperCase();
    }

    function renderAgentSkills() {
      const target = document.getElementById("skills-list");
      if (!target) return;
      const countEl = document.getElementById("skills-count");
      if (countEl) {
        const advertised = agentSkills.filter((skill) => skill.visible !== false).length;
        const hidden = agentSkills.length - advertised;
        countEl.textContent = agentSkills.length ? `${advertised} advertised · ${hidden} hidden` : "";
      }
      const cards = agentSkills.map((skill) => {
        const visible = skill.visible !== false;
        const description = String(skill.description || "").trim();
        const chips = [skill.source || "manual", `sort ${skill.sort_order || 0}`, ...safeArray(skill.tags)];
        const chipHtml = chips
          .map((chip) => String(chip || "").trim())
          .filter(Boolean)
          .map((chip) => `<span class="askill-chip">${escapeHtml(chip)}</span>`)
          .join("");
        return `
          <div class="askill-card">
            <div class="askill-card-head">
              <div class="askill-tile">${escapeHtml(skillMonogram(skill.name || skill.skill_id))}</div>
              <div class="askill-card-meta">
                <div class="askill-card-title-row">
                  <span class="askill-card-name">${escapeHtml(skill.name || skill.skill_id)}</span>
                  ${pill(visible ? "Advertised" : "Hidden", visible ? "ready" : "blocked")}
                </div>
                <div class="askill-card-id">${escapeHtml(skill.skill_id || "")}</div>
              </div>
            </div>
            ${description ? `<div class="askill-card-desc">${escapeHtml(description)}</div>` : ""}
            ${chipHtml ? `<div class="askill-chips">${chipHtml}</div>` : ""}
            <div class="askill-card-foot">
              <div class="askill-card-actions">
                <button class="secondary" type="button" data-skill-edit="${escapeHtml(skill.skill_id || "")}">Edit</button>
                <button class="secondary" type="button" data-skill-toggle="${escapeHtml(skill.skill_id || "")}">${visible ? "Hide" : "Advertise"}</button>
                <button class="secondary danger" type="button" data-skill-delete="${escapeHtml(skill.skill_id || "")}">Delete</button>
              </div>
            </div>
          </div>
        `;
      });
      const newCard = `<button type="button" class="askill-card askill-card-new" data-skill-new>+ New skill</button>`;
      target.innerHTML = cards.join("") + newCard;
      target.querySelectorAll("[data-skill-edit]").forEach((button) => {
        button.addEventListener("click", () => editAgentSkill(button.dataset.skillEdit));
      });
      target.querySelectorAll("[data-skill-toggle]").forEach((button) => {
        button.addEventListener("click", () => {
          toggleAgentSkill(button.dataset.skillToggle).catch((error) => {
            document.getElementById("skills-status").textContent = error.message;
            document.getElementById("skills-status").className = "status-text error";
          });
        });
      });
      target.querySelectorAll("[data-skill-delete]").forEach((button) => {
        button.addEventListener("click", () => {
          deleteAgentSkill(button.dataset.skillDelete).catch((error) => {
            document.getElementById("skills-status").textContent = error.message;
            document.getElementById("skills-status").className = "status-text error";
          });
        });
      });
      target.querySelector("[data-skill-new]")?.addEventListener("click", openNewSkillForm);
    }

    async function loadAgentSkills() {
      const target = document.getElementById("skills-list");
      if (!target) return;
      try {
        const data = await fetchJson("/v1/wattetheria/agent-skills", { auth: true });
        agentSkills = safeArray(data.items);
        renderAgentSkills();
      } catch (error) {
        document.getElementById("skills-status").textContent = error.message;
        document.getElementById("skills-status").className = "status-text error";
      }
    }

    function bindSkillControls() {
      document.getElementById("skills-form")?.addEventListener("submit", submitAgentSkill);
      document.getElementById("skills-reset")?.addEventListener("click", () => {
        resetSkillForm();
        hideSkillForm();
      });
      document.getElementById("skills-modal-close")?.addEventListener("click", () => {
        resetSkillForm();
        hideSkillForm();
      });
      skillModal()?.addEventListener("click", (event) => {
        if (event.target === event.currentTarget) {
          resetSkillForm();
          hideSkillForm();
        }
      });
      document.addEventListener("keydown", (event) => {
        if (event.key === "Escape" && !skillModal()?.hidden) {
          resetSkillForm();
          hideSkillForm();
        }
      });
      document.getElementById("skills-new")?.addEventListener("click", openNewSkillForm);
      document.getElementById("skills-refresh")?.addEventListener("click", () => {
        loadAgentSkills().catch((error) => {
          document.getElementById("skills-status").textContent = error.message;
          document.getElementById("skills-status").className = "status-text error";
        });
      });
    }

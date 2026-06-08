    function identityContext(record) {
      return record?.identity || record || {};
    }

    function identityRecordPublicIdentity(record) {
      const context = identityContext(record);
      return context.public_identity || record?.public_identity || null;
    }

    function identityRecordPublicId(record) {
      const publicIdentity = identityRecordPublicIdentity(record);
      return publicIdentity?.public_id
        || at(record, ["identity", "public_memory_owner", "public_id"])
        || record?.public_id
        || "";
    }

    function identityRecordDisplayName(record) {
      const publicIdentity = identityRecordPublicIdentity(record);
      return publicIdentity?.display_name || identityRecordPublicId(record) || "Unnamed identity";
    }

    function identityDisplayStatus(message, isError = false) {
      const target = qs("identity-display-status");
      if (!target) return;
      target.textContent = message;
      target.className = isError ? "status-text error" : "status-text";
    }

    function syncIdentityDisplayForm() {
      selectedIdentityRecord = identitiesByPublicId.get(publicIdEl.value) || null;
      if (!selectedIdentityRecord) identityDisplayEditing = false;
      const view = qs("identity-display-view");
      const form = qs("identity-display-form");
      const value = qs("identity-display-value");
      const input = qs("identity-display-name");
      const editButton = qs("identity-display-edit");
      const button = qs("identity-display-save");
      const cancelButton = qs("identity-display-cancel");
      if (!view || !form || !value || !input || !editButton || !button || !cancelButton) return;
      const identity = identityRecordPublicIdentity(selectedIdentityRecord) || {};
      const displayName = identity.display_name || "";
      value.textContent = displayName || "-";
      input.value = displayName;
      view.hidden = identityDisplayEditing;
      form.hidden = !identityDisplayEditing;
      editButton.disabled = !selectedIdentityRecord;
      input.disabled = !selectedIdentityRecord || !identityDisplayEditing;
      button.disabled = !selectedIdentityRecord || !identityDisplayEditing;
      cancelButton.disabled = !selectedIdentityRecord || !identityDisplayEditing;
      identityDisplayStatus(selectedIdentityRecord ? "" : "Load an identity before editing the display name.");
    }

    function identityDisplayPayload(displayName) {
      const identity = identityRecordPublicIdentity(selectedIdentityRecord) || {};
      return {
        public_id: identity.public_id || identityRecordPublicId(selectedIdentityRecord),
        display_name: displayName,
      };
    }

    function editIdentityDisplayName() {
      if (!selectedIdentityRecord) {
        identityDisplayStatus("Load an identity before editing.", true);
        return;
      }
      identityDisplayEditing = true;
      syncIdentityDisplayForm();
      qs("identity-display-name")?.focus();
    }

    function cancelIdentityDisplayNameEdit() {
      identityDisplayEditing = false;
      syncIdentityDisplayForm();
    }

    function isAgentIdentityRecord(record) {
      const publicIdentity = identityRecordPublicIdentity(record);
      const publicId = identityRecordPublicId(record);
      return publicIdentity?.active !== false && publicId.startsWith("agent-");
    }

    function identityRecordControllerBinding(record) {
      const context = identityContext(record);
      return context.controller_binding || record?.controller_binding || null;
    }

    function identityRecordProfile(record) {
      const context = identityContext(record);
      return context.profile || record?.profile || null;
    }

    function identityRecordPublicMemoryOwner(record) {
      const context = identityContext(record);
      return context.public_memory_owner || record?.public_memory_owner || null;
    }

    function publicIdFingerprint(publicId) {
      const match = String(publicId || "").match(/\.([0-9a-fA-F]{16})$/);
      return match ? match[1].toLowerCase() : "";
    }

    function didMethod(agentDid) {
      const match = String(agentDid || "").match(/^did:([^:]+):/);
      return match ? `did:${match[1]}` : "";
    }

    function identityProtectionBadges(identity, binding) {
      const publicId = identity.public_id || "";
      const agentDid = identity.agent_did || "";
      const fingerprint = publicIdFingerprint(publicId);
      const selfCertifying = publicId.startsWith("did:key:") || Boolean(fingerprint);
      const method = didMethod(agentDid);
      return [
        {
          label: selfCertifying ? "Self-certifying public_id" : "Plain public_id",
          state: selfCertifying ? "verified" : "needs fingerprint",
          className: selfCertifying ? "ready" : "pending",
        },
        {
          label: agentDid ? "Agent DID bound" : "Agent DID missing",
          state: method || "unbound",
          className: agentDid ? "ready" : "pending",
        },
        {
          label: binding?.active === false ? "Controller inactive" : "Controller active",
          state: valueOrDash(binding?.controller_kind),
          className: binding?.active === false ? "blocked" : "ready",
        },
      ];
    }

    function identityFieldRows(rows) {
      return rows.map(([label, value]) => `
        <div class="identity-field">
          <span>${escapeHtml(label)}</span>
          <strong>${escapeHtml(valueOrDash(value))}</strong>
        </div>
      `).join("");
    }

    function identityCompactList(items, getLabel, emptyLabel) {
      const labels = safeArray(items)
        .map(getLabel)
        .map((value) => String(value || "").trim())
        .filter(Boolean);
      if (!labels.length) return `<span>${escapeHtml(emptyLabel)}</span>`;
      return labels.slice(0, 4).map((label) => `<span>${escapeHtml(label)}</span>`).join("");
    }

    function selectPreferredIdentity() {
      const savedPublicId = publicIdEl.dataset.savedPublicId || "";
      if (savedPublicId && identitiesByPublicId.has(savedPublicId)) {
        publicIdEl.value = savedPublicId;
        return;
      }
      const firstPublicId = publicIdEl.options.length ? publicIdEl.options[0].value : "";
      if (firstPublicId) {
        publicIdEl.value = firstPublicId;
        publicIdEl.dataset.savedPublicId = firstPublicId;
      }
    }


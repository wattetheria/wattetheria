    async function loadIdentities() {
      setStatus("Loading local identities...");
      try {
        const payload = await fetchJson("/v1/civilization/identities", { auth: true });
        const identities = safeArray(payload.public_identities).filter(isAgentIdentityRecord);
        publicIdEl.innerHTML = "";
        identitiesByPublicId.clear();
        for (const record of identities) {
          const publicId = identityRecordPublicId(record);
          if (!publicId) continue;
          identitiesByPublicId.set(publicId, record);
          const option = document.createElement("option");
          option.value = publicId;
          option.textContent = `${identityRecordDisplayName(record)} (${publicId})`;
          publicIdEl.appendChild(option);
        }
        if (!publicIdEl.options.length) {
          const option = document.createElement("option");
          option.value = "";
          option.textContent = "No usable identities";
          publicIdEl.appendChild(option);
          setStatus("Identity records loaded, but no public_id was present.", true);
          syncIdentityDisplayForm();
          renderIdentities([]);
          return;
        }
        selectPreferredIdentity();
        syncIdentityDisplayForm();
        renderIdentities([...identitiesByPublicId.values()]);
        setStatus(`Loaded ${publicIdEl.options.length} local identities.`);
      } catch (error) {
        setStatus(error.message, true);
      }
    }

    async function saveIdentityDisplayName(event) {
      event.preventDefault();
      const input = qs("identity-display-name");
      const displayName = String(input?.value || "").trim();
      if (!selectedIdentityRecord) {
        identityDisplayStatus("Load an identity before saving.", true);
        return;
      }
      if (!displayName) {
        identityDisplayStatus("Display name is required.", true);
        return;
      }
      const payload = identityDisplayPayload(displayName);
      if (!payload.public_id) {
        identityDisplayStatus("Selected identity is missing public_id.", true);
        return;
      }
      identityDisplayStatus("Saving...");
      try {
        await fetchJson("/v1/civilization/public-identity", {
          method: "PATCH",
          auth: true,
          headers: { "content-type": "application/json" },
          body: JSON.stringify(payload),
        });
        await loadIdentities();
        publicIdEl.value = payload.public_id;
        publicIdEl.dataset.savedPublicId = payload.public_id;
        publicIdEl.dispatchEvent(new Event("change"));
        identityDisplayEditing = false;
        syncIdentityDisplayForm();
        if (lastConsolePayload || publicIdEl.value) await refreshConsole();
        identityDisplayStatus("Display name saved.");
      } catch (error) {
        identityDisplayStatus(error.message, true);
      }
    }

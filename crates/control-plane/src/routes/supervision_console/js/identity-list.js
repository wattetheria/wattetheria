    function renderIdentities(rows) {
      renderList("identities-list", rows, "No agent identities loaded.", (row) => {
        const record = row || {};
        const identity = identityRecordPublicIdentity(record) || {};
        const binding = identityRecordControllerBinding(record) || {};
        const owner = identityRecordPublicMemoryOwner(record) || {};
        const profile = identityRecordProfile(record) || {};
        const travel = record.travel_state || {};
        const currentPosition = travel.current_position || {};
        const organizations = safeArray(record.organizations);
        const agentDid = identity.agent_did || owner.agent_did || profile.agent_did;
        const controllerId = binding.controller_node_id || owner.controller_id || owner.controller;
        const protectionBadges = identityProtectionBadges(identity, binding);
        const fingerprint = publicIdFingerprint(identity.public_id);
        return `
          <div class="row identity-row">
            <div class="row-head">
              <div class="row-title">${escapeHtml(identity.display_name || identity.public_id || "Unnamed identity")}</div>
              ${pill(identity.active === false ? "inactive" : "active", identity.active === false ? "blocked" : "ready")}
            </div>
            <div class="row-body identity-public-id">${escapeHtml(identity.public_id || owner.public_id || "-")}</div>
            <div class="subtle">controller ${escapeHtml(compactId(controllerId, 28))} | ${escapeHtml(valueOrDash(profile.role))}</div>
            <div class="identity-protection" aria-label="Identity protection">
              ${protectionBadges.map((badge) => `
                <div class="identity-protection-item ${escapeHtml(badge.className)}">
                  <span>${escapeHtml(badge.label)}</span>
                  <strong>${escapeHtml(badge.state)}</strong>
                </div>
              `).join("")}
            </div>
            <div class="identity-detail-grid">
              <section class="identity-detail-section">
                <div class="identity-detail-title">Public Identity</div>
                ${identityFieldRows([
                  ["agent_did", compactId(agentDid, 36)],
                  ["fingerprint", fingerprint || "-"],
                  ["created_at", formatTime(identity.created_at)],
                  ["updated_at", formatTime(identity.updated_at)],
                ])}
              </section>
              <section class="identity-detail-section">
                <div class="identity-detail-title">Controller Binding</div>
                ${identityFieldRows([
                  ["controller_kind", binding.controller_kind],
                  ["controller_ref", binding.controller_ref],
                  ["controller_node_id", compactId(controllerId, 36)],
                  ["ownership_scope", binding.ownership_scope],
                ])}
              </section>
              <section class="identity-detail-section">
                <div class="identity-detail-title">Profile</div>
                ${identityFieldRows([
                  ["faction", profile.faction],
                  ["role", profile.role],
                  ["strategy", profile.strategy],
                  ["home_subnet_id", profile.home_subnet_id],
                  ["home_zone_id", profile.home_zone_id],
                ])}
              </section>
              <section class="identity-detail-section">
                <div class="identity-detail-title">Travel</div>
                ${identityFieldRows([
                  ["system_id", currentPosition.system_id || travel.system_id],
                  ["zone_id", currentPosition.zone_id || travel.zone_id],
                  ["status", travel.status || travel.travel_status],
                  ["updated_at", formatTime(travel.updated_at || travel.last_updated_at)],
                ])}
              </section>
            </div>
            <div class="row-meta identity-orgs" aria-label="Identity organizations">
              ${identityCompactList(organizations, (org) => org.name || org.organization_name || org.id || org.organization_id, "No guilds")}
            </div>
          </div>
        `;
      });
    }

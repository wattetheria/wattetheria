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
        const publicId = identity.public_id || owner.public_id || "";
        const agentDid = identity.agent_did || owner.agent_did || profile.agent_did || "";
        const agentAlias = identityAlias(publicId);
        const agentAddress = identityAddress(agentDid);
        const controllerId = binding.controller_node_id || owner.controller_id || owner.controller;
        const protectionBadges = identityProtectionBadges(identity, binding);
        const fingerprint = publicIdFingerprint(publicId);
        const displayName = identity.display_name || publicId || "Unnamed identity";
        const active = identity.active !== false;
        const monogram = identityMonogram(displayName);
        return `
          <div class="identity-card">
            <div class="identity-cred-head">
              <div class="identity-cred-top">
                <span class="identity-cred-network">${escapeHtml(IDENTITY_NETWORK_ID)}</span>
                <div class="identity-cred-actions">
                  <button type="button" class="identity-cred-btn" onclick="copyIdentityId('${escapeHtml(agentAlias)}', this)"${agentAlias ? "" : " disabled"}>Copy ID</button>
                  <button type="button" class="identity-cred-btn" onclick="editIdentityFromCard('${escapeHtml(publicId)}')"${publicId ? "" : " disabled"}>Edit</button>
                </div>
              </div>
              <div class="identity-cred-id">
                <div class="identity-cred-avatar">${escapeHtml(monogram)}</div>
                <div class="identity-cred-meta">
                  <div class="identity-cred-name-row">
                    <span class="identity-cred-name">${escapeHtml(displayName)}</span>
                    <span class="identity-cred-status ${active ? "is-active" : "is-inactive"}">
                      <span class="identity-cred-dot"></span>${active ? "active" : "inactive"}
                    </span>
                  </div>
                  <div class="identity-cred-handle">${escapeHtml(agentAlias || publicId || "-")}</div>
                </div>
              </div>
              <div class="identity-trust" aria-label="Identity protection">
              ${protectionBadges.map((badge) => `
                <div class="identity-trust-item ${escapeHtml(badge.className)}">
                  <span class="identity-trust-mark" aria-hidden="true">${badge.className === "ready" ? "&#10003;" : "!"}</span>
                  <span class="identity-trust-text">
                    <strong>${escapeHtml(badge.label)}</strong>
                    <em>${escapeHtml(badge.state)}</em>
                  </span>
                </div>
              `).join("")}
              </div>
            </div>
            <div class="identity-card-body">
              <div class="identity-spec-grid">
                ${identitySpecSection("Public Identity", identitySpecRows([
                  ["agent_did", compactId(agentDid, 28), true],
                  ["fingerprint", fingerprint || "-", true],
                  ["created_at", formatTime(identity.created_at)],
                  ["updated_at", formatTime(identity.updated_at)],
                ]))}
                ${identitySpecSection("Addresses", identitySpecRows([
                  ["@public_id", agentAlias || "-", true],
                  ["identity_uri", agentAddress || "-", true],
                ]))}
                ${identitySpecSection("DID Service", identitySpecRows([
                  ["type", agentDid ? "WattetheriaNodeEndpoint" : "-"],
                  ["network", agentDid ? IDENTITY_NETWORK_ID : "-"],
                  ["transport", agentDid ? "wattswarm" : "-"],
                  ["agentDid", compactId(agentDid, 28), true],
                ]))}
                ${identitySpecSection("Controller Binding", identitySpecRows([
                  ["controller_kind", binding.controller_kind],
                  ["controller_ref", binding.controller_ref],
                  ["controller_node_id", compactId(controllerId, 28), true],
                  ["ownership_scope", binding.ownership_scope],
                ]))}
                ${identitySpecSection("Profile", identitySpecRows([
                  ["faction", profile.faction],
                  ["role", profile.role],
                  ["strategy", profile.strategy],
                  ["home_subnet_id", profile.home_subnet_id],
                  ["home_zone_id", profile.home_zone_id],
                ]))}
                ${identitySpecSection("Travel", identitySpecRows([
                  ["system_id", currentPosition.system_id || travel.system_id],
                  ["zone_id", currentPosition.zone_id || travel.zone_id],
                  ["status", travel.status || travel.travel_status],
                  ["updated_at", formatTime(travel.updated_at || travel.last_updated_at)],
                ]))}
              </div>
              <div class="identity-guilds" aria-label="Identity organizations">
                ${identityCompactList(organizations, (org) => org.name || org.organization_name || org.id || org.organization_id, "No guilds")}
              </div>
            </div>
          </div>
        `;
      });
    }

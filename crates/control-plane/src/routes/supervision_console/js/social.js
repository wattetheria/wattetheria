    function renderFriends(payload) {
      if (!qs("friends-list")) return;
      const rows = safeArray(payload.friend_relationships)
        .filter((row) => !row.pending_inbound && !row.pending_outbound && row.relationship_state !== "blocked");
      renderList("friends-list", rows, "No friends recorded.", (row) => `
        <div class="row">
          <div class="row-head">
            <div class="row-title">${escapeHtml(row.counterpart_display_name || row.counterpart_agent_name || row.counterpart_agent_did || row.counterpart_public_id || row.remote_node_id)}</div>
            ${pill(row.relationship_state || row.relationship_kind || "friend", row.status || row.relationship_state || row.relationship_kind)}
          </div>
          <div class="row-body">
            Public ${escapeHtml(compactId(row.counterpart_agent_public_id || row.counterpart_public_id || row.public_id, 30))}
            | Agent ${escapeHtml(compactId(row.counterpart_agent_did, 30))}
            | Node ${escapeHtml(compactId(row.remote_node_id, 30))}
          </div>
          <div class="row-meta">
            <span>${escapeHtml(valueOrDash(row.status))}</span>
            <span>${escapeHtml(valueOrDash(row.network_id))}</span>
            ${skillPreview(row.counterpart_skills) ? `<span>Skills ${escapeHtml(skillPreview(row.counterpart_skills))}</span>` : ""}
          </div>
        </div>
      `);
    }

    function matchingFriendForConversation(payload, conversation) {
      return safeArray(payload.friend_relationships).find((row) => {
        const publicIds = [
          row.counterpart_agent_public_id,
          row.counterpart_public_id,
          row.public_id,
        ].filter(Boolean);
        const remoteNodes = [row.remote_node_id, row.counterpart_node_id].filter(Boolean);
        return publicIds.includes(conversation.counterpartPublicId)
          || remoteNodes.includes(conversation.remoteNodeId);
      }) || {};
    }

    function isNodeId(value) {
      return /^[a-f0-9]{64}$/i.test(String(value || ""));
    }

    function dmMessageEnvelope(row) {
      return row?.agent_envelope?.message || {};
    }

    function dmMessageIdentifiers(row) {
      const message = dmMessageEnvelope(row);
      return [
        row.counterpart_public_id,
        row.remote_node_id,
        row.agent_envelope?.source_agent_id,
        row.agent_envelope?.target_agent_id,
        row.agent_envelope?.source_node_id,
        row.agent_envelope?.target_node_id,
        message.source_public_id,
        message.target_public_id,
      ].filter(Boolean);
    }

    function friendIdentifiers(row) {
      return [
        row.counterpart_agent_public_id,
        row.counterpart_public_id,
        row.public_id,
        row.counterpart_agent_did,
        row.remote_node_id,
        row.counterpart_node_id,
      ].filter(Boolean);
    }

    function matchingFriendForMessage(payload, message) {
      const identifiers = new Set(dmMessageIdentifiers(message));
      return safeArray(payload.friend_relationships).find((row) =>
        friendIdentifiers(row).some((identifier) => identifiers.has(identifier))
      ) || {};
    }

    function renderFriendRequests(payload) {
      const rows = safeArray(payload.pending_friend_requests);
      renderList("friend-requests-list", rows, "No pending friend requests.", (row) => `
        <div class="row">
          <div class="row-head">
            <div class="row-title">${escapeHtml(row.from || row.counterpart_display_name || row.counterpart_public_id || row.request_id)}</div>
            ${pill("inbound", "pending")}
          </div>
          <div class="row-body">${escapeHtml(row.preview || compactId(row.request_id, 32))}</div>
        </div>
      `);
    }

    function dmConversationLabel(row) {
      return row.counterpart_display_name
        || row.counterpart_public_id
        || row.remote_node_id
        || row.thread_id
        || "Unknown counterpart";
    }

    function dmDisplayName(value) {
      const text = valueOrDash(value).replace(/^wattetheria\s+/i, "").trim();
      return text || valueOrDash(value);
    }

    function dmConversationLabelForFriend(friend, row) {
      return dmDisplayName(friend.counterpart_display_name
        || friend.display_name
        || friend.counterpart_agent_name
        || friend.counterpart_agent_did
        || friend.counterpart_agent_public_id
        || friend.counterpart_public_id
        || friend.public_id
        || dmConversationLabel(row));
    }

    function dmCounterpartPublicId(friend, row) {
      const message = dmMessageEnvelope(row);
      const localPublicId = publicIdEl.value;
      return friend.counterpart_agent_public_id
        || friend.counterpart_public_id
        || friend.public_id
        || [row.counterpart_public_id, message.source_public_id, message.target_public_id]
          .find((value) => value && value !== localPublicId && !isNodeId(value))
        || row.counterpart_public_id
        || row.remote_node_id;
    }

    function dmRemoteNodeId(friend, row) {
      const identifiers = dmMessageIdentifiers(row);
      return friend.remote_node_id
        || friend.counterpart_node_id
        || identifiers.find(isNodeId)
        || "";
    }

    function dmConversationKey(friend, row) {
      return dmCounterpartPublicId(friend, row)
        || dmRemoteNodeId(friend, row)
        || row.thread_id
        || row.message_id
        || "unknown";
    }

    function isAcceptedDmFriend(row) {
      const state = String(row.relationship_state || row.relationship_kind || row.status || "").toLowerCase();
      return !row.pending_inbound
        && !row.pending_outbound
        && row.active !== false
        && state !== "blocked"
        && (state === "friend" || state === "active" || state === "accepted" || row.active === true);
    }

    function dmAcceptedFriends(payload) {
      const friends = [
        ...safeArray(payload.friend_relationships),
        ...safeArray(payload.local_client_friends),
      ];
      const byKey = new Map();
      for (const friend of friends.filter(isAcceptedDmFriend)) {
        const key = dmConversationKey(friend, {});
        if (key && key !== "unknown" && !byKey.has(key)) {
          byKey.set(key, friend);
        }
      }
      return [...byKey.values()];
    }

    function emptyDmConversationForFriend(friend) {
      const key = dmConversationKey(friend, {});
      return {
        key,
        label: dmConversationLabelForFriend(friend, {}),
        remoteNodeId: dmRemoteNodeId(friend, {}),
        counterpartPublicId: dmCounterpartPublicId(friend, {}),
        threadId: friend.dm_thread_id || friend.thread_id || "",
        friend,
        messageMap: new Map(),
        messages: [],
        latestAt: timestampValue(friend.updated_at || friend.responded_at || friend.created_at),
      };
    }

    function isSyntheticDmMessage(row) {
      const kind = String(row.message_kind || "").toLowerCase();
      const content = row.content || dmMessageEnvelope(row).content || {};
      return kind === "session_init"
        || kind === "relationship_established"
        || content.synthetic === true;
    }

    function dmLogicalMessageKey(row) {
      return dmMessageEnvelope(row).message_id || row.transport_message_id || row.message_id || "";
    }

    function dmDeliveryRank(row) {
      const state = String(row.delivery_state || "").toLowerCase();
      if (state === "acknowledged") return 3;
      if (state === "delivered") return 2;
      if (state === "sent") return 1;
      return 0;
    }

    function preferredDmMessage(current, candidate) {
      if (!current) return candidate;
      const currentRank = dmDeliveryRank(current);
      const candidateRank = dmDeliveryRank(candidate);
      if (candidateRank !== currentRank) {
        return candidateRank > currentRank ? candidate : current;
      }
      return timestampValue(candidate.updated_at || candidate.created_at)
        >= timestampValue(current.updated_at || current.created_at)
        ? candidate
        : current;
    }

    function timestampValue(value) {
      if (value == null || value === "") return 0;
      if (typeof value === "number") return value > 100000000000 ? value : value * 1000;
      const parsed = Date.parse(value);
      return Number.isNaN(parsed) ? 0 : parsed;
    }

    function groupDmConversations(payload) {
      const groups = new Map();
      for (const friend of dmAcceptedFriends(payload)) {
        const conversation = emptyDmConversationForFriend(friend);
        if (conversation.key && !groups.has(conversation.key)) {
          groups.set(conversation.key, conversation);
        }
      }
      for (const row of safeArray(payload.dm_messages).filter((message) => !isSyntheticDmMessage(message))) {
        const friend = matchingFriendForMessage(payload, row);
        const key = dmConversationKey(friend, row);
        if (!groups.has(key)) {
          groups.set(key, {
            key,
            label: dmConversationLabelForFriend(friend, row),
            remoteNodeId: dmRemoteNodeId(friend, row),
            counterpartPublicId: dmCounterpartPublicId(friend, row),
            threadId: row.thread_id,
            friend,
            messageMap: new Map(),
            messages: [],
          });
        }
        const group = groups.get(key);
        group.friend = Object.keys(group.friend || {}).length ? group.friend : friend;
        group.remoteNodeId = group.remoteNodeId || dmRemoteNodeId(friend, row);
        group.counterpartPublicId = group.counterpartPublicId || dmCounterpartPublicId(friend, row);
        group.threadId = group.threadId || row.thread_id;
        const messageKey = dmLogicalMessageKey(row) || row.message_id || `${row.thread_id}:${row.created_at}`;
        group.messageMap.set(messageKey, preferredDmMessage(group.messageMap.get(messageKey), row));
      }
      return [...groups.values()].map((group) => {
        group.messages = [...group.messageMap.values()];
        group.messages.sort((a, b) => timestampValue(a.created_at) - timestampValue(b.created_at));
        group.latestMessage = group.messages[group.messages.length - 1];
        group.latestAt = timestampValue(group.latestMessage?.created_at) || group.latestAt || 0;
        group.threadId = group.latestMessage?.thread_id || group.threadId;
        delete group.messageMap;
        return group;
      }).sort((a, b) => b.latestAt - a.latestAt);
    }

    function dmDirectionClass(direction) {
      const value = String(direction || "").toLowerCase();
      return value === "inbound" || value === "outbound" ? value : "unknown";
    }

    function dmDetailField(label, value) {
      return `
        <div class="dm-detail-field">
          <span>${escapeHtml(label)}</span>
          <strong>${escapeHtml(valueOrDash(value))}</strong>
        </div>
      `;
    }

    function dmAgentInitials(name) {
      const text = valueOrDash(name).replace(/^agent[-_\s]*/i, "").trim();
      const parts = text.split(/[-_\s.]+/).filter(Boolean);
      if (!parts.length) return "DM";
      if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
      return `${parts[0][0]}${parts[1][0]}`.toUpperCase();
    }

    function dmSkillLabels(skills) {
      return safeArray(skills)
        .map((skill) => {
          if (typeof skill === "string") return skill;
          return skill?.name || skill?.id || "";
        })
        .map((skill) => String(skill).trim())
        .filter(Boolean);
    }

    function renderDmThread(conversation) {
      const detail = conversation.friend || {};
      const displayName = dmDisplayName(detail.counterpart_display_name
        || detail.counterpart_agent_name
        || detail.counterpart_agent_did
        || detail.counterpart_public_id
        || detail.remote_node_id
        || conversation.label);
      const publicId = conversation.counterpartPublicId || detail.counterpart_agent_public_id || detail.counterpart_public_id;
      const agentDid = detail.counterpart_agent_did || detail.counterpart_agent_name || "-";
      const remoteNodeId = conversation.remoteNodeId || detail.remote_node_id;
      const status = detail.status || detail.relationship_state || detail.relationship_kind || "friend";
      const network = detail.network_id || "-";
      const relationshipLabel = detail.relationship_state || detail.relationship_kind || "friend";
      const relationshipClass = detail.status || detail.relationship_state || detail.relationship_kind;
      const skills = dmSkillLabels(detail.counterpart_skills);
      const description = String(detail.counterpart_description || "").trim();
      const detailCard = `
        <div class="dm-detail-card">
          <div class="dm-detail-hero">
            <div class="dm-detail-avatar">${escapeHtml(dmAgentInitials(displayName))}</div>
            <div class="dm-detail-title-block">
              <div class="dm-detail-title-row">
                <h3>${escapeHtml(displayName)}</h3>
                ${pill(relationshipLabel, relationshipClass)}
              </div>
              <p>${escapeHtml(compactId(publicId, 42))}</p>
              <div class="dm-detail-meta">
                <span>${escapeHtml(valueOrDash(status))}</span>
                <span>${escapeHtml(valueOrDash(network))}</span>
              </div>
              ${description ? `<p class="dm-detail-description">${escapeHtml(description)}</p>` : ""}
            </div>
            <button type="button" class="secondary dm-detail-close" data-dm-detail-close>Close</button>
          </div>
          <div class="dm-detail-grid">
            <section class="dm-detail-section">
              <h4>Public Identity</h4>
              ${dmDetailField("Public", compactId(publicId, 52))}
              ${dmDetailField("Agent", compactId(agentDid, 52))}
              ${dmDetailField("Node", compactId(remoteNodeId, 52))}
            </section>
            <section class="dm-detail-section">
              <h4>Network</h4>
              ${dmDetailField("Status", status)}
              ${dmDetailField("Network", network)}
              ${dmDetailField("Relationship", relationshipLabel)}
            </section>
          </div>
          <section class="dm-detail-section dm-detail-skills">
            <h4>Skills</h4>
            <div class="dm-detail-skill-list">
              ${skills.length
                ? skills.map((skill) => `<span>${escapeHtml(skill)}</span>`).join("")
                : "<span>-</span>"}
            </div>
          </section>
        </div>
      `;
      return `
        <div class="dm-thread-head-shell">
          <button type="button" class="dm-thread-head" data-dm-detail-toggle>
            <span>
              <span class="row-title">${escapeHtml(displayName)}</span>
              <span class="row-body">
                Public ${escapeHtml(compactId(publicId, 38))}
                | Node ${escapeHtml(compactId(remoteNodeId, 34))}
              </span>
            </span>
            ${pill(status, status)}
          </button>
          ${dmDetailOpen ? `<div class="dm-detail-modal" data-dm-detail-modal>${detailCard}</div>` : ""}
        </div>
        <div class="dm-thread-context">[ thread context: ${escapeHtml(compactId(conversation.threadId, 34))} ]</div>
        <div class="dm-bubble-list">
          ${conversation.messages.length ? conversation.messages.map((message) => {
            const direction = dmDirectionClass(message.direction);
            const actor = direction === "outbound" ? "You / Operator" : compactId(conversation.label, 24);
            return `
              <div class="dm-bubble-row ${direction}">
                <div class="dm-bubble-meta">
                  <span>${escapeHtml(String(message.direction || message.delivery_state || "message").toUpperCase())}</span>
                  <span>${escapeHtml(actor)}</span>
                  <span>${escapeHtml(formatTime(message.created_at))}</span>
                </div>
                <div class="dm-bubble">${escapeHtml(textFromContent(message.content) || message.encrypted_body || "No message preview")}</div>
              </div>
            `;
          }).join("") : empty("No direct messages yet.")}
        </div>
      `;
    }

    function scrollDmThreadToLatest(container) {
      const bubbleList = container.querySelector(".dm-bubble-list");
      if (!bubbleList) return;
      requestAnimationFrame(() => {
        bubbleList.scrollTop = bubbleList.scrollHeight;
      });
    }

    function renderDmMessages(payload) {
      const target = qs("dm-list");
      const conversations = groupDmConversations(payload);
      if (!conversations.length) {
        target.innerHTML = empty("No direct messages recorded.");
        activeDmThreadKey = "";
        return;
      }
      if (!conversations.some((conversation) => conversation.key === activeDmThreadKey)) {
        activeDmThreadKey = conversations[0].key;
      }
      const activeConversation = conversations.find((conversation) => conversation.key === activeDmThreadKey) || conversations[0];
      conversations.forEach((conversation) => {
        conversation.friend = Object.keys(conversation.friend || {}).length
          ? conversation.friend
          : matchingFriendForConversation(payload, conversation);
      });
      target.innerHTML = `
        <div class="dm-workspace">
          <div class="dm-session-list">
            ${conversations.map((conversation) => {
              const active = conversation.key === activeConversation.key;
              return `
                <button type="button" class="dm-session-card ${active ? "active" : ""}" data-dm-thread="${escapeHtml(conversation.key)}">
                  <span class="dm-session-title">
                    <span>${escapeHtml(compactId(conversation.label, 32))}</span>
                    ${pill(`${conversation.messages.length} msg`, active ? "friend" : "ready")}
                  </span>
                  <span class="dm-session-subtitle">Public: ${escapeHtml(compactId(conversation.counterpartPublicId || conversation.remoteNodeId, 30))}</span>
                </button>
              `;
            }).join("")}
          </div>
          <div class="dm-thread-view">${renderDmThread(activeConversation)}</div>
        </div>
      `;
      scrollDmThreadToLatest(target);
      target.querySelectorAll("[data-dm-thread]").forEach((button) => {
        button.addEventListener("click", () => {
          activeDmThreadKey = button.dataset.dmThread || "";
          dmDetailOpen = false;
          renderDmMessages(payload);
        });
      });
      target.querySelector("[data-dm-detail-toggle]")?.addEventListener("click", () => {
        dmDetailOpen = true;
        renderDmMessages(payload);
      });
      target.querySelector("[data-dm-detail-close]")?.addEventListener("click", () => {
        dmDetailOpen = false;
        renderDmMessages(payload);
      });
      target.querySelector("[data-dm-detail-modal]")?.addEventListener("click", (event) => {
        if (event.target === event.currentTarget) {
          dmDetailOpen = false;
          renderDmMessages(payload);
        }
      });
    }

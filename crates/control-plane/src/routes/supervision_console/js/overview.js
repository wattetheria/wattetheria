    function nearbyLastSeenLabel(ageMs) {
      const value = Number(ageMs);
      if (!Number.isFinite(value) || value < 0) return "";
      if (value < 1000) return "last seen now";
      const seconds = Math.round(value / 1000);
      if (seconds < 60) return `last seen ${seconds}s ago`;
      const minutes = Math.round(seconds / 60);
      if (minutes < 60) return `last seen ${minutes}m ago`;
      const hours = Math.round(minutes / 60);
      return `last seen ${hours}h ago`;
    }

    function nearbyStatus(row) {
      const relationshipStatus = String(row.relationship_state || row.relationship_kind || "").toLowerCase();
      const status = String(row.status || relationshipStatus || "").toLowerCase();
      if (status === "blocked" || relationshipStatus === "blocked") return "blocked";
      if (row.pending_inbound || row.pending_outbound || status === "requested" || status === "pending") return "pending";
      if (row.connected === true) return "online";
      if (row.stale === true || status === "stale") return "stale";
      if (status === "online" || status === "friend") return "discovered";
      if (status === "discovered") return "discovered";
      return "offline";
    }

    function nearbyRank(row) {
      const status = nearbyStatus(row);
      if (status === "blocked") return 90;
      if (row.kind === "friend" && status === "online") return 10;
      if (row.last_message_at) return 20;
      if (status === "pending") return 30;
      if (row.kind === "node" && status === "online") return 40;
      if (row.kind === "node" && status === "stale") return 50;
      if (row.kind === "node") return 55;
      if (row.kind === "friend") return 60;
      return 70;
    }

    function nodeRelationshipState(node) {
      return node.relationship_state
        || at(node, ["relationship", "relationship_state"])
        || at(node, ["relationship", "last_action"]);
    }

    function nearbySourceAgentCard(node) {
      return node.source_agent_card
        || at(node, ["relationship", "agent_envelope", "source_agent_card"])
        || at(node, ["metadata", "contact_material", "source_agent_card"])
        || at(node, ["discovery", "source_agent_card"])
        || {};
    }

    function nearbyAgentCard(node) {
      const sourceAgentCard = nearbySourceAgentCard(node);
      return node.agent_card || sourceAgentCard.card || {};
    }

    function nearbyAgentDisplayName(node, fallback) {
      const card = nearbyAgentCard(node);
      const metadata = card.metadata || {};
      return card.name
        || metadata.display_name
        || node.display_name
        || node.name
        || fallback;
    }

    function nearbySkillLabels(card) {
      return safeArray(card.skills)
        .map((skill) => skill.name || skill.id || skill.description)
        .filter((value) => value != null && value !== "");
    }

    function buildNearbyRows(payload) {
      const rows = [];
      const seen = new Set();
      for (const node of safeArray(payload.nodes).concat(safeArray(payload.peers))) {
        const nodeId = node.node_id || node.id;
        if (!nodeId) continue;
        const key = `node:${nodeId}`;
        if (seen.has(key)) continue;
        seen.add(key);
        const relationshipState = nodeRelationshipState(node);
        const connected = node.connected === true;
        const stale = node.stale === true;
        const recentlySeen = node.recently_seen === true;
        const lastSeenLabel = nearbyLastSeenLabel(node.last_seen_age_ms);
        const sourceKind = node.source_kind || at(node, ["discovery", "source_kind"]);
        const endpoint = node.endpoint || at(node, ["metadata", "endpoint_id"]) || at(node, ["discovery", "endpoint_id"]);
        const sourceAgentCard = nearbySourceAgentCard(node);
        const agentCard = nearbyAgentCard(node);
        const displayName = nearbyAgentDisplayName(node, nodeId);
        const connectionLabel = connected ? "online" : (stale ? "stale" : "not connected");
        const sourceLabel = sourceKind ? `last source: ${sourceKind}` : "";
        const metaLines = [connectionLabel];
        if (lastSeenLabel) metaLines.push(lastSeenLabel);
        if (sourceLabel) metaLines.push(sourceLabel);
        rows.push({
          key,
          node_id: nodeId,
          raw: node,
          kind: "node",
          name: displayName,
          agent_card: agentCard,
          source_agent_card: sourceAgentCard,
          status: node.status || relationshipState || (connected ? "online" : (stale ? "stale" : "discovered")),
          connected,
          recently_seen: recentlySeen,
          stale,
          last_seen_age_ms: node.last_seen_age_ms,
          relationship_state: relationshipState,
          source_kind: sourceKind,
          detail: connectionLabel,
          meta_lines: metaLines,
          endpoint_detail: endpoint ? `endpoint ${compactId(endpoint, 24)}` : compactId(nodeId, 24),
          updated_at: node.updated_at || at(node, ["discovery", "updated_at"]) || at(node, ["metadata", "last_identified_at"]),
        });
      }

      return rows.sort((left, right) => {
        const rankDelta = nearbyRank(left) - nearbyRank(right);
        if (rankDelta !== 0) return rankDelta;
        return valueOrZero(right.last_message_at || right.updated_at) - valueOrZero(left.last_message_at || left.updated_at);
      });
    }

    function nearbyRowsHtml(rows) {
      return rows.map((row) => {
        const status = nearbyStatus(row);
        const label = row.kind === "request"
          ? (row.pending_inbound ? "inbound" : "request")
          : row.kind;
        const metaLines = safeArray(row.meta_lines).length
          ? safeArray(row.meta_lines)
          : [row.detail || row.source_kind || status];
        return `
          <div class="nearby-item">
            <div class="nearby-line">
              <span class="nearby-dot ${escapeHtml(status)}"></span>
              <span class="nearby-name">${escapeHtml(compactId(row.name, 20))}</span>
              <span class="nearby-kind">${escapeHtml(label)}</span>
            </div>
            <div class="nearby-meta">${metaLines.map((line) => `<div>${escapeHtml(line)}</div>`).join("")}</div>
          </div>
        `;
      }).join("");
    }

    function renderNearbyList(countId, listId, rows, emptyText) {
      qs(countId).textContent = `Top ${rows.length}`;
      qs(listId).innerHTML = rows.length ? nearbyRowsHtml(rows) : empty(emptyText);
    }

    function nearbyAgeShort(ageMs) {
      const value = Number(ageMs);
      if (!Number.isFinite(value) || value < 0) return "-";
      if (value < 1000) return "now";
      const seconds = Math.round(value / 1000);
      if (seconds < 60) return `${seconds}s`;
      const minutes = Math.round(seconds / 60);
      if (minutes < 60) return `${minutes}m`;
      const hours = Math.round(minutes / 60);
      if (hours < 24) return `${hours}h`;
      return `${Math.round(hours / 24)}d`;
    }

    function nearbyStatusGroup(row) {
      const status = nearbyStatus(row);
      if (status === "online") return "online";
      if (status === "pending") return "pending";
      if (status === "stale") return "stale";
      if (status === "blocked") return "blocked";
      return "offline";
    }

    function nearbyStatusCounts(rows) {
      const counts = { all: rows.length, online: 0, pending: 0, stale: 0, offline: 0, blocked: 0 };
      for (const row of rows) counts[nearbyStatusGroup(row)] += 1;
      return counts;
    }

    function nearbyFilteredRows(rows) {
      const query = nearbySearchQuery.trim().toLowerCase();
      return rows.filter((row) => {
        if (nearbyStatusFilter !== "all" && nearbyStatusGroup(row) !== nearbyStatusFilter) return false;
        if (!query) return true;
        return `${row.node_id || ""} ${row.name || ""}`.toLowerCase().includes(query);
      });
    }

    function nearbyTableRowsHtml(rows) {
      return rows.map((row) => {
        const status = nearbyStatus(row);
        const fullId = row.node_id || row.name || "";
        const displayName = row.name || fullId;
        return `
          <tr class="nearby-row" data-nearby-node="${escapeHtml(fullId)}" tabindex="0" role="button" title="View agent card">
            <td>${pill(status, status)}</td>
            <td class="nearby-cell-id" title="${escapeHtml(fullId)}">${escapeHtml(compactId(displayName, 22))}</td>
            <td class="nearby-cell-kind">${escapeHtml(row.kind || "node")}</td>
            <td class="nearby-cell-age">${escapeHtml(nearbyAgeShort(row.last_seen_age_ms))}</td>
            <td class="nearby-cell-source">${escapeHtml(valueOrDash(row.source_kind))}</td>
          </tr>
        `;
      }).join("");
    }

    function nearbyDetailCardHtml(row) {
      const node = row.raw || {};
      const card = row.agent_card || nearbyAgentCard(node);
      const sourceAgentCard = row.source_agent_card || nearbySourceAgentCard(node);
      const metadata = card.metadata || {};
      const displayName = row.name || nearbyAgentDisplayName(node, row.node_id || "agent");
      const status = nearbyStatus(row);
      const fullId = row.node_id || "";
      const endpoint = node.endpoint || at(node, ["metadata", "endpoint_id"]) || at(node, ["discovery", "endpoint_id"]);
      const agentId = sourceAgentCard.agent_id || metadata.agent_id || node.agent_id;
      const publicAddress = agentPublicAddress(
        sourceAgentCard.public_id,
        at(sourceAgentCard, ["card", "metadata", "public_id"]),
        metadata.public_id,
        node.public_id,
        at(node, ["relationship", "counterpart_agent_public_id"]),
        at(node, ["relationship", "counterpart_public_id"]),
        at(node, ["metadata", "public_id"]),
        at(node, ["discovery", "public_id"])
      );
      const publicAddressLabel = agentPublicAddressLabel(publicAddress);
      const lastSeen = nearbyLastSeenLabel(row.last_seen_age_ms) || "-";
      const updatedAt = row.updated_at ? formatTime(row.updated_at) : "-";
      const connection = row.connected ? "connected" : (row.stale ? "stale" : "not connected");
      const skills = nearbySkillLabels(card);
      return renderAgentDetailCard({
        avatarSeed: displayName,
        title: compactId(displayName, 28),
        statusLabel: status,
        statusClass: status,
        subtitle: compactId(fullId, 48),
        meta: [connection, lastSeen, row.source_kind],
        description: card.description,
        sections: [
          { title: "Identity", fields: [
            { label: "Public", value: compactId(publicAddressLabel, 52) },
            { label: "Agent", value: compactId(agentId, 52) },
            { label: "Node", value: compactId(fullId, 52) },
            { label: "Endpoint", value: compactId(endpoint, 52) },
          ] },
          { title: "Network", fields: [
            { label: "Connection", value: connection },
            { label: "Last Seen", value: lastSeen },
            { label: "Source", value: row.source_kind },
            { label: "Relationship", value: row.relationship_state },
            { label: "Updated", value: updatedAt },
          ] },
        ],
        extra: `
          <section class="dm-detail-section dm-detail-skills">
            <h4>Skills</h4>
            <div class="dm-detail-skill-list">
              ${skills.length
                ? skills.map((skill) => `<span>${escapeHtml(skill)}</span>`).join("")
                : "<span>-</span>"}
            </div>
          </section>
        `,
        modalAttr: "data-nearby-detail-modal",
        closeAttr: "data-nearby-detail-close",
      });
    }

    function bindNearbyTableEvents(target) {
      target.querySelectorAll("[data-nearby-node]").forEach((rowEl) => {
        const open = () => {
          nearbyDetailId = rowEl.dataset.nearbyNode || "";
          renderNearbyPage();
        };
        rowEl.addEventListener("click", open);
        rowEl.addEventListener("keydown", (event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            open();
          }
        });
      });
      const closeDetail = () => {
        nearbyDetailId = "";
        renderNearbyPage();
      };
      target.querySelector("[data-nearby-detail-close]")?.addEventListener("click", closeDetail);
      target.querySelector("[data-nearby-detail-modal]")?.addEventListener("click", (event) => {
        if (event.target === event.currentTarget) closeDetail();
      });
    }

    function renderNearbyPage() {
      const rows = nearbyAllRows;
      const counts = nearbyStatusCounts(rows);
      document.querySelectorAll("[data-nearby-count]").forEach((el) => {
        el.textContent = String(counts[el.dataset.nearbyCount] ?? 0);
      });
      document.querySelectorAll("[data-nearby-status]").forEach((button) => {
        const active = (button.dataset.nearbyStatus || "all") === nearbyStatusFilter;
        button.classList.toggle("active", active);
        button.setAttribute("aria-selected", active ? "true" : "false");
      });

      const filtered = nearbyFilteredRows(rows);
      const countLabel = qs("nearby-count");
      if (countLabel) {
        const narrowed = nearbySearchQuery.trim() || nearbyStatusFilter !== "all";
        countLabel.textContent = narrowed
          ? `${filtered.length} shown / ${rows.length} total`
          : `${rows.length} total`;
      }

      if (nearbyDetailId && !rows.some((row) => (row.node_id || "") === nearbyDetailId)) {
        nearbyDetailId = "";
      }
      const detailRow = nearbyDetailId
        ? rows.find((row) => (row.node_id || "") === nearbyDetailId)
        : null;

      const target = qs("nearby-table");
      if (!target) return;
      const emptyText = rows.length ? "No agents match this filter." : "No nearby agents.";
      const tableHtml = filtered.length
        ? `<table class="nearby-table">
            <thead><tr><th>Status</th><th>Agent</th><th>Kind</th><th>Last seen</th><th>Source</th></tr></thead>
            <tbody>${nearbyTableRowsHtml(filtered)}</tbody>
          </table>`
        : empty(emptyText);
      target.innerHTML = tableHtml + (detailRow ? nearbyDetailCardHtml(detailRow) : "");
      bindNearbyTableEvents(target);
    }

    function renderNearby(payload) {
      nearbyAllRows = buildNearbyRows(payload);

      const overviewNearby = qs("overview-nearby");
      overviewNearby.hidden = nearbyAllRows.length === 0;
      if (nearbyAllRows.length) {
        renderNearbyList("overview-nearby-count", "overview-nearby-list", nearbyAllRows, "No nearby agents.");
      }

      renderNearbyPage();
    }

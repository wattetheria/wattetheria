    function nearbyStatus(row) {
      const status = String(row.status || row.relationship_state || row.relationship_kind || "").toLowerCase();
      if (status === "blocked") return "blocked";
      if (row.pending_inbound || row.pending_outbound || status === "requested" || status === "pending") return "pending";
      if (row.connected === true) return "online";
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
      if (row.kind === "node") return 50;
      if (row.kind === "friend") return 60;
      return 70;
    }

    function nodeRelationshipState(node) {
      return node.relationship_state
        || at(node, ["relationship", "relationship_state"])
        || at(node, ["relationship", "last_action"]);
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
        const sourceKind = node.source_kind || at(node, ["discovery", "source_kind"]);
        const endpoint = node.endpoint || at(node, ["metadata", "endpoint_id"]) || at(node, ["discovery", "endpoint_id"]);
        const connectionLabel = connected ? "connected" : "not connected";
        const sourceLabel = sourceKind ? `last source: ${sourceKind}` : "";
        const metaLines = sourceLabel ? [connectionLabel, sourceLabel] : [connectionLabel];
        rows.push({
          key,
          kind: "node",
          name: node.display_name || node.name || nodeId,
          status: node.status || relationshipState || (connected ? "online" : "discovered"),
          connected,
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

    function renderNearby(payload) {
      const rows = buildNearbyRows(payload);
      renderNearbyList("nearby-count", "nearby-list", rows, "No nearby agents.");

      const overviewNearby = qs("overview-nearby");
      overviewNearby.hidden = rows.length === 0;
      if (rows.length) {
        renderNearbyList("overview-nearby-count", "overview-nearby-list", rows, "No nearby agents.");
      }
    }

    function renderRpcLogs(payload) {
      renderList("rpc-list", safeArray(payload.rpc_logs), "No recent events recorded.", (row) => `
        <div class="row">
          <div class="row-head">
            <div class="row-title">${escapeHtml(row.message || "event")}</div>
            ${pill(row.level || "info", row.level)}
          </div>
          <div class="row-body">${escapeHtml(formatTime(row.timestamp))}</div>
        </div>
      `);
    }

    function diagnosticIsError(row) {
      const text = `${row.level || ""} ${row.status || ""}`.toLowerCase();
      return text.includes("error") || text.includes("fail") || text.includes("warn");
    }

    function diagnosticIsMcpTool(row) {
      return row.source === "wattetheria"
        && (row.component === "wattetheria.mcp" || row.category === "tool_call");
    }

    function diagnosticIsAgentCallback(row) {
      const phase = String(row.phase || "");
      return row.source === "wattetheria"
        && row.category === "agent_event"
        && (phase.startsWith("callback.") || phase.startsWith("decision."));
    }

    function diagnosticIsEventBus(row) {
      return row.source === "wattetheria"
        && (row.component === "wattetheria.event_bus" || row.category === "agent_action_commit");
    }

    function diagnosticDetails(row) {
      return row && row.details && typeof row.details === "object" && !Array.isArray(row.details)
        ? row.details
        : {};
    }

    function diagnosticNodeId(row) {
      if (!row) return "";
      if (row.object_kind === "node" && row.object_id) return row.object_id;
      if (row.category === "transport" && row.object_id) return row.object_id;
      return row.source_node_id || "";
    }

    function diagnosticTitle(row) {
      const phase = String(row.phase || "");
      if (row.source === "wattswarm" && phase === "connection.established") return "network connection established";
      if (row.source === "wattswarm" && phase === "connection.closed") return "network connection closed";
      if (row.source === "wattswarm" && phase === "handshake.rejected") return "network handshake rejected";
      return row.message || row.phase || "diagnostic";
    }

    const DIAGNOSTIC_TITLE_MAX = 160;

    // Some messages (e.g. decision.failed) embed an entire request/response dump.
    // Keep the row title concise and consistent with sibling rows; full text stays in JSON.
    function conciseDiagnosticTitle(row) {
      const raw = String(diagnosticTitle(row) || "");
      if (raw.length <= DIAGNOSTIC_TITLE_MAX && !raw.includes("\n")) return raw;
      const phase = String(row.phase || "").trim();
      if (phase) {
        const label = "agent event " + phase.replace(/[._]+/g, " ").trim();
        const details = diagnosticDetails(row);
        const eventType = details.event_type || row.event_type || "";
        return eventType ? `${label}: ${eventType}` : label;
      }
      const firstLine = raw.split(/\r?\n/)[0];
      return firstLine.length > DIAGNOSTIC_TITLE_MAX
        ? `${firstLine.slice(0, DIAGNOSTIC_TITLE_MAX - 1)}…`
        : firstLine;
    }

    function diagnosticContextSummary(row) {
      const details = diagnosticDetails(row);
      const items = [];
      const nodeId = diagnosticNodeId(row);
      if (nodeId) items.push(`node ${compactId(nodeId, 28)}`);
      if (details.remote_addr) items.push(`remote ${details.remote_addr}`);
      if (details.remaining_established != null) items.push(`remaining ${details.remaining_established}`);
      if (details.endpoint_url) items.push(`callback ${details.endpoint_url}`);
      if (details.event_type) items.push(`event type ${details.event_type}`);
      if (details.feed_key) items.push(`feed ${details.feed_key}`);
      if (details.events_applied != null) items.push(`events ${details.events_applied}`);
      if (row.scope_hint) items.push(`scope ${row.scope_hint}`);
      return items.join(" | ");
    }

    function filteredDiagnosticEntries(entries) {
      const explicitSource = qs("diagnostic-source").value.trim();
      return safeArray(entries).filter((row) => {
        if (explicitSource && row.source !== explicitSource) return false;
        if (activeLogMode === "wattetheria" && row.source !== "wattetheria") return false;
        if (activeLogMode === "mcp" && !diagnosticIsMcpTool(row)) return false;
        if (activeLogMode === "callbacks" && !diagnosticIsAgentCallback(row)) return false;
        if (activeLogMode === "eventbus" && !diagnosticIsEventBus(row)) return false;
        if (activeLogMode === "wattswarm" && (row.source !== "wattswarm" || diagnosticIsError(row))) return false;
        if (activeLogMode === "errors" && !diagnosticIsError(row)) return false;
        return true;
      });
    }

    function renderDiagnostics(payload, entries) {
      const local = payload?.local || {};
      const swarm = payload?.swarm || {};
      const snapshot = swarm && swarm.snapshot ? swarm.snapshot : {};
      const localRows = safeArray(local.entries);
      qs("local-log-count").textContent = valueOrDash(localRows.length);
      qs("mcp-log-count").textContent = valueOrDash(localRows.filter((row) => diagnosticIsMcpTool({ ...row, source: "wattetheria" })).length);
      qs("callback-log-count").textContent = valueOrDash(localRows.filter((row) => diagnosticIsAgentCallback({ ...row, source: "wattetheria" })).length);
      qs("event-bus-log-count").textContent = valueOrDash(localRows.filter((row) => diagnosticIsEventBus({ ...row, source: "wattetheria" })).length);
      qs("local-log-errors").textContent = valueOrDash(localRows.filter(diagnosticIsError).length);
      qs("local-log-last").textContent = localRows.length ? compactId(localRows[0].phase || localRows[0].message || "-", 24) : "-";
      qs("swarm-diag-service").textContent = swarm && swarm.network_service_started ? "running" : "stopped";
      qs("swarm-diag-connected").textContent = valueOrDash(snapshot.connected_node_count || snapshot.known_iroh_contacts || 0);
      qs("swarm-diag-scopes").textContent = valueOrDash(safeArray(snapshot.subscribed_scopes).length);
      const visibleEntries = filteredDiagnosticEntries(entries);
      renderList("diagnostic-list", visibleEntries, "No logs recorded for the current filters.", (row) => {
        const details = diagnosticDetails(row);
        const contextSummary = diagnosticContextSummary(row);
        const meta = [
          row.source_label,
          row.component,
          row.category,
          row.phase,
          row.object_kind && row.object_id ? `${row.object_kind} ${compactId(row.object_id, 24)}` : "",
          row.event_id ? `event ${compactId(row.event_id, 18)}` : "",
          row.source_node_id && row.source_node_id !== row.object_id ? `from ${compactId(row.source_node_id, 18)}` : "",
          details.author_node_id ? `author ${compactId(details.author_node_id, 18)}` : "",
        ].filter(Boolean);
        const timestamp = row.timestamp_ms || row.timestamp || row.generated_at;
        return `
          <div class="row">
            <div class="row-head">
              <div class="row-title">${escapeHtml(conciseDiagnosticTitle(row))}</div>
              ${pill(row.source_label || row.source || "log", row.source)}
              ${pill(row.level || "info", row.status || row.level)}
            </div>
            <div class="row-body">${escapeHtml(formatTime(timestamp))}${contextSummary ? ` | ${escapeHtml(contextSummary)}` : ""}</div>
            <div class="row-meta">${meta.map((item) => `<span>${escapeHtml(item)}</span>`).join("")}</div>
            <details class="row-details">
              <summary>JSON</summary>
              <pre class="code">${escapeHtml(JSON.stringify(row, null, 2))}</pre>
            </details>
          </div>
        `;
      });
    }

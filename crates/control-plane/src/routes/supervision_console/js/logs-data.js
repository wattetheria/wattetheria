    function diagnosticQuery(limitOverride) {
      const params = new URLSearchParams();
      const search = qs("diagnostic-search").value.trim();
      const level = qs("diagnostic-level").value.trim();
      const component = qs("diagnostic-component").value.trim();
      const category = qs("diagnostic-category").value.trim();
      const objectId = qs("diagnostic-object-id").value.trim();
      const sourceNodeId = qs("diagnostic-source-node-id").value.trim();
      const limit = limitOverride || qs("diagnostic-limit").value || limitEl.value || "200";
      params.set("limit", String(limit));
      if (search) params.set("search", search);
      if (level) params.set("level", level);
      if (component) params.set("component", component);
      if (category) params.set("category", category);
      if (objectId) params.set("object_id", objectId);
      if (sourceNodeId) params.set("source_node_id", sourceNodeId);
      return params;
    }

    async function refreshDiagnostics(limitOverride) {
      const query = diagnosticQuery(limitOverride).toString();
      const [localResult, swarmResult] = await Promise.allSettled([
        fetchJson(`/v1/client/diagnostics?${query}`, { auth: true }),
        fetchJson(`/v1/client/wattswarm-diagnostics?${query}`, { auth: true }),
      ]);
      const localPayload = localResult.status === "fulfilled"
        ? localResult.value
        : { generated_at: new Date().toISOString(), entries: [], error: localResult.reason?.message || "local diagnostics unavailable" };
      const swarmPayload = swarmResult.status === "fulfilled"
        ? swarmResult.value
        : { ok: false, generated_at: new Date().toISOString(), network_service_started: false, snapshot: null, diagnostics: [], error: swarmResult.reason?.message || "swarm diagnostics unavailable" };
      lastDiagnosticPayload = { local: localPayload, swarm: swarmPayload };
      lastDiagnosticEntries = mergeDiagnosticEntries(localPayload, swarmPayload);
      renderDiagnostics(lastDiagnosticPayload, lastDiagnosticEntries);
      return lastDiagnosticEntries;
    }

    function mergeDiagnosticEntries(localPayload, swarmPayload) {
      const localRows = safeArray(localPayload.entries).map((row) => ({
        ...row,
        source: "wattetheria",
        source_label: "WATTETHERIA",
        timestamp_sort: Date.parse(row.timestamp || row.generated_at || 0) || 0,
      }));
      const swarmRows = safeArray(swarmPayload.diagnostics).map((row) => ({
        ...row,
        source: "wattswarm",
        source_label: "WATTSWARM",
        timestamp_sort: Number(row.timestamp_ms || 0) || Date.parse(row.timestamp || row.generated_at || 0) || 0,
      }));
      return [...localRows, ...swarmRows].sort((a, b) => b.timestamp_sort - a.timestamp_sort);
    }

    function exportDiagnostics() {
      const rows = lastDiagnosticEntries.length ? lastDiagnosticEntries : [];
      const body = rows.map((row) => JSON.stringify(row)).join("\n") + (rows.length ? "\n" : "");
      const blob = new Blob([body], { type: "application/x-ndjson" });
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `wattetheria-node-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.jsonl`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
    }

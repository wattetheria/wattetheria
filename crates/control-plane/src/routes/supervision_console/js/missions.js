    function missionSearchText(row) {
      return [
        row.status,
        row.title,
        row.id,
        row.domain,
        row.created_by_agent_identity,
        row.created_by_display_name,
        row.publisher_id,
        row.claimer_agent_identity,
        row.claimer_display_name,
        row.claimer_id,
        row.publisher_network_reward_watt,
        row.executor_bounty_watt,
        row.task_bounty_watt,
        row.reward_watt,
      ].map((value) => String(value ?? "")).join(" ").toLowerCase();
    }

    function missionActorLabel(row, displayKeys, idKey) {
      for (const key of displayKeys) {
        const value = String(row[key] ?? "").trim();
        if (value) return value;
      }
      return compactId(row[idKey], 20);
    }

    function missionOrigin(row) {
      return row.task_origin === "claimed" ? "claimed" : "published";
    }

    function missionRowsForActiveTab(rows) {
      return rows.filter((row) => missionOrigin(row) === activeMissionTab);
    }

    function missionStatusPills(row) {
      if (missionOrigin(row) !== "claimed") return pill(row.status, row.status);
      const taskStatus = row.status || "unknown";
      return `${pill("node claimed", row.node_claim_status || "claimed")} ${pill(`task ${taskStatus}`, taskStatus)}`;
    }

    function filteredMissionRows(rows) {
      const query = missionSearchQuery.trim().toLowerCase();
      if (!query) return rows;
      return rows.filter((row) => missionSearchText(row).includes(query));
    }

    function updateMissionTabs(rows) {
      document.querySelectorAll("[data-mission-tab]").forEach((button) => {
        const tab = button.dataset.missionTab || "published";
        const active = tab === activeMissionTab;
        button.classList.toggle("active", active);
        button.setAttribute("aria-selected", active ? "true" : "false");
      });
    }

    function updateMissionControls(totalCount, filteredCount, pageCount) {
      const searchInput = qs("missions-search");
      if (searchInput && searchInput.value !== missionSearchQuery) {
        searchInput.value = missionSearchQuery;
      }
      const missionPage = missionPageByTab[activeMissionTab] || 1;
      qs("missions-prev").disabled = missionPage <= 1;
      qs("missions-next").disabled = missionPage >= pageCount;
      const rangeStart = filteredCount === 0 ? 0 : ((missionPage - 1) * missionPageSize) + 1;
      const rangeEnd = Math.min(filteredCount, missionPage * missionPageSize);
      const countText = missionSearchQuery.trim()
        ? `${rangeStart}-${rangeEnd} / ${filteredCount} matched, ${totalCount} total`
        : `${rangeStart}-${rangeEnd} / ${totalCount}`;
      qs("missions-page-status").textContent = `${countText} | Page ${missionPage} / ${pageCount}`;
    }

    function renderMissions(payload) {
      const rows = safeArray(payload.tasks);
      updateMissionTabs(rows);
      const tabRows = missionRowsForActiveTab(rows);
      const filteredRows = filteredMissionRows(tabRows);
      const pageCount = Math.max(1, Math.ceil(filteredRows.length / missionPageSize));
      missionPageByTab[activeMissionTab] = Math.min(
        Math.max(1, missionPageByTab[activeMissionTab] || 1),
        pageCount,
      );
      const missionPage = missionPageByTab[activeMissionTab];
      const start = (missionPage - 1) * missionPageSize;
      const pageRows = filteredRows.slice(start, start + missionPageSize);
      updateMissionControls(tabRows.length, filteredRows.length, pageCount);
      renderTable("missions-table", [
        { label: "Status", render: (row) => missionStatusPills(row) },
        { label: "Mission", render: (row) => `<strong>${escapeHtml(row.title || row.id)}</strong><div class="subtle">${escapeHtml(row.id || "")}</div>` },
        { label: "Domain", render: (row) => escapeHtml(valueOrDash(row.domain)) },
        { label: "Publisher", render: (row) => escapeHtml(missionActorLabel(row, ["created_by_agent_identity", "created_by_display_name"], "publisher_id")) },
        { label: "Claimer", render: (row) => escapeHtml(missionActorLabel(row, ["claimer_agent_identity", "claimer_display_name"], "claimer_id")) },
        { label: "Network Reward", render: (row) => escapeHtml(signedWatt(row.publisher_network_reward_watt)) },
        { label: "Executor Bounty", render: (row) => escapeHtml(valueOrDash(row.executor_bounty_watt ?? row.task_bounty_watt ?? row.reward_watt)) },
        { label: "Created", render: (row) => escapeHtml(formatTime(row.created_at)) },
      ], pageRows, missionSearchQuery.trim() ? "No missions match this search." : `No ${activeMissionTab} missions recorded.`);
    }

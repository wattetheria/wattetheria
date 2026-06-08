    function missionSearchText(row) {
      return [
        row.status,
        row.title,
        row.id,
        row.domain,
        row.publisher_id,
        row.claimer_id,
        row.publisher_network_reward_watt,
        row.executor_bounty_watt,
        row.task_bounty_watt,
        row.reward_watt,
      ].map((value) => String(value ?? "")).join(" ").toLowerCase();
    }

    function filteredMissionRows(rows) {
      const query = missionSearchQuery.trim().toLowerCase();
      if (!query) return rows;
      return rows.filter((row) => missionSearchText(row).includes(query));
    }

    function updateMissionControls(totalCount, filteredCount, pageCount) {
      const searchInput = qs("missions-search");
      if (searchInput && searchInput.value !== missionSearchQuery) {
        searchInput.value = missionSearchQuery;
      }
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
      const filteredRows = filteredMissionRows(rows);
      const pageCount = Math.max(1, Math.ceil(filteredRows.length / missionPageSize));
      missionPage = Math.min(Math.max(1, missionPage), pageCount);
      const start = (missionPage - 1) * missionPageSize;
      const pageRows = filteredRows.slice(start, start + missionPageSize);
      updateMissionControls(rows.length, filteredRows.length, pageCount);
      renderTable("missions-table", [
        { label: "Status", render: (row) => pill(row.status, row.status) },
        { label: "Mission", render: (row) => `<strong>${escapeHtml(row.title || row.id)}</strong><div class="subtle">${escapeHtml(row.id || "")}</div>` },
        { label: "Domain", render: (row) => escapeHtml(valueOrDash(row.domain)) },
        { label: "Publisher", render: (row) => escapeHtml(compactId(row.publisher_id, 20)) },
        { label: "Claimer", render: (row) => escapeHtml(compactId(row.claimer_id, 20)) },
        { label: "Network Reward", render: (row) => escapeHtml(signedWatt(row.publisher_network_reward_watt)) },
        { label: "Executor Bounty", render: (row) => escapeHtml(valueOrDash(row.executor_bounty_watt ?? row.task_bounty_watt ?? row.reward_watt)) },
        { label: "Created", render: (row) => escapeHtml(formatTime(row.created_at)) },
      ], pageRows, missionSearchQuery.trim() ? "No missions match this search." : "No missions recorded.");
    }

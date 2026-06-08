    function renderOrganizations(payload) {
      renderList("organizations-list", safeArray(payload.organizations), "No guilds recorded.", (row) => `
        <div class="row">
          <div class="row-head">
            <div class="row-title">${escapeHtml(row.name || row.id)}</div>
            ${pill(row.status || "org", row.status)}
          </div>
          <div class="row-body">${escapeHtml(valueOrDash(row.member_count))} members | ${escapeHtml(valueOrDash(row.treasury_watt))} WATT | ${escapeHtml(valueOrDash(row.mission_count))} missions</div>
        </div>
      `);
    }

    function pill(text, className = "") {
      return `<span class="pill ${String(className || "").toLowerCase()}">${escapeHtml(valueOrDash(text))}</span>`;
    }

    function escapeHtml(value) {
      return String(value)
        .replaceAll("&", "&amp;")
        .replaceAll("<", "&lt;")
        .replaceAll(">", "&gt;")
        .replaceAll('"', "&quot;")
        .replaceAll("'", "&#39;");
    }

    function empty(message) {
      return `<div class="empty">${escapeHtml(message)}</div>`;
    }

    function renderKpis(payload) {
      const tasks = safeArray(payload.tasks);
      const relationships = safeArray(payload.friend_relationships);
      const requests = safeArray(payload.pending_friend_requests);
      const dmMessages = safeArray(payload.dm_messages);
      const topics = safeArray(payload.public_topics);
      const topicMessages = safeArray(payload.public_topic_messages);
      const statusCounts = tasks.reduce((counts, task) => {
        const status = task.status || "unknown";
        counts[status] = (counts[status] || 0) + 1;
        return counts;
      }, {});
      const kpis = [
        ["Friends", relationships.filter((item) => item.relationship_state === "friend" || item.relationship_kind === "friend").length],
        ["Friend Requests", requests.length],
        ["DM Messages", dmMessages.length],
        ["Hives", topics.length],
        ["Hive Messages", topicMessages.length],
        ["Published", statusCounts.published || 0],
        ["Claimed", statusCounts.claimed || 0],
        ["Completed", statusCounts.completed || 0],
        ["Settled", statusCounts.settled || 0],
      ];
      qs("kpis").innerHTML = kpis.map(([label, value]) =>
        `<div class="kpi"><strong>${escapeHtml(value)}</strong><span>${escapeHtml(label)}</span></div>`
      ).join("");
    }

    function renderTable(targetId, columns, rows, emptyMessage) {
      const target = qs(targetId);
      if (!rows.length) {
        target.innerHTML = empty(emptyMessage);
        return;
      }
      target.innerHTML = `
        <table>
          <thead><tr>${columns.map((column) => `<th>${escapeHtml(column.label)}</th>`).join("")}</tr></thead>
          <tbody>
            ${rows.map((row) => `
              <tr>${columns.map((column) => `<td>${column.render(row)}</td>`).join("")}</tr>
            `).join("")}
          </tbody>
        </table>
      `;
    }

    function renderList(targetId, rows, emptyMessage, renderRow) {
      const target = qs(targetId);
      if (!rows.length) {
        target.innerHTML = empty(emptyMessage);
        return;
      }
      target.innerHTML = rows.map(renderRow).join("");
    }

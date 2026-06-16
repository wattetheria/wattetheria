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

    // Global agent detail card. One skeleton (modal > card > hero + 2-col grid +
    // optional extra). Every page that shows an agent card builds a model and
    // calls this — never duplicate the markup. Model fields:
    //   title, avatarSeed?, subtitle?, statusLabel?, statusClass?, description?
    //   meta?: string[]              -> hero meta line (empty entries dropped)
    //   sections?: [{title, fields:[{label, value}]}]  -> 2-col grid
    //   extra?: html                 -> full-width blocks after the grid (skills, raw, ...)
    //   cardClass?: string           -> defaults to "dm-agent-detail-card"; pass "" for the base card
    //   modalAttr?, closeAttr?: string  -> per-call hooks, e.g. 'data-dm-detail-modal'
    function renderAgentDetailCard(model) {
      const sectionsHtml = safeArray(model.sections).map((section) => `
            <section class="dm-detail-section">
              <h4>${escapeHtml(section.title)}</h4>
              ${safeArray(section.fields).map((field) => dmDetailField(field.label, field.value)).join("")}
            </section>`).join("");
      const metaHtml = safeArray(model.meta)
        .filter((item) => item != null && item !== "")
        .map((item) => `<span>${escapeHtml(item)}</span>`)
        .join("");
      const statusHtml = model.statusLabel ? pill(model.statusLabel, model.statusClass || model.statusLabel) : "";
      const cardClass = model.cardClass === undefined ? " dm-agent-detail-card" : (model.cardClass ? ` ${model.cardClass}` : "");
      return `
        <div class="dm-detail-modal"${model.modalAttr ? ` ${model.modalAttr}` : ""}>
          <div class="dm-detail-card${cardClass}">
            <div class="dm-detail-hero">
              <div class="dm-detail-avatar">${escapeHtml(dmAgentInitials(model.avatarSeed || model.title))}</div>
              <div class="dm-detail-title-block">
                <div class="dm-detail-title-row">
                  <h3>${escapeHtml(valueOrDash(model.title))}</h3>
                  ${statusHtml}
                </div>
                ${model.subtitle ? `<p>${escapeHtml(model.subtitle)}</p>` : ""}
                ${metaHtml ? `<div class="dm-detail-meta">${metaHtml}</div>` : ""}
                ${model.description ? `<p class="dm-detail-description">${escapeHtml(model.description)}</p>` : ""}
              </div>
              <button type="button" class="secondary dm-detail-close"${model.closeAttr ? ` ${model.closeAttr}` : ""}>Close</button>
            </div>
            <div class="dm-detail-grid">${sectionsHtml}
            </div>
            ${model.extra || ""}
          </div>
        </div>
      `;
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

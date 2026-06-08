    function at(value, path) {
      let current = value;
      for (const key of path) {
        if (current == null || typeof current !== "object") return undefined;
        current = current[key];
      }
      return current;
    }

    function valueOrDash(value) {
      return value == null || value === "" ? "-" : String(value);
    }

    function valueOrZero(value) {
      const number = Number(value);
      return Number.isFinite(number) ? number : 0;
    }

    function signedWatt(value) {
      const number = valueOrZero(value);
      return `${number > 0 ? "+" : ""}${number}`;
    }

    function compactId(value, size = 18) {
      const text = valueOrDash(value);
      if (text.length <= size + 8) return text;
      return `${text.slice(0, size)}...${text.slice(-6)}`;
    }

    function formatTime(value) {
      if (value == null || value === "") return "-";
      if (typeof value === "number") {
        const milliseconds = value > 100000000000 ? value : value * 1000;
        return new Date(milliseconds).toLocaleString();
      }
      const parsed = Date.parse(value);
      if (!Number.isNaN(parsed)) return new Date(parsed).toLocaleString();
      return String(value);
    }

    function safeArray(value) {
      return Array.isArray(value) ? value : [];
    }

    function skillPreview(skills) {
      const labels = safeArray(skills)
        .map((skill) => {
          if (typeof skill === "string") return skill;
          return skill?.name || skill?.id || "";
        })
        .map((skill) => String(skill).trim())
        .filter(Boolean);
      if (!labels.length) return "";
      const visible = labels.slice(0, 3).join(", ");
      return labels.length > 3 ? `${visible} +${labels.length - 3}` : visible;
    }

    function skillLabels(skills) {
      return safeArray(skills)
        .map((skill) => (typeof skill === "string" ? skill : skill?.name || skill?.id || ""))
        .map((skill) => String(skill).trim())
        .filter(Boolean);
    }

    function skillTags(skills, max = 6) {
      const labels = skillLabels(skills);
      if (!labels.length) return "";
      const shown = labels.slice(0, max).map((label) => `<span class="tag">${escapeHtml(label)}</span>`).join("");
      const extra = labels.length > max ? `<span class="tag more">+${labels.length - max}</span>` : "";
      return shown + extra;
    }

    function textFromContent(content) {
      if (content == null) return "";
      if (typeof content === "string") return content;
      return content.text
        || content.message
        || content.summary
        || content.body
        || content.kind
        || JSON.stringify(content);
    }

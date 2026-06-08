    // ===== Custom themed dropdowns =====
    // Each native <select> stays the source of truth (value / change / dynamic
    // <option>s keep working); a themed trigger + popup mirrors it both ways.
    const nativeSelectValue = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value");

    function enhanceAllSelects() {
      document.querySelectorAll("select").forEach(enhanceSelect);
    }

    function enhanceSelect(select) {
      if (!select || select.dataset.enhanced === "1") return;
      select.dataset.enhanced = "1";
      select.classList.add("select-native");

      const shell = document.createElement("div");
      shell.className = "select-shell";
      select.parentNode.insertBefore(shell, select);
      shell.appendChild(select);

      const trigger = document.createElement("button");
      trigger.type = "button";
      trigger.className = "select-trigger";
      trigger.setAttribute("aria-haspopup", "listbox");
      trigger.setAttribute("aria-expanded", "false");
      trigger.innerHTML = '<span class="select-value"></span><span class="select-arrow" aria-hidden="true">▾</span>';

      const popup = document.createElement("div");
      popup.className = "select-popup";
      popup.setAttribute("role", "listbox");
      popup.hidden = true;

      shell.appendChild(trigger);
      shell.appendChild(popup);

      const valueEl = trigger.querySelector(".select-value");
      let focusedIndex = -1;

      function rebuildOptions() {
        popup.innerHTML = "";
        Array.from(select.options).forEach((opt, index) => {
          const item = document.createElement("div");
          item.className = "select-option";
          item.setAttribute("role", "option");
          item.dataset.index = String(index);
          item.textContent = opt.textContent;
          item.addEventListener("mousedown", (event) => {
            event.preventDefault();
            choose(index);
          });
          popup.appendChild(item);
        });
        syncFromSelect();
      }

      function syncFromSelect() {
        const opt = select.options[select.selectedIndex];
        const label = opt ? opt.textContent : "";
        valueEl.textContent = label;
        valueEl.classList.toggle("placeholder", !opt || opt.value === "");
        Array.from(popup.children).forEach((item) => {
          const isSel = Number(item.dataset.index) === select.selectedIndex;
          item.classList.toggle("selected", isSel);
          item.setAttribute("aria-selected", isSel ? "true" : "false");
        });
      }

      function choose(index) {
        if (index !== select.selectedIndex) {
          nativeSelectValue.set.call(select, select.options[index] ? select.options[index].value : "");
          syncFromSelect();
          select.dispatchEvent(new Event("change", { bubbles: true }));
        }
        close();
        trigger.focus();
      }

      function setFocused(index) {
        const items = popup.children;
        if (!items.length) return;
        focusedIndex = Math.max(0, Math.min(index, items.length - 1));
        Array.from(items).forEach((item, i) => item.classList.toggle("focused", i === focusedIndex));
        items[focusedIndex].scrollIntoView({ block: "nearest" });
      }

      function open() {
        if (!shell.classList.contains("open")) {
          document.querySelectorAll(".select-shell.open").forEach((other) => {
            if (other !== shell) other.dispatchEvent(new CustomEvent("select-close"));
          });
        }
        shell.classList.add("open");
        popup.hidden = false;
        trigger.setAttribute("aria-expanded", "true");
        popup.classList.toggle("up", trigger.getBoundingClientRect().bottom + 260 > window.innerHeight);
        setFocused(select.selectedIndex < 0 ? 0 : select.selectedIndex);
      }

      function close() {
        shell.classList.remove("open");
        popup.hidden = true;
        popup.classList.remove("up");
        trigger.setAttribute("aria-expanded", "false");
        Array.from(popup.children).forEach((item) => item.classList.remove("focused"));
      }

      function toggle() {
        if (shell.classList.contains("open")) close();
        else open();
      }

      shell.addEventListener("select-close", close);
      trigger.addEventListener("click", toggle);
      trigger.addEventListener("keydown", (event) => {
        const open_ = shell.classList.contains("open");
        if (!open_ && (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ")) {
          event.preventDefault();
          open();
          return;
        }
        if (!open_) return;
        if (event.key === "ArrowDown") { event.preventDefault(); setFocused(focusedIndex + 1); }
        else if (event.key === "ArrowUp") { event.preventDefault(); setFocused(focusedIndex - 1); }
        else if (event.key === "Enter" || event.key === " ") { event.preventDefault(); if (focusedIndex >= 0) choose(focusedIndex); }
        else if (event.key === "Escape") { event.preventDefault(); close(); }
        else if (event.key === "Tab") { close(); }
      });

      // Reflect programmatic value changes (e.g. form reset / dynamic population).
      Object.defineProperty(select, "value", {
        configurable: true,
        get() { return nativeSelectValue.get.call(this); },
        set(v) { nativeSelectValue.set.call(this, v); syncFromSelect(); },
      });
      select.addEventListener("change", syncFromSelect);
      new MutationObserver(rebuildOptions).observe(select, { childList: true });

      rebuildOptions();
    }

    document.addEventListener("click", (event) => {
      document.querySelectorAll(".select-shell.open").forEach((shell) => {
        if (!shell.contains(event.target)) shell.dispatchEvent(new CustomEvent("select-close"));
      });
    });

    // Enhance selects added later (e.g. the wallet network dropdown rendered via innerHTML).
    function observeDynamicSelects() {
      new MutationObserver((mutations) => {
        mutations.forEach((mutation) => {
          mutation.addedNodes.forEach((node) => {
            if (node.nodeType !== 1) return;
            if (node.matches && node.matches("select:not([data-enhanced])")) enhanceSelect(node);
            if (node.querySelectorAll) node.querySelectorAll("select:not([data-enhanced])").forEach(enhanceSelect);
          });
        });
      }).observe(document.body, { childList: true, subtree: true });
    }

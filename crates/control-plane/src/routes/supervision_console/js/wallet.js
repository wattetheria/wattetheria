    function walletSummaryRows(rows) {
      return rows.map(([label, value]) => `
        <div class="row">
          <div class="row-head"><div class="row-title">${escapeHtml(label)}</div></div>
          <div class="row-body">${escapeHtml(valueOrDash(value))}</div>
        </div>
      `).join("");
    }

    function chainNetwork(chainId) {
      return chainLabels[String(chainId || "").toLowerCase()] || "";
    }

    function networkChainId(network) {
      const targetNetwork = String(network || "").toLowerCase();
      return Object.entries(chainLabels).find(([, label]) => label === targetNetwork)?.[0] || "";
    }

    function renderWalletNetworkOptions(activeNetwork) {
      const selectedNetwork = String(activeNetwork || "base").toLowerCase();
      const knownNetworks = new Set(walletNetworkOptions.map((option) => option.value));
      const options = knownNetworks.has(selectedNetwork)
        ? walletNetworkOptions
        : [{ value: selectedNetwork, label: selectedNetwork }, ...walletNetworkOptions];
      return options.map((option) => {
        const selected = option.value === selectedNetwork ? " selected" : "";
        return `<option value="${escapeHtml(option.value)}"${selected}>${escapeHtml(option.label)}</option>`;
      }).join("");
    }

    function walletPaymentAccounts(operator = currentWalletOperator) {
      const accounts = safeArray(operator?.payment_accounts);
      const active = operator?.active_payment_account;
      if (!active?.account_id || accounts.some((account) => account.account_id === active.account_id)) {
        return accounts;
      }
      return [active, ...accounts];
    }

    function walletPaymentAccountFor(network, rail, operator = currentWalletOperator) {
      const targetNetwork = String(network || "").toLowerCase();
      const targetRail = String(rail || "x402").toLowerCase();
      return walletPaymentAccounts(operator).find((account) => (
        String(account?.network || "").toLowerCase() === targetNetwork
        && String(account?.rail || "x402").toLowerCase() === targetRail
        && account?.can_sign
      )) || null;
    }

    function walletActiveIdentity(operator = currentWalletOperator) {
      const identities = safeArray(operator?.wallet_identities);
      return identities.find((identity) => identity?.active)
        || identities.find((identity) => String(identity?.status || "").toLowerCase() === "active")
        || null;
    }

    function walletTagList(values, emptyLabel = "-") {
      const labels = safeArray(values)
        .map((value) => String(value || "").trim())
        .filter(Boolean);
      if (!labels.length) return `<span class="subtle">${escapeHtml(emptyLabel)}</span>`;
      return `<div class="wallet-tags">${labels.map((label) => `<span>${escapeHtml(label)}</span>`).join("")}</div>`;
    }

    function walletTrustItem(ok, label, sub) {
      return `
        <div class="wallet-trust-item ${ok ? "ready" : "pending"}">
          <span class="wallet-trust-mark" aria-hidden="true">${ok ? "&#10003;" : "!"}</span>
          <span class="wallet-trust-text">
            <strong>${escapeHtml(label)}</strong>
            <em>${escapeHtml(sub)}</em>
          </span>
        </div>
      `;
    }

    function renderWalletIdentity(operator) {
      const identity = walletActiveIdentity(operator);
      const identities = safeArray(operator?.wallet_identities);
      return `
        <section class="wallet-section identity">
          <div class="wallet-section-head">
            <div class="wallet-section-title">Wallet Identity</div>
            ${pill(identity ? "DID backed" : "missing", identity ? "ready" : "pending")}
          </div>
          ${walletSummaryRows([
            ["Active Identity", identity?.identity_id || "none"],
            ["DID", identity?.did || operator.agent_did || operator.wallet_bound_agent_did],
            ["Status", identity?.status || (identity ? "active" : "none")],
            ["Created", formatTime(identity?.created_at_ms)],
          ])}
          <div class="wallet-subsection">
            <div class="wallet-subsection-title">Purposes</div>
            ${walletTagList(identity?.purposes, "No purposes recorded")}
          </div>
          <div class="wallet-subtle-line">${escapeHtml(identities.length ? `${identities.length} local identity record${identities.length === 1 ? "" : "s"}` : "No local wallet identities loaded.")}</div>
        </section>
      `;
    }

    function renderWalletPaymentAccounts(operator) {
      const accounts = walletPaymentAccounts(operator);
      if (!accounts.length) {
        return `<div class="empty">No payment accounts recorded.</div>`;
      }
      const activeAccountId = operator.active_payment_account?.account_id || "";
      return `
        <div class="table-wrap wallet-account-table">
          <table>
            <thead>
              <tr>
                <th>Account</th>
                <th>Address</th>
                <th>Rail</th>
                <th>Network</th>
                <th>Custody</th>
                <th>Authority</th>
                <th>Capabilities</th>
              </tr>
            </thead>
            <tbody>
              ${accounts.map((account) => `
                <tr>
                  <td>
                    <strong>${escapeHtml(compactId(account.account_id, 18))}</strong>
                    ${account.account_id === activeAccountId ? pill("active", "ready") : ""}
                  </td>
                  <td class="wallet-address-cell">${escapeHtml(valueOrDash(account.address || "none"))}</td>
                  <td>${escapeHtml(valueOrDash(account.rail))}</td>
                  <td>${escapeHtml(valueOrDash(account.network))}</td>
                  <td>${escapeHtml(valueOrDash(account.custody))}</td>
                  <td>${escapeHtml(account.can_sign ? "can sign" : account.receive_only ? "receive only" : "no signing")}</td>
                  <td>${walletTagList(account.capabilities, "none")}</td>
                </tr>
              `).join("")}
            </tbody>
          </table>
        </div>
      `;
    }

    function walletBindingStatus(operator, selectedPayment) {
      const binding = operator.payment_account_binding || {};
      const proofAvailable = Boolean(binding.proof_available || selectedPayment?.can_sign);
      const status = binding.status || (proofAvailable ? "ready" : selectedPayment ? "watch_only" : "missing_payment_account");
      return {
        binding,
        proofAvailable,
        status,
        pillClass: proofAvailable ? "ready" : selectedPayment ? "pending" : "blocked",
        pillText: proofAvailable ? "proof ready" : selectedPayment ? "watch only" : "missing",
      };
    }

    function renderWalletBinding(operator, selectedPayment) {
      const { binding, proofAvailable, status, pillClass, pillText } = walletBindingStatus(operator, selectedPayment);
      const agentDid = binding.agent_did || operator.agent_did || operator.wallet_bound_agent_did;
      const paymentAddress = binding.payment_address || selectedPayment?.address;
      return `
        <section class="wallet-section binding">
          <div class="wallet-section-head">
            <div class="wallet-section-title">DID Payment Binding</div>
            ${pill(pillText, pillClass)}
          </div>
          <div class="wallet-binding-chain">
            <span>Agent DID</span>
            <strong>${escapeHtml(compactId(agentDid, 30))}</strong>
            <span>Wallet identity key</span>
            <strong>${escapeHtml(proofAvailable ? "local Ed25519 signer" : "not proof-ready")}</strong>
            <span>Payment account</span>
            <strong>${escapeHtml(compactId(paymentAddress || "none", 30))}</strong>
          </div>
          ${walletSummaryRows([
            ["Binding Status", status],
            ["Custody", binding.custody || selectedPayment?.custody],
            ["Agent Proof", binding.agent_proof_algorithm || (agentDid ? "ed25519-binding" : "none")],
            ["Payment Proof", binding.payment_proof_algorithm || (proofAvailable ? "secp256k1-binding" : "none")],
            ["Receive Only", binding.receive_only ? "yes" : "no"],
          ])}
          <div class="wallet-subsection">
            <div class="wallet-subsection-title">Binding Capabilities</div>
            ${walletTagList(binding.capabilities || selectedPayment?.capabilities, "none")}
          </div>
        </section>
      `;
    }

    function stablecoinTokensFor(networkRef) {
      const key = String(networkRef || "").toLowerCase();
      if (stablecoinContracts[key]) return stablecoinContracts[key];
      const chainId = Object.entries(chainLabels).find(([, label]) => label === key)?.[0];
      return chainId ? stablecoinContracts[chainId] || [] : [];
    }

    function balanceOfData(address) {
      return `0x70a08231${String(address || "").replace(/^0x/i, "").padStart(64, "0")}`;
    }

    async function rpcCall(rpcUrl, method, params) {
      const response = await fetch(rpcUrl, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
      });
      const payload = await response.json();
      if (!response.ok || payload.error) {
        throw new Error(payload.error?.message || `RPC request failed with ${response.status}`);
      }
      return payload.result;
    }

    function formatTokenAmount(hexValue, decimals) {
      const raw = BigInt(hexValue || "0x0");
      const scale = 10n ** BigInt(decimals);
      const whole = raw / scale;
      const fraction = raw % scale;
      const fractionText = fraction.toString().padStart(decimals, "0").slice(0, 4).replace(/0+$/, "");
      return fractionText ? `${whole}.${fractionText}` : whole.toString();
    }

    function renderTokenBalances(networkRef, balances = {}) {
      const tokens = stablecoinTokensFor(networkRef);
      const list = qs("web3-token-balances");
      if (!tokens.length) {
        list.innerHTML = `<div class="empty">No stablecoin contracts configured for this network.</div>`;
        return;
      }
      list.innerHTML = tokens.map((token) => `
        <div class="token-balance">
          <span>${escapeHtml(token.symbol)}</span>
          <strong>${escapeHtml(balances[token.symbol] || "-")}</strong>
          <span>${escapeHtml(chainNetwork(networkRef) || networkRef)}</span>
        </div>
      `).join("");
    }

    async function refreshStablecoinBalances() {
      const selectedNetwork = qs("web3-wallet-network")?.value.trim() || selectedWalletNetwork || "base";
      const selectedRail = qs("web3-wallet-rail")?.value.trim() || "x402";
      const selectedChainId = networkChainId(selectedNetwork);
      const rpcUrl = stablecoinRpcUrls[selectedChainId];
      const payment = walletPaymentAccountFor(selectedNetwork, selectedRail);
      const address = payment?.address || "";
      if (!address) {
        setStatus("Create an agent wallet first.", true);
        return;
      }
      if (!rpcUrl) {
        setStatus(`Balance refresh RPC is not configured for ${selectedNetwork}.`, true);
        renderTokenBalances(selectedNetwork);
        return;
      }
      const tokens = stablecoinTokensFor(selectedNetwork);
      renderTokenBalances(selectedNetwork);
      const balances = {};
      await Promise.all(tokens.map(async (token) => {
        const result = await rpcCall(
          rpcUrl,
          "eth_call",
          [{
            to: token.address,
            data: balanceOfData(address),
          }, "latest"],
        );
        balances[token.symbol] = formatTokenAmount(result, token.decimals);
      }));
      renderTokenBalances(selectedNetwork, balances);
    }

    async function createAgentWallet() {
      const address = qs("web3-wallet-address").value.trim();
      const chainId = connectedWeb3Wallet?.chainId || "";
      const network = qs("web3-wallet-network").value.trim() || chainNetwork(chainId);
      const rail = qs("web3-wallet-rail").value.trim() || "x402";
      selectedWalletNetwork = network || "base";
      const data = await fetchJson("/v1/wallet/payment-account/create", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          network,
          rail,
          label: "agent-wallet",
        }),
        auth: true,
      });
      const activeAddress = data.active_payment_account?.address || address;
      selectedWalletNetwork = data.active_payment_account?.network || selectedWalletNetwork;
      setStatus(data.already_exists
        ? `Agent wallet already exists ${compactId(activeAddress, 28)}.`
        : `Created agent wallet ${compactId(activeAddress, 28)}.`);
      await refreshConsole();
    }

    function bindWalletControls() {
      qs("create-agent-wallet")?.addEventListener("click", () => {
        createAgentWallet().catch((error) => setStatus(error.message, true));
      });
      qs("refresh-web3-balances")?.addEventListener("click", () => {
        refreshStablecoinBalances().catch((error) => setStatus(error.message, true));
      });
      qs("web3-wallet-network")?.addEventListener("change", (event) => {
        selectedWalletNetwork = event.target.value || "base";
        renderWallet(currentWalletOperator || {});
      });
    }

    function renderWallet(operator) {
      currentWalletOperator = operator;
      const chainId = connectedWeb3Wallet?.chainId || "";
      const activeNetwork = selectedWalletNetwork || chainNetwork(chainId) || "base";
      const fallbackRail = operator.active_payment_account?.rail || "x402";
      const selectedPayment = walletPaymentAccountFor(activeNetwork, fallbackRail, operator);
      const activeAddress = selectedPayment?.address || "";
      const hasSigningAccount = Boolean(selectedPayment?.can_sign);
      const walletIdentity = walletActiveIdentity(operator);
      const railValue = selectedPayment?.rail || fallbackRail;
      const publicAlias = operator.id ? `@${operator.id}` : "";
      const headerHandle = publicAlias || valueOrDash(operator.agent_did || operator.wallet_bound_agent_did);
      qs("wallet-list").innerHTML = `
        <div class="wallet-cred-head">
          <div class="wallet-cred-top">
            <span class="wallet-cred-eyebrow">WATT wallet · ${escapeHtml(activeNetwork)}</span>
            <button id="refresh-web3-balances" type="button" class="wallet-cred-btn">Refresh balances</button>
          </div>
          <div class="wallet-cred-balances">
            <div class="wallet-cred-metric">
              <div class="wallet-cred-metric-value">${escapeHtml(valueOrDash(operator.watt_balance))}<span>WATT</span></div>
              <div class="wallet-cred-metric-label">internal ledger</div>
            </div>
            <div class="wallet-cred-divider"></div>
            <div id="web3-token-balances" class="wallet-onchain"></div>
          </div>
          <div class="wallet-cred-id">
            <span class="wallet-cred-handle">${escapeHtml(headerHandle)}</span>
            ${publicAlias ? `<button type="button" class="wallet-cred-copy" onclick="copyIdentityId('${escapeHtml(publicAlias)}', this)">Copy</button>` : ""}
          </div>
          <div class="wallet-cred-trust">
            ${walletTrustItem(true, "Local ledger", `${valueOrDash(operator.watt_balance)} WATT`)}
            ${walletTrustItem(Boolean(walletIdentity), "DID backed", walletIdentity ? "active identity" : "no identity")}
            ${walletTrustItem(Boolean(selectedPayment), "Payment bound", selectedPayment ? `${activeNetwork} · ${railValue}` : "unbound")}
          </div>
        </div>
        <section class="wallet-section">
          <div class="wallet-section-head">
            <div class="wallet-section-title">WATT Internal Ledger</div>
            ${pill("local", "ready")}
          </div>
          ${walletSummaryRows([
            ["WATT", operator.watt_balance],
            ["Reward Policy", operator.reward_policy_version],
            ["Wallet Agent DID", operator.agent_did || operator.wallet_bound_agent_did],
            ["Controller", operator.controller_id],
          ])}
        </section>
        ${renderWalletIdentity(operator)}
        <section class="wallet-section web3">
          <div class="wallet-section-head">
            <div class="wallet-section-title">Agent Payment Account</div>
            ${pill(selectedPayment ? "bound" : "unbound", selectedPayment ? "ready" : "pending")}
          </div>
          ${walletSummaryRows([
            ["Payment Account", selectedPayment ? selectedPayment.account_id : "none"],
            ["Address", activeAddress || "none"],
            ["Rail", selectedPayment?.rail || fallbackRail],
            ["Network", activeNetwork || "none"],
            ["Custody", selectedPayment?.custody || "none"],
            ["Can Sign", selectedPayment?.can_sign ? "yes" : "no"],
          ])}
          <div class="wallet-fields">
            <label>
              Address
              <input id="web3-wallet-address" value="${escapeHtml(activeAddress)}" readonly>
            </label>
            <label>
              Network
              <select id="web3-wallet-network">
                ${renderWalletNetworkOptions(activeNetwork)}
              </select>
            </label>
            <label>
              Rail
              <input id="web3-wallet-rail" value="${escapeHtml(selectedPayment?.rail || fallbackRail)}">
            </label>
          </div>
          <div class="wallet-actions">
            <button id="create-agent-wallet" type="button" ${hasSigningAccount ? "disabled" : ""}>Create Agent Wallet</button>
          </div>
          <div id="web3-wallet-status" class="subtle">${escapeHtml(activeAddress ? compactId(activeAddress, 28) : "No agent payment account created.")}</div>
        </section>
        <section class="wallet-section accounts">
          <div class="wallet-section-head">
            <div class="wallet-section-title">Payment Accounts</div>
            ${pill(`${walletPaymentAccounts(operator).length} accounts`, walletPaymentAccounts(operator).length ? "ready" : "pending")}
          </div>
          ${renderWalletPaymentAccounts(operator)}
        </section>
        ${renderWalletBinding(operator, selectedPayment)}
        <section class="wallet-section web2">
          <div class="wallet-section-head">
            <div class="wallet-section-title">Web2 Payments</div>
            ${pill("reserved", "pending")}
          </div>
          ${walletSummaryRows([
            ["Payment Account", "not implemented"],
            ["Payment Kind", "web2 reserved"],
          ])}
        </section>
      `;
      renderTokenBalances(activeNetwork);
      bindWalletControls();
    }

    const storageKey = "wattetheria-node-console";
    const bootstrapControlToken = __BOOTSTRAP_CONTROL_TOKEN__;

    const statusEl = document.getElementById("status");
    const tokenEl = document.getElementById("token");
    const publicIdEl = document.getElementById("public-id");
    const limitEl = document.getElementById("limit");
    const lastRefreshEl = document.getElementById("last-refresh");
    const identitiesByPublicId = new Map();
    let connectedWeb3Wallet = null;
    let currentWalletOperator = null;
    let selectedWalletNetwork = "base";
    let lastConsolePayload = null;
    let lastDiagnosticEntries = [];
    let lastDiagnosticPayload = null;
    let activeLogMode = "all";
    let activeDmThreadKey = "";
    let dmDetailOpen = false;
    let friendRequestDetailId = "";
    let activeHiveKey = "";
    let hiveMessageLoadingKey = "";
    const hiveMessageCache = new Map();
    const hiveMessageErrors = new Map();
    let missionSearchQuery = "";
    let activeMissionTab = "published";
    let nearbyStatusFilter = "all";
    let nearbySearchQuery = "";
    let nearbyDetailId = "";
    let nearbyAllRows = [];
    const missionPageByTab = { published: 1, claim_submitted: 1, claimed: 1 };
    const missionPageSize = 10;
    let servicenetTemplate = null;
    let servicenetAgents = [];
    let selectedIdentityRecord = null;
    let identityDisplayEditing = false;

    const stablecoinContracts = {
      "0x1": [
        { symbol: "USDT", address: "0xdAC17F958D2ee523a2206206994597C13D831ec7", decimals: 6 },
        { symbol: "USDC", address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", decimals: 6 },
        { symbol: "DAI", address: "0x6B175474E89094C44Da98b954EedeAC495271d0F", decimals: 18 },
      ],
      "0x89": [
        { symbol: "USDC", address: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359", decimals: 6 },
      ],
      "0x2105": [
        { symbol: "USDT", address: "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2", decimals: 6 },
        { symbol: "USDC", address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", decimals: 6 },
      ],
      "0x14a34": [
        { symbol: "USDC", address: "0x036CbD53842c5426634e7929541eC2318f3dCF7e", decimals: 6 },
      ],
    };

    const chainLabels = {
      "0x1": "ethereum",
      "0x89": "polygon",
      "0xa": "optimism",
      "0xa4b1": "arbitrum-one",
      "0x2105": "base",
      "0x14a34": "base-sepolia",
    };

    const stablecoinRpcUrls = {
      "0x2105": "https://mainnet.base.org",
      "0x14a34": "https://sepolia.base.org",
    };

    const walletNetworkOptions = [
      { value: "base", label: "Base" },
      { value: "base-sepolia", label: "Base Sepolia" },
    ];
